use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use am_agents::PermissionPolicy;
use am_proto::{new_id, now, AgentKind, ApiGatewayConfig, ExecutionBackend, UsageLedgerEntry};
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use serde_json::Value;
use tokio::{net::TcpListener, sync::oneshot};

use crate::policy::{PolicyPreflight, PolicyPreflightInput};
use crate::{AppCore, CoreError};

/// How long a cached preflight decision may serve identical requests. Budget
/// caps can overshoot by at most `TTL x request rate` before a re-evaluation
/// blocks; rule/config edits invalidate instantly via the policy generation.
const PREFLIGHT_CACHE_TTL: Duration = Duration::from_secs(3);
const CONFIG_CACHE_TTL: Duration = Duration::from_secs(30);
const PREFLIGHT_CACHE_SWEEP_LEN: usize = 512;

fn preflight_cache_ttl() -> Duration {
    std::env::var("AM_GATEWAY_PREFLIGHT_TTL_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(PREFLIGHT_CACHE_TTL)
}

/// Identity of a proxied caller for preflight purposes. Requests sharing a key
/// are policy-equivalent: same provider, attribution headers, and model.
#[derive(Clone, PartialEq, Eq, Hash)]
struct PreflightKey {
    provider: &'static str,
    project_id: Option<String>,
    group_id: Option<String>,
    session_id: Option<String>,
    run_id: Option<String>,
    model: Option<String>,
    source_label: String,
}

struct CachedEntry<T> {
    at: Instant,
    generation: u64,
    value: T,
}

#[derive(Default)]
struct GatewayCache {
    configs: RwLock<Option<CachedEntry<Vec<ApiGatewayConfig>>>>,
    preflight: RwLock<HashMap<PreflightKey, CachedEntry<PolicyPreflight>>>,
}

impl GatewayCache {
    fn configs(&self, generation: u64) -> Option<Vec<ApiGatewayConfig>> {
        let cached = self.configs.read().unwrap();
        let entry = cached.as_ref()?;
        (entry.generation == generation && entry.at.elapsed() < CONFIG_CACHE_TTL)
            .then(|| entry.value.clone())
    }

    fn store_configs(&self, generation: u64, configs: Vec<ApiGatewayConfig>) {
        *self.configs.write().unwrap() = Some(CachedEntry {
            at: Instant::now(),
            generation,
            value: configs,
        });
    }

    fn preflight(
        &self,
        key: &PreflightKey,
        generation: u64,
        ttl: Duration,
    ) -> Option<PolicyPreflight> {
        let cached = self.preflight.read().unwrap();
        let entry = cached.get(key)?;
        (entry.generation == generation && entry.at.elapsed() < ttl).then(|| entry.value.clone())
    }

    fn store_preflight(&self, key: PreflightKey, generation: u64, value: PolicyPreflight) {
        let mut cached = self.preflight.write().unwrap();
        if cached.len() >= PREFLIGHT_CACHE_SWEEP_LEN {
            let ttl = preflight_cache_ttl();
            cached.retain(|_, entry| entry.generation == generation && entry.at.elapsed() < ttl);
        }
        cached.insert(
            key,
            CachedEntry {
                at: Instant::now(),
                generation,
                value,
            },
        );
    }
}

#[derive(Debug)]
pub struct ApiGatewayHandle {
    pub addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
}

impl ApiGatewayHandle {
    pub async fn shutdown(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

#[derive(Clone)]
struct GatewayState {
    core: AppCore,
    client: reqwest::Client,
    cache: Arc<GatewayCache>,
}

pub async fn serve_api_gateway(core: AppCore, port: u16) -> Result<ApiGatewayHandle, CoreError> {
    let state = GatewayState {
        core,
        client: reqwest::Client::new(),
        cache: Arc::new(GatewayCache::default()),
    };
    let app = Router::new()
        .route("/{provider}/{*path}", any(proxy))
        .route("/gateway/{provider}/{*path}", any(proxy))
        .with_state(state);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|err| CoreError::Other(err.to_string()))?;
    let addr = listener
        .local_addr()
        .map_err(|err| CoreError::Other(err.to_string()))?;
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
        {
            tracing::warn!(error = %err, "AgentManager API gateway stopped with error");
        }
    });
    Ok(ApiGatewayHandle {
        addr,
        shutdown: Some(tx),
    })
}

async fn proxy(
    State(state): State<GatewayState>,
    Path((provider, path)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match proxy_inner(state, provider, path, method, uri, headers, body).await {
        Ok(response) => response,
        Err((status, message)) => (status, message).into_response(),
    }
}

async fn proxy_inner(
    state: GatewayState,
    provider: String,
    path: String,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    let provider = normalize_provider(&provider).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "unknown gateway provider".to_string(),
        )
    })?;
    let generation = state.core.policy_generation();
    let configs = match state.cache.configs(generation) {
        Some(configs) => configs,
        None => {
            let configs = state
                .core
                .list_api_gateway_configs()
                .await
                .map_err(internal)?;
            state.cache.store_configs(generation, configs.clone());
            configs
        }
    };
    let config = configs
        .into_iter()
        .find(|config| config.provider == provider && config.enabled)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("{provider} gateway is not enabled"),
            )
        })?;

    let request_json = serde_json::from_slice::<Value>(&body).ok();
    let model = request_json
        .as_ref()
        .and_then(|json| json.get("model"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let agent = agent_for_provider(provider);
    let source_label = headers
        .get("x-agentmanager-source")
        .and_then(|value| value.to_str().ok())
        .unwrap_or(config.name.as_str())
        .to_string();
    let project_id = header_string(&headers, "x-agentmanager-project-id");
    let group_id = header_string(&headers, "x-agentmanager-group-id");
    let session_id = header_string(&headers, "x-agentmanager-session-id");
    let run_id = header_string(&headers, "x-agentmanager-run-id");

    let policy = if config.enforce_policies {
        let key = PreflightKey {
            provider,
            project_id: project_id.clone(),
            group_id: group_id.clone(),
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            model: model.clone(),
            source_label: source_label.clone(),
        };
        let ttl = preflight_cache_ttl();
        match state.cache.preflight(&key, generation, ttl) {
            Some(cached) => Some(cached),
            None => {
                let fresh = state
                    .core
                    .policy_preflight(PolicyPreflightInput {
                        project_id: project_id.clone(),
                        group_id: group_id.clone(),
                        repo_ids: Vec::new(),
                        branch: None,
                        task_type: Some("api_gateway".into()),
                        agent,
                        model: model.clone(),
                        runtime: ExecutionBackend::Host,
                        permission: PermissionPolicy::ReadOnly,
                        session_id: session_id.clone(),
                        run_id: run_id.clone(),
                        provider: Some(provider.to_string()),
                        traffic_kind: Some("api_gateway".into()),
                        api_source: Some(source_label.clone()),
                        requested_paths: Vec::new(),
                        requested_tools: Vec::new(),
                        requested_mcp_server_ids: Vec::new(),
                        prompt_bytes: body.len() as u64,
                    })
                    .await
                    .map_err(|err| (StatusCode::FORBIDDEN, err.to_string()))?;
                state.cache.store_preflight(key, generation, fresh.clone());
                Some(fresh)
            }
        }
    } else {
        None
    };

    let mut upstream = config.upstream_base_url.trim_end_matches('/').to_string();
    upstream.push('/');
    upstream.push_str(path.trim_start_matches('/'));
    if let Some(query) = uri.query() {
        upstream.push('?');
        upstream.push_str(query);
    }

    let reqwest_method =
        reqwest::Method::from_bytes(method.as_str().as_bytes()).map_err(internal)?;
    let mut request = state
        .client
        .request(reqwest_method, upstream)
        .body(body.clone());
    for (name, value) in &headers {
        if name == header::HOST || name == header::CONTENT_LENGTH {
            continue;
        }
        request = request.header(name.as_str(), value);
    }
    if let Some(env_var) = &config.auth_env_var {
        if let Ok(secret) = std::env::var(env_var) {
            request = add_auth_header(request, provider, &secret);
        }
    }

    let response = request.send().await.map_err(internal)?;
    let status = response.status();
    let streaming = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"));
    let mut response_builder = Response::builder().status(status);
    for (name, value) in response.headers() {
        if name == header::CONTENT_LENGTH {
            continue;
        }
        response_builder = response_builder.header(name, value);
    }
    if streaming {
        let input_tokens = request_json
            .as_ref()
            .map(estimate_request_tokens)
            .unwrap_or_default();
        let _ = record_gateway_usage(
            &state.core,
            project_id,
            group_id,
            session_id,
            run_id,
            agent,
            provider,
            model,
            source_label,
            policy.as_ref().map(|policy| policy.envelope.id.clone()),
            input_tokens,
            0,
            status.as_u16(),
        )
        .await;
        return response_builder
            .body(Body::from_stream(response.bytes_stream()))
            .map_err(internal);
    }
    let bytes = response.bytes().await.map_err(internal)?;
    let (input_tokens, output_tokens) =
        usage_from_response(provider, request_json.as_ref(), &bytes);
    let _ = record_gateway_usage(
        &state.core,
        project_id,
        group_id,
        session_id,
        run_id,
        agent,
        provider,
        model,
        source_label,
        policy.as_ref().map(|policy| policy.envelope.id.clone()),
        input_tokens,
        output_tokens,
        status.as_u16(),
    )
    .await;
    response_builder.body(Body::from(bytes)).map_err(internal)
}

async fn record_gateway_usage(
    core: &AppCore,
    project_id: Option<String>,
    group_id: Option<String>,
    session_id: Option<String>,
    run_id: Option<String>,
    agent: AgentKind,
    provider: &str,
    model: Option<String>,
    source_label: String,
    policy_envelope_id: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    status_code: u16,
) -> Result<(), CoreError> {
    let entry = UsageLedgerEntry {
        id: new_id(),
        ts: now(),
        org_id: Some("local".into()),
        team_id: None,
        user_id: Some("local-user".into()),
        project_id,
        group_id,
        repo_id: None,
        session_id,
        run_id,
        agent: Some(agent),
        provider: Some(provider.to_string()),
        model,
        traffic_kind: Some("api_gateway".into()),
        api_source: Some(source_label.clone()),
        source_label: Some(source_label),
        input_tokens,
        output_tokens,
        estimated_cost_usd: None,
        policy_envelope_id,
        request_count: 1,
        status_code: Some(status_code),
    };
    am_db::repos::policy::insert_usage(&core.db.pool, &entry).await?;
    Ok(())
}

fn usage_from_response(provider: &str, request: Option<&Value>, bytes: &[u8]) -> (u64, u64) {
    let fallback_input = request.map(estimate_request_tokens).unwrap_or_default();
    if let Ok(json) = serde_json::from_slice::<Value>(bytes) {
        if provider == "anthropic" {
            let usage = json.get("usage").unwrap_or(&Value::Null);
            return (
                usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(fallback_input),
                usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
            );
        }
        let usage = json.get("usage").unwrap_or(&Value::Null);
        return (
            usage
                .get("input_tokens")
                .or_else(|| usage.get("prompt_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(fallback_input),
            usage
                .get("output_tokens")
                .or_else(|| usage.get("completion_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or_default(),
        );
    }
    (fallback_input, 0)
}

fn estimate_request_tokens(json: &Value) -> u64 {
    serde_json::to_string(json)
        .map(|raw| (raw.len() as u64 / 4).max(1))
        .unwrap_or_default()
}

fn normalize_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" | "codex" => Some("openai"),
        "anthropic" | "claude" | "claude_code" => Some("anthropic"),
        _ => None,
    }
}

fn agent_for_provider(provider: &str) -> AgentKind {
    match provider {
        "anthropic" => AgentKind::ClaudeCode,
        _ => AgentKind::Codex,
    }
}

fn add_auth_header(
    request: reqwest::RequestBuilder,
    provider: &str,
    secret: &str,
) -> reqwest::RequestBuilder {
    if provider == "anthropic" {
        request.header("x-api-key", secret)
    } else {
        request.bearer_auth(secret)
    }
}

fn header_string(headers: &HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn internal(err: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn preflight_fixture() -> PolicyPreflight {
        let envelope = serde_json::from_value(json!({
            "id": "env-1",
            "request_id": "req-1",
            "decision_id": "dec-1",
            "created_at": now(),
            "agent": "claude_code",
            "runtime": "host",
            "permission": "read_only",
            "action": "allow",
        }))
        .expect("minimal envelope");
        PolicyPreflight {
            agent: AgentKind::ClaudeCode,
            model: None,
            runtime: ExecutionBackend::Host,
            runtime_policy: Default::default(),
            envelope,
        }
    }

    fn key() -> PreflightKey {
        PreflightKey {
            provider: "anthropic",
            project_id: Some("p1".into()),
            group_id: None,
            session_id: Some("s1".into()),
            run_id: None,
            model: Some("claude-fable-5".into()),
            source_label: "test".into(),
        }
    }

    #[test]
    fn preflight_cache_hits_within_ttl_and_generation() {
        let cache = GatewayCache::default();
        let ttl = Duration::from_secs(3);
        assert!(cache.preflight(&key(), 1, ttl).is_none(), "cold miss");

        cache.store_preflight(key(), 1, preflight_fixture());
        assert!(cache.preflight(&key(), 1, ttl).is_some(), "warm hit");

        // A different caller identity misses.
        let other = PreflightKey {
            session_id: Some("s2".into()),
            ..key()
        };
        assert!(cache.preflight(&other, 1, ttl).is_none());

        // Zero TTL expires immediately.
        assert!(cache.preflight(&key(), 1, Duration::ZERO).is_none());
    }

    #[test]
    fn policy_generation_bump_invalidates_cached_entries() {
        let cache = GatewayCache::default();
        let ttl = Duration::from_secs(60);
        cache.store_preflight(key(), 1, preflight_fixture());
        assert!(cache.preflight(&key(), 1, ttl).is_some());
        assert!(
            cache.preflight(&key(), 2, ttl).is_none(),
            "generation bump must invalidate"
        );

        cache.store_configs(1, Vec::new());
        assert!(cache.configs(1).is_some());
        assert!(cache.configs(2).is_none());
    }

    #[test]
    fn provider_normalization_covers_agent_aliases() {
        assert_eq!(normalize_provider("claude"), Some("anthropic"));
        assert_eq!(normalize_provider("claude_code"), Some("anthropic"));
        assert_eq!(normalize_provider("codex"), Some("openai"));
        assert_eq!(normalize_provider("unknown"), None);
    }
}
