use std::collections::HashSet;
use std::process::Stdio;
use std::time::Duration;

use am_agents::{binary_version, find_binary, AgentKind, LocalModelRuntime};
use am_proto::{
    ExecutionBackend, LocalModelInfo, LocalModelPolicy, LocalModelProviderKind, LocalModelStatus,
    LocalModelTarget, ModelTargetKind, OllamaImportInput,
};
use keyring::Entry;
use reqwest::header::AUTHORIZATION;
use serde_json::Value;
use tokio::process::Command;

use crate::{AppCore, CoreError};

const LOCAL_MODEL_POLICY_KEY: &str = "local_model_policy";
const KEYRING_SERVICE: &str = "com.agentmanager.app";
const KEYRING_LM_STUDIO_TOKEN: &str = "lm-studio-api-token";
const LOCAL_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
const LOCAL_CLI_TIMEOUT: Duration = Duration::from_secs(60 * 30);

impl AppCore {
    pub async fn get_local_model_policy(&self) -> Result<LocalModelPolicy, CoreError> {
        let mut policy = am_db::repos::settings::get(&self.db.pool, LOCAL_MODEL_POLICY_KEY)
            .await?
            .and_then(|raw| serde_json::from_str::<LocalModelPolicy>(&raw).ok())
            .unwrap_or_default();
        policy = normalize_policy(policy);
        policy.lm_studio_api_token_configured = load_lm_studio_token()?.is_some();
        policy.lm_studio_api_token = None;
        Ok(policy)
    }

    pub async fn set_local_model_policy(
        &self,
        policy: LocalModelPolicy,
    ) -> Result<LocalModelPolicy, CoreError> {
        if let Some(token) = policy.lm_studio_api_token.as_deref() {
            let token = token.trim();
            if token.is_empty() {
                delete_lm_studio_token()?;
            } else {
                store_lm_studio_token(token)?;
            }
        } else if !policy.lm_studio_api_token_configured {
            delete_lm_studio_token()?;
        }

        let mut normalized = normalize_policy(policy);
        normalized.lm_studio_api_token_configured = load_lm_studio_token()?.is_some();
        normalized.lm_studio_api_token = None;
        let mut stored = normalized.clone();
        stored.lm_studio_api_token_configured = false;
        let value = serde_json::to_string(&stored).map_err(|e| CoreError::Other(e.to_string()))?;
        am_db::repos::settings::set(&self.db.pool, LOCAL_MODEL_POLICY_KEY, &value).await?;
        Ok(normalized)
    }

    pub async fn detect_local_models(&self) -> Result<Vec<LocalModelStatus>, CoreError> {
        let policy = self.get_local_model_policy().await?;
        let token = load_lm_studio_token()?;
        let http = reqwest::Client::builder()
            .timeout(LOCAL_PROBE_TIMEOUT)
            .build()
            .map_err(http_error)?;

        let (ollama, lm_studio) = tokio::join!(
            detect_ollama(http.clone(), policy.ollama_base_url.clone()),
            detect_lm_studio(http, policy.lm_studio_base_url.clone(), token)
        );
        Ok(vec![ollama, lm_studio])
    }

    pub(crate) async fn best_ready_local_target(
        &self,
        policy: &LocalModelPolicy,
    ) -> Result<Option<LocalModelTarget>, CoreError> {
        if !policy.use_local_fallback || policy.targets.is_empty() {
            return Ok(None);
        }
        let statuses = self.detect_local_models().await?;
        for target in &policy.targets {
            let Some(status) = statuses
                .iter()
                .find(|status| status.provider == target.provider)
            else {
                continue;
            };
            if !status.server_running || !status.authenticated {
                continue;
            }
            if status.models.is_empty() || model_available(&status.models, &target.model) {
                return Ok(Some(target.clone()));
            }
        }
        Ok(None)
    }

    pub async fn pull_ollama_model(&self, model: String) -> Result<(), CoreError> {
        let model = clean_required(&model, "model")?;
        run_local_cli("ollama", &["pull", model.as_str()]).await
    }

    pub async fn import_ollama_model(&self, input: OllamaImportInput) -> Result<(), CoreError> {
        let model = clean_required(&input.model, "model")?;
        let path = clean_required(&input.modelfile_path, "Modelfile path")?;
        run_local_cli("ollama", &["create", model.as_str(), "-f", path.as_str()]).await
    }

    pub async fn lmstudio_get_model(&self, model: String) -> Result<(), CoreError> {
        let model = clean_required(&model, "model")?;
        run_local_cli("lms", &["get", model.as_str()]).await
    }

    pub async fn lmstudio_import_model(&self, path: String) -> Result<(), CoreError> {
        let path = clean_required(&path, "model path")?;
        run_local_cli("lms", &["import", path.as_str()]).await
    }

    pub(crate) fn local_model_runtime(
        &self,
        provider: Option<LocalModelProviderKind>,
        model: Option<String>,
        base_url: Option<String>,
    ) -> Result<Option<LocalModelRuntime>, CoreError> {
        let Some(provider) = provider else {
            return Ok(None);
        };
        let model = model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .ok_or_else(|| CoreError::Other("local model target requires a model".into()))?;
        let api_token = match provider {
            LocalModelProviderKind::LmStudio => load_lm_studio_token()?,
            LocalModelProviderKind::Ollama => None,
        };
        Ok(Some(LocalModelRuntime {
            provider,
            model: model.to_string(),
            base_url: base_url
                .as_deref()
                .map(|url| normalize_base_url(url, provider.default_base_url()))
                .filter(|url| !url.is_empty()),
            api_token,
        }))
    }
}

pub(crate) fn run_target_hash(
    agent: AgentKind,
    model: Option<&str>,
    reasoning: Option<&str>,
    local_provider: Option<LocalModelProviderKind>,
    local_base_url: Option<&str>,
    execution_backend: ExecutionBackend,
    model_target: ModelTargetKind,
    compute_lease_id: Option<&str>,
) -> String {
    let identity = format!(
        "agent={}|model={}|reasoning={}|local_provider={}|local_base_url={}|backend={}|model_target={}|compute_lease_id={}",
        agent.as_str(),
        clean_identity_part(model),
        clean_identity_part(reasoning),
        local_provider
            .map(|provider| provider.as_str())
            .unwrap_or("cloud"),
        clean_identity_part(local_base_url),
        execution_backend.as_str(),
        model_target.as_str(),
        clean_identity_part(compute_lease_id),
    );
    stable_hex_hash(&identity)
}

pub(crate) fn legacy_run_target_hash(
    agent: AgentKind,
    model: Option<&str>,
    reasoning: Option<&str>,
    local_provider: Option<LocalModelProviderKind>,
    local_base_url: Option<&str>,
) -> String {
    let identity = format!(
        "agent={}|model={}|reasoning={}|local_provider={}|local_base_url={}",
        agent.as_str(),
        clean_identity_part(model),
        clean_identity_part(reasoning),
        local_provider
            .map(|provider| provider.as_str())
            .unwrap_or("cloud"),
        clean_identity_part(local_base_url),
    );
    stable_hex_hash(&identity)
}

pub(crate) fn target_hash_matches(
    stored: Option<&str>,
    current: &str,
    legacy_current: &str,
) -> bool {
    stored
        .map(|hash| hash == current || hash == legacy_current)
        .unwrap_or(legacy_current == current)
}

async fn detect_ollama(http: reqwest::Client, base_url: String) -> LocalModelStatus {
    let provider = LocalModelProviderKind::Ollama;
    let binary = tokio::task::spawn_blocking(|| find_binary("ollama"))
        .await
        .ok()
        .flatten();
    let version_from_cli = binary.as_ref().and_then(|path| binary_version(path));
    let cli_path = binary
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());

    let mut status = LocalModelStatus {
        provider,
        label: provider.label().to_string(),
        base_url: base_url.clone(),
        server_running: false,
        cli_installed: binary.is_some(),
        cli_path,
        authenticated: false,
        version: version_from_cli,
        models: Vec::new(),
        error: None,
    };

    match http.get(endpoint(&base_url, "/api/version")).send().await {
        Ok(response) if response.status().is_success() => {
            status.server_running = true;
            status.authenticated = true;
            if let Ok(body) = response.json::<Value>().await {
                if let Some(version) = body.get("version").and_then(Value::as_str) {
                    status.version = Some(version.to_string());
                }
            }
        }
        Ok(response) => {
            status.error = Some(format!("Ollama returned HTTP {}", response.status()));
            return status;
        }
        Err(err) => {
            status.error = Some(local_http_message("Ollama", &err));
            return status;
        }
    }

    match http.get(endpoint(&base_url, "/api/tags")).send().await {
        Ok(response) if response.status().is_success() => match response.json::<Value>().await {
            Ok(body) => status.models = parse_ollama_models(&body),
            Err(err) => status.error = Some(format!("could not parse Ollama model list: {err}")),
        },
        Ok(response) => {
            status.error = Some(format!(
                "Ollama model list returned HTTP {}",
                response.status()
            ));
        }
        Err(err) => {
            status.error = Some(local_http_message("Ollama", &err));
        }
    }

    status
}

async fn detect_lm_studio(
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
) -> LocalModelStatus {
    let provider = LocalModelProviderKind::LmStudio;
    let binary = tokio::task::spawn_blocking(|| find_binary("lms"))
        .await
        .ok()
        .flatten();
    let version_from_cli = binary.as_ref().and_then(|path| binary_version(path));
    let cli_path = binary
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());

    let mut status = LocalModelStatus {
        provider,
        label: provider.label().to_string(),
        base_url: base_url.clone(),
        server_running: false,
        cli_installed: binary.is_some(),
        cli_path,
        authenticated: token.is_none(),
        version: version_from_cli,
        models: Vec::new(),
        error: None,
    };

    let primary =
        request_lm_studio_models(&http, &base_url, "/api/v1/models", token.as_deref()).await;
    let response = match primary {
        Ok(response) if response.status().as_u16() == 404 => {
            request_lm_studio_models(&http, &base_url, "/v1/models", token.as_deref()).await
        }
        other => other,
    };

    match response {
        Ok(response) if response.status().is_success() => {
            status.server_running = true;
            status.authenticated = true;
            match response.json::<Value>().await {
                Ok(body) => status.models = parse_lm_studio_models(&body),
                Err(err) => {
                    status.error = Some(format!("could not parse LM Studio model list: {err}"))
                }
            }
        }
        Ok(response) if response.status().as_u16() == 401 || response.status().as_u16() == 403 => {
            status.server_running = true;
            status.authenticated = false;
            status.error = Some("LM Studio requires an API token".to_string());
        }
        Ok(response) => {
            status.server_running = true;
            status.error = Some(format!("LM Studio returned HTTP {}", response.status()));
        }
        Err(err) => {
            status.error = Some(local_http_message("LM Studio", &err));
        }
    }

    status
}

async fn request_lm_studio_models(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    token: Option<&str>,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut request = http.get(endpoint(base_url, path));
    if let Some(token) = token {
        request = request.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    request.send().await
}

fn parse_ollama_models(body: &Value) -> Vec<LocalModelInfo> {
    body.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model
                .get("model")
                .or_else(|| model.get("name"))
                .and_then(Value::as_str)?
                .trim();
            if id.is_empty() {
                return None;
            }
            let details = model.get("details").unwrap_or(&Value::Null);
            Some(LocalModelInfo {
                id: id.to_string(),
                name: model
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
                family: details
                    .get("family")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                parameter_size: details
                    .get("parameter_size")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                quantization: details
                    .get("quantization_level")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                size: model.get("size").and_then(Value::as_u64),
                loaded: true,
            })
        })
        .collect()
}

fn parse_lm_studio_models(body: &Value) -> Vec<LocalModelInfo> {
    body.get("data")
        .and_then(Value::as_array)
        .or_else(|| body.get("models").and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model
                .get("id")
                .or_else(|| model.get("model_key"))
                .or_else(|| model.get("path"))
                .and_then(Value::as_str)?
                .trim();
            if id.is_empty() {
                return None;
            }
            let loaded = model
                .get("loaded")
                .and_then(Value::as_bool)
                .or_else(|| {
                    model
                        .get("state")
                        .and_then(Value::as_str)
                        .map(|state| matches!(state, "loaded" | "ready" | "running"))
                })
                .unwrap_or(false);
            Some(LocalModelInfo {
                id: id.to_string(),
                name: model
                    .get("name")
                    .or_else(|| model.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
                family: model
                    .get("arch")
                    .or_else(|| model.get("family"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                parameter_size: model
                    .get("params_string")
                    .or_else(|| model.get("parameter_size"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                quantization: model
                    .get("quantization")
                    .or_else(|| model.get("quantization_level"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                size: model.get("size").and_then(Value::as_u64),
                loaded,
            })
        })
        .collect()
}

fn model_available(models: &[LocalModelInfo], requested: &str) -> bool {
    let requested = requested.trim();
    models.iter().any(|model| {
        model.id.eq_ignore_ascii_case(requested) || model.name.eq_ignore_ascii_case(requested)
    })
}

async fn run_local_cli(binary_name: &str, args: &[&str]) -> Result<(), CoreError> {
    let binary_name = binary_name.to_string();
    let binary_for_lookup = binary_name.clone();
    let binary = tokio::task::spawn_blocking(move || find_binary(&binary_for_lookup))
        .await
        .ok()
        .flatten()
        .ok_or_else(|| CoreError::Other(format!("{binary_name} CLI not found")))?;

    let child = Command::new(&binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CoreError::Other(format!("failed to start {}: {e}", binary.display())))?;

    let output = tokio::time::timeout(LOCAL_CLI_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| CoreError::Other(format!("{} timed out", binary.display())))?
        .map_err(|e| CoreError::Other(format!("failed to run {}: {e}", binary.display())))?;

    if output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let diagnostics = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (true, true) => format!("exit status {}", output.status),
        (false, true) => stdout.trim().to_string(),
        (true, false) => stderr.trim().to_string(),
        (false, false) => format!("{}\n{}", stdout.trim(), stderr.trim()),
    };
    Err(CoreError::Other(format!(
        "{} failed: {}",
        binary.display(),
        truncate(&diagnostics, 2000)
    )))
}

fn normalize_policy(mut policy: LocalModelPolicy) -> LocalModelPolicy {
    policy.probe_interval_secs = policy.probe_interval_secs.clamp(5, 3600);
    policy.offline_grace_secs = policy.offline_grace_secs.min(600);
    policy.stable_successes = policy.stable_successes.clamp(1, 10);
    policy.ollama_base_url = normalize_base_url(
        &policy.ollama_base_url,
        LocalModelProviderKind::Ollama.default_base_url(),
    );
    policy.lm_studio_base_url = normalize_base_url(
        &policy.lm_studio_base_url,
        LocalModelProviderKind::LmStudio.default_base_url(),
    );
    policy.lm_studio_api_token = policy
        .lm_studio_api_token
        .take()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty());

    let mut seen = HashSet::<(LocalModelProviderKind, String, Option<String>)>::new();
    policy.targets = policy
        .targets
        .into_iter()
        .filter_map(|target| normalize_target(target))
        .filter(|target| {
            seen.insert((
                target.provider,
                target.model.to_ascii_lowercase(),
                target.base_url.clone(),
            ))
        })
        .collect();

    policy
}

fn normalize_target(target: LocalModelTarget) -> Option<LocalModelTarget> {
    let model = target.model.trim();
    if model.is_empty() {
        return None;
    }
    Some(LocalModelTarget {
        provider: target.provider,
        model: model.to_string(),
        base_url: target
            .base_url
            .as_deref()
            .map(|url| normalize_base_url(url, target.provider.default_base_url()))
            .filter(|url| url != target.provider.default_base_url()),
    })
}

fn normalize_base_url(value: &str, fallback: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn clean_required(value: &str, label: &str) -> Result<String, CoreError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(CoreError::Other(format!("{label} is required")))
    } else {
        Ok(trimmed.to_string())
    }
}

fn clean_identity_part(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("-")
        .to_ascii_lowercase()
}

fn stable_hex_hash(value: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

fn local_http_message(provider: &str, err: &reqwest::Error) -> String {
    if err.is_connect() || err.is_timeout() {
        format!("{provider} is not reachable")
    } else {
        format!("{provider} request failed: {err}")
    }
}

fn http_error(err: reqwest::Error) -> CoreError {
    CoreError::Other(format!("local model request failed: {err}"))
}

fn lm_studio_token_entry() -> Result<Entry, CoreError> {
    Entry::new(KEYRING_SERVICE, KEYRING_LM_STUDIO_TOKEN).map_err(keyring_error)
}

fn load_lm_studio_token() -> Result<Option<String>, CoreError> {
    match lm_studio_token_entry()?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(keyring_error(err)),
    }
}

fn store_lm_studio_token(token: &str) -> Result<(), CoreError> {
    lm_studio_token_entry()?
        .set_password(token)
        .map_err(keyring_error)
}

fn delete_lm_studio_token() -> Result<(), CoreError> {
    match lm_studio_token_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(keyring_error(err)),
    }
}

fn keyring_error(err: keyring::Error) -> CoreError {
    CoreError::Other(format!("LM Studio keychain access failed: {err}"))
}

fn truncate(value: &str, max: usize) -> String {
    let mut out = String::with_capacity(max.min(value.len()));
    for (idx, ch) in value.char_indices() {
        if idx >= max {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_ollama_tags_extracts_details() {
        let body = json!({
            "models": [{
                "name": "qwen3:8b",
                "model": "qwen3:8b",
                "size": 5220000000u64,
                "details": {
                    "family": "qwen3",
                    "parameter_size": "8B",
                    "quantization_level": "Q4_K_M"
                }
            }]
        });

        let models = parse_ollama_models(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "qwen3:8b");
        assert_eq!(models[0].family.as_deref(), Some("qwen3"));
        assert_eq!(models[0].parameter_size.as_deref(), Some("8B"));
        assert_eq!(models[0].quantization.as_deref(), Some("Q4_K_M"));
        assert!(models[0].loaded);
    }

    #[test]
    fn parse_lm_studio_models_accepts_rest_and_openai_shapes() {
        let rest = json!({
            "data": [{
                "id": "lmstudio-community/qwen3-8b",
                "arch": "qwen3",
                "params_string": "8B",
                "quantization": "Q4_K_M",
                "state": "loaded"
            }]
        });
        let openai = json!({
            "object": "list",
            "data": [{ "id": "local-model", "object": "model", "owned_by": "lmstudio" }]
        });

        let rest_models = parse_lm_studio_models(&rest);
        assert_eq!(rest_models[0].id, "lmstudio-community/qwen3-8b");
        assert_eq!(rest_models[0].family.as_deref(), Some("qwen3"));
        assert!(rest_models[0].loaded);

        let openai_models = parse_lm_studio_models(&openai);
        assert_eq!(openai_models[0].id, "local-model");
        assert!(!openai_models[0].loaded);
    }

    #[test]
    fn model_available_matches_id_or_name_case_insensitively() {
        let models = vec![LocalModelInfo {
            id: "qwen3:8b".to_string(),
            name: "Qwen 3 8B".to_string(),
            ..LocalModelInfo::default()
        }];

        assert!(model_available(&models, "QWEN3:8B"));
        assert!(model_available(&models, "qwen 3 8b"));
        assert!(!model_available(&models, "other"));
    }

    #[test]
    fn normalize_policy_clamps_intervals_and_deduplicates_targets() {
        let policy = LocalModelPolicy {
            probe_interval_secs: 1,
            offline_grace_secs: 1000,
            stable_successes: 0,
            ollama_base_url: "http://127.0.0.1:11434/".to_string(),
            targets: vec![
                LocalModelTarget {
                    provider: LocalModelProviderKind::Ollama,
                    model: " qwen3:8b ".to_string(),
                    base_url: Some("http://127.0.0.1:11434/".to_string()),
                },
                LocalModelTarget {
                    provider: LocalModelProviderKind::Ollama,
                    model: "QWEN3:8B".to_string(),
                    base_url: None,
                },
            ],
            ..LocalModelPolicy::default()
        };

        let normalized = normalize_policy(policy);
        assert_eq!(normalized.probe_interval_secs, 5);
        assert_eq!(normalized.offline_grace_secs, 600);
        assert_eq!(normalized.stable_successes, 1);
        assert_eq!(normalized.ollama_base_url, "http://127.0.0.1:11434");
        assert_eq!(normalized.targets.len(), 1);
        assert_eq!(normalized.targets[0].model, "qwen3:8b");
        assert_eq!(normalized.targets[0].base_url, None);
    }

    #[test]
    fn target_hash_separates_cloud_and_local_codex() {
        let cloud = run_target_hash(
            AgentKind::Codex,
            None,
            None,
            None,
            None,
            ExecutionBackend::Host,
            ModelTargetKind::FrontierDefault,
            None,
        );
        let local = run_target_hash(
            AgentKind::Codex,
            Some("qwen3:8b"),
            None,
            Some(LocalModelProviderKind::Ollama),
            Some("http://127.0.0.1:11434"),
            ExecutionBackend::Host,
            ModelTargetKind::LocalProvider,
            None,
        );
        let legacy_cloud = legacy_run_target_hash(AgentKind::Codex, None, None, None, None);

        assert_ne!(cloud, local);
        assert!(target_hash_matches(Some(&cloud), &cloud, &legacy_cloud));
        assert!(target_hash_matches(
            Some(&legacy_cloud),
            &cloud,
            &legacy_cloud
        ));
        assert!(!target_hash_matches(Some(&local), &cloud, &legacy_cloud));
    }

    #[test]
    fn target_hash_separates_execution_backends() {
        let host = run_target_hash(
            AgentKind::Codex,
            Some("gpt-5.5"),
            Some("medium"),
            None,
            None,
            ExecutionBackend::Host,
            ModelTargetKind::FrontierDefault,
            None,
        );
        let docker = run_target_hash(
            AgentKind::Codex,
            Some("gpt-5.5"),
            Some("medium"),
            None,
            None,
            ExecutionBackend::DockerSandbox,
            ModelTargetKind::FrontierDefault,
            None,
        );
        let cloud = run_target_hash(
            AgentKind::Codex,
            Some("gpt-5.5"),
            Some("medium"),
            None,
            None,
            ExecutionBackend::Cloud,
            ModelTargetKind::FrontierDefault,
            None,
        );

        assert_ne!(host, docker);
        assert_ne!(host, cloud);
        assert_ne!(docker, cloud);
    }

    #[test]
    fn target_hash_normalizes_case_and_whitespace() {
        let left = run_target_hash(
            AgentKind::Codex,
            Some(" QWEN3:8B "),
            Some("Medium"),
            Some(LocalModelProviderKind::Ollama),
            Some(" HTTP://LOCALHOST:11434/ "),
            ExecutionBackend::Host,
            ModelTargetKind::LocalProvider,
            None,
        );
        let right = run_target_hash(
            AgentKind::Codex,
            Some("qwen3:8b"),
            Some("medium"),
            Some(LocalModelProviderKind::Ollama),
            Some("http://localhost:11434/"),
            ExecutionBackend::Host,
            ModelTargetKind::LocalProvider,
            None,
        );

        assert_eq!(left, right);
    }

    #[test]
    fn endpoint_joins_base_and_path() {
        assert_eq!(
            endpoint("http://127.0.0.1:11434/", "/api/tags"),
            "http://127.0.0.1:11434/api/tags"
        );
    }

    #[test]
    fn import_input_serializes_with_stable_fields() {
        let input = OllamaImportInput {
            model: "custom".to_string(),
            modelfile_path: "/tmp/Modelfile".to_string(),
        };
        let value = serde_json::to_value(input).unwrap();
        assert_eq!(value["model"], "custom");
        assert_eq!(value["modelfile_path"], "/tmp/Modelfile");
    }
}
