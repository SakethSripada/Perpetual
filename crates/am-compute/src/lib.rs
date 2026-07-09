//! Compute provider, placement, wallet, and billing helpers shared by the
//! desktop app and the hosted AgentManager control plane.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;

use am_proto::{
    new_id, now, ComputeLease, ComputeLeaseStatus, ComputeOffer, ComputeProviderKind, ComputeQuote,
    ComputeQuoteRequest, ModelRuntimeTargetKind, OpenModelSpec, WalletBalance, WalletTransaction,
    WalletTransactionKind,
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error)]
pub enum ComputeError {
    #[error("model '{0}' was not found in the open-model catalog")]
    ModelNotFound(String),
    #[error("no compatible compute offers were found")]
    NoCompatibleOffers,
    #[error("provider '{0}' is not implemented for production use yet")]
    ProviderUnavailable(&'static str),
    #[error(
        "wallet has insufficient available balance: need ${needed:.2}, available ${available:.2}"
    )]
    InsufficientBalance { needed: f64, available: f64 },
    #[error("invalid wallet amount")]
    InvalidAmount,
    #[error("invalid Stripe signature")]
    InvalidStripeSignature,
    #[error("provider HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("provider response could not be parsed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderLaunchSpec {
    pub quote: ComputeQuote,
    pub lease: ComputeLease,
    pub runner_image: String,
    pub hf_token_policy: HfTokenPolicy,
    pub lease_api_token: String,
    pub gateway_public_url: String,
    #[serde(default)]
    pub registry_login: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HfTokenPolicy {
    None,
    ControlPlaneSecret,
    UserScopedSecret,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInstance {
    pub provider: ComputeProviderKind,
    pub provider_instance_id: String,
    pub status: ComputeLeaseStatus,
    #[serde(default)]
    pub public_ip: Option<String>,
    #[serde(default)]
    pub direct_port: Option<u16>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub raw: Value,
}

#[async_trait]
pub trait ComputeProvider: Send + Sync {
    fn kind(&self) -> ComputeProviderKind;
    fn production_ready(&self) -> bool;
    async fn search_offers(
        &self,
        request: &ComputeQuoteRequest,
        model: &OpenModelSpec,
    ) -> Result<Vec<ComputeOffer>, ComputeError>;
    async fn create_instance(
        &self,
        offer: &ComputeOffer,
        launch: &ProviderLaunchSpec,
    ) -> Result<ProviderInstance, ComputeError>;
    async fn get_instance(
        &self,
        provider_instance_id: &str,
    ) -> Result<ProviderInstance, ComputeError>;
    async fn terminate_instance(&self, provider_instance_id: &str) -> Result<(), ComputeError>;
}

pub fn default_model_catalog() -> Vec<OpenModelSpec> {
    vec![
        OpenModelSpec {
            id: "qwen3-coder-30b-a3b".into(),
            label: "Qwen3 Coder 30B A3B".into(),
            family: "Qwen".into(),
            huggingface_model_id: "Qwen/Qwen3-Coder-30B-A3B-Instruct".into(),
            description: Some(
                "Strong coding model that fits on a single high-memory GPU with quantization."
                    .into(),
            ),
            runtime: ModelRuntimeTargetKind::VllmOpenAi,
            min_vram_gb: 24.0,
            recommended_vram_gb: 48.0,
            min_gpu_count: 1,
            disk_gb: 160,
            download_gb: 65.0,
            context_window: Some(262_144),
            quantization: Some("fp8/awq where available".into()),
            supports_tools: true,
            supports_coding_agent: true,
            vllm_args: vec![
                "--trust-remote-code".into(),
                "--enable-auto-tool-choice".into(),
            ],
            hidden: false,
        },
        OpenModelSpec {
            id: "qwen3-coder-480b-a35b".into(),
            label: "Qwen3 Coder 480B A35B".into(),
            family: "Qwen".into(),
            huggingface_model_id: "Qwen/Qwen3-Coder-480B-A35B-Instruct".into(),
            description: Some("Large MoE coding model for high-end multi-GPU rentals.".into()),
            runtime: ModelRuntimeTargetKind::VllmOpenAi,
            min_vram_gb: 320.0,
            recommended_vram_gb: 640.0,
            min_gpu_count: 4,
            disk_gb: 900,
            download_gb: 520.0,
            context_window: Some(262_144),
            quantization: Some("fp8".into()),
            supports_tools: true,
            supports_coding_agent: true,
            vllm_args: vec![
                "--trust-remote-code".into(),
                "--tensor-parallel-size=$AM_GPU_COUNT".into(),
            ],
            hidden: false,
        },
        OpenModelSpec {
            id: "kimi-k2-instruct".into(),
            label: "Kimi K2 Instruct".into(),
            family: "Kimi".into(),
            huggingface_model_id: "moonshotai/Kimi-K2-Instruct".into(),
            description: Some(
                "Large MoE model for agentic coding and tool use on multi-GPU rentals.".into(),
            ),
            runtime: ModelRuntimeTargetKind::VllmOpenAi,
            min_vram_gb: 320.0,
            recommended_vram_gb: 640.0,
            min_gpu_count: 4,
            disk_gb: 900,
            download_gb: 520.0,
            context_window: Some(131_072),
            quantization: Some("fp8".into()),
            supports_tools: true,
            supports_coding_agent: true,
            vllm_args: vec![
                "--trust-remote-code".into(),
                "--tensor-parallel-size=$AM_GPU_COUNT".into(),
            ],
            hidden: false,
        },
        OpenModelSpec {
            id: "deepseek-coder-v2-lite".into(),
            label: "DeepSeek Coder V2 Lite".into(),
            family: "DeepSeek".into(),
            huggingface_model_id: "deepseek-ai/DeepSeek-Coder-V2-Lite-Instruct".into(),
            description: Some("Fast inexpensive coding model for routine edits and triage.".into()),
            runtime: ModelRuntimeTargetKind::VllmOpenAi,
            min_vram_gb: 16.0,
            recommended_vram_gb: 24.0,
            min_gpu_count: 1,
            disk_gb: 100,
            download_gb: 35.0,
            context_window: Some(128_000),
            quantization: Some("awq/fp8 where available".into()),
            supports_tools: true,
            supports_coding_agent: true,
            vllm_args: vec!["--trust-remote-code".into()],
            hidden: false,
        },
        OpenModelSpec {
            id: "llama-3-1-8b-instruct".into(),
            label: "Llama 3.1 8B Instruct".into(),
            family: "Llama".into(),
            huggingface_model_id: "meta-llama/Llama-3.1-8B-Instruct".into(),
            description: Some("Tiny smoke-test and low-cost routing model.".into()),
            runtime: ModelRuntimeTargetKind::VllmOpenAi,
            min_vram_gb: 12.0,
            recommended_vram_gb: 16.0,
            min_gpu_count: 1,
            disk_gb: 80,
            download_gb: 18.0,
            context_window: Some(131_072),
            quantization: None,
            supports_tools: false,
            supports_coding_agent: false,
            vllm_args: Vec::new(),
            hidden: false,
        },
    ]
}

pub fn find_model(model_id: &str) -> Result<OpenModelSpec, ComputeError> {
    default_model_catalog()
        .into_iter()
        .find(|model| model.id == model_id || model.huggingface_model_id == model_id)
        .ok_or_else(|| ComputeError::ModelNotFound(model_id.to_string()))
}

pub fn score_offers(
    model: &OpenModelSpec,
    offers: impl IntoIterator<Item = ComputeOffer>,
) -> Vec<ComputeOffer> {
    let mut scored = offers
        .into_iter()
        .filter(|offer| offer_compatible(model, offer))
        .map(|mut offer| {
            offer.score = Some(score_offer(model, &offer));
            offer
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.price_per_hour_usd
                    .partial_cmp(&b.price_per_hour_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    scored
}

pub fn quote_top_offers(
    request: &ComputeQuoteRequest,
    model: &OpenModelSpec,
    offers: Vec<ComputeOffer>,
    limit: usize,
) -> Result<Vec<ComputeQuote>, ComputeError> {
    let quotes = score_offers(model, offers)
        .into_iter()
        .take(limit)
        .map(|mut offer| {
            offer
                .expected_cold_start_secs
                .get_or_insert_with(|| estimate_cold_start_secs(model, offer.bandwidth_gbps));
            let mut quote = ComputeQuote::new(model, offer, 5 * 60);
            quote.max_compute_usd = request.max_compute_usd;
            quote
        })
        .collect::<Vec<_>>();
    if quotes.is_empty() {
        Err(ComputeError::NoCompatibleOffers)
    } else {
        Ok(quotes)
    }
}

pub fn offer_compatible(model: &OpenModelSpec, offer: &ComputeOffer) -> bool {
    offer.rentable
        && offer.gpu_count >= model.min_gpu_count
        && (offer.gpu_vram_gb * offer.gpu_count as f64) >= model.min_vram_gb
        && offer.disk_gb >= model.disk_gb as f64
        && offer.direct_ports.unwrap_or(0) > 0
}

fn score_offer(model: &OpenModelSpec, offer: &ComputeOffer) -> f64 {
    let total_vram = offer.gpu_vram_gb * offer.gpu_count as f64;
    let vram_fit = (total_vram / model.recommended_vram_gb).min(1.5);
    let reliability = offer.reliability.unwrap_or(0.75).clamp(0.0, 1.0);
    let verified = if offer.verified { 0.1 } else { -0.3 };
    let ports = if offer.direct_ports.unwrap_or(0) > 0 {
        0.08
    } else {
        -0.5
    };
    let bandwidth = offer.bandwidth_gbps.unwrap_or(0.5).ln_1p().min(2.0) * 0.05;
    let cold_start_penalty =
        estimate_cold_start_secs(model, offer.bandwidth_gbps) as f64 / 3600.0 * 0.2;
    let price_penalty = offer.price_per_hour_usd.max(0.01).ln_1p() * 0.7;
    vram_fit + reliability + verified + ports + bandwidth - cold_start_penalty - price_penalty
}

pub fn estimate_cold_start_secs(model: &OpenModelSpec, bandwidth_gbps: Option<f64>) -> u32 {
    let bandwidth = bandwidth_gbps.unwrap_or(1.0).max(0.25);
    let download_secs = ((model.download_gb * 8.0) / bandwidth) as u32;
    (download_secs + 180).clamp(180, 7200)
}

#[derive(Debug, Clone)]
pub struct WalletLedger {
    org_id: String,
    available_usd: f64,
    reserved_usd: f64,
    transactions: Vec<WalletTransaction>,
}

impl WalletLedger {
    pub fn new(org_id: impl Into<String>) -> Self {
        Self {
            org_id: org_id.into(),
            available_usd: 0.0,
            reserved_usd: 0.0,
            transactions: Vec::new(),
        }
    }

    pub fn balance(&self) -> WalletBalance {
        WalletBalance {
            org_id: self.org_id.clone(),
            available_usd: round_money(self.available_usd),
            reserved_usd: round_money(self.reserved_usd),
            updated_at: now(),
        }
    }

    pub fn credit(
        &mut self,
        amount_usd: f64,
        idempotency_key: Option<String>,
    ) -> Result<WalletTransaction, ComputeError> {
        validate_amount(amount_usd)?;
        self.available_usd += amount_usd;
        Ok(self.push(
            WalletTransactionKind::Credit,
            amount_usd,
            None,
            idempotency_key,
        ))
    }

    pub fn reserve(
        &mut self,
        amount_usd: f64,
        lease_id: Option<String>,
    ) -> Result<WalletTransaction, ComputeError> {
        validate_amount(amount_usd)?;
        if self.available_usd + f64::EPSILON < amount_usd {
            return Err(ComputeError::InsufficientBalance {
                needed: amount_usd,
                available: self.available_usd,
            });
        }
        self.available_usd -= amount_usd;
        self.reserved_usd += amount_usd;
        Ok(self.push(WalletTransactionKind::Reserve, amount_usd, lease_id, None))
    }

    pub fn capture(
        &mut self,
        amount_usd: f64,
        lease_id: Option<String>,
    ) -> Result<WalletTransaction, ComputeError> {
        validate_amount(amount_usd)?;
        let captured = amount_usd.min(self.reserved_usd);
        self.reserved_usd -= captured;
        Ok(self.push(WalletTransactionKind::Capture, captured, lease_id, None))
    }

    pub fn refund(
        &mut self,
        amount_usd: f64,
        lease_id: Option<String>,
    ) -> Result<WalletTransaction, ComputeError> {
        validate_amount(amount_usd)?;
        let refunded = amount_usd.min(self.reserved_usd);
        self.reserved_usd -= refunded;
        self.available_usd += refunded;
        Ok(self.push(WalletTransactionKind::Refund, refunded, lease_id, None))
    }

    pub fn transactions(&self) -> &[WalletTransaction] {
        &self.transactions
    }

    fn push(
        &mut self,
        kind: WalletTransactionKind,
        amount_usd: f64,
        lease_id: Option<String>,
        idempotency_key: Option<String>,
    ) -> WalletTransaction {
        let txn = WalletTransaction {
            id: new_id(),
            org_id: self.org_id.clone(),
            kind,
            amount_usd: round_money(amount_usd),
            lease_id,
            stripe_event_id: None,
            idempotency_key,
            metadata: json!({}),
            created_at: now(),
        };
        self.transactions.push(txn.clone());
        txn
    }
}

pub fn verify_stripe_signature(
    payload: &[u8],
    stripe_signature_header: &str,
    webhook_secret: &str,
    tolerance_secs: i64,
    now: DateTime<Utc>,
) -> Result<(), ComputeError> {
    let timestamp = parse_stripe_signature_part(stripe_signature_header, "t")
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or(ComputeError::InvalidStripeSignature)?;
    if (now.timestamp() - timestamp).abs() > tolerance_secs {
        return Err(ComputeError::InvalidStripeSignature);
    }
    let expected = parse_stripe_signature_part(stripe_signature_header, "v1")
        .ok_or(ComputeError::InvalidStripeSignature)?;
    let signed = format!("{timestamp}.{}", String::from_utf8_lossy(payload));
    let mut mac = HmacSha256::new_from_slice(webhook_secret.as_bytes())
        .map_err(|_| ComputeError::InvalidStripeSignature)?;
    mac.update(signed.as_bytes());
    let actual = hex::encode(mac.finalize().into_bytes());
    if constant_time_eq(actual.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(ComputeError::InvalidStripeSignature)
    }
}

fn parse_stripe_signature_part<'a>(header: &'a str, key: &str) -> Option<&'a str> {
    header.split(',').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == key).then_some(v)
    })
}

fn validate_amount(amount_usd: f64) -> Result<(), ComputeError> {
    if amount_usd.is_finite() && amount_usd > 0.0 {
        Ok(())
    } else {
        Err(ComputeError::InvalidAmount)
    }
}

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

fn normalize_bearer_secret(value: &str) -> String {
    value
        .trim()
        .strip_prefix("Bearer ")
        .or_else(|| value.trim().strip_prefix("bearer "))
        .unwrap_or_else(|| value.trim())
        .trim()
        .to_string()
}

fn truncate_provider_error(value: &str) -> String {
    const MAX: usize = 400;
    if value.len() <= MAX {
        value.to_string()
    } else {
        format!("{}...", &value[..MAX])
    }
}

pub struct VastProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl VastProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        let api_key = normalize_bearer_secret(&api_key.into());
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://cloud.vast.ai".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }
}

#[async_trait]
impl ComputeProvider for VastProvider {
    fn kind(&self) -> ComputeProviderKind {
        ComputeProviderKind::Vast
    }

    fn production_ready(&self) -> bool {
        true
    }

    async fn search_offers(
        &self,
        request: &ComputeQuoteRequest,
        model: &OpenModelSpec,
    ) -> Result<Vec<ComputeOffer>, ComputeError> {
        let body = vast_search_body(request, model);
        let response = self
            .client
            .get(self.url("/api/v0/bundles/"))
            .bearer_auth(&self.api_key)
            .query(&[("q", body.to_string())])
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ComputeError::Other(format!(
                "Vast search returned HTTP {status}: {}",
                truncate_provider_error(&body)
            )));
        }
        let value = response.json::<Value>().await?;
        Ok(parse_vast_offers(&value))
    }

    async fn create_instance(
        &self,
        offer: &ComputeOffer,
        launch: &ProviderLaunchSpec,
    ) -> Result<ProviderInstance, ComputeError> {
        let body = vast_create_body(offer, launch);
        let response = self
            .client
            .put(self.url(&format!("/api/v0/asks/{}/", offer.provider_offer_id)))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let value = response.json::<Value>().await?;
        Ok(ProviderInstance {
            provider: ComputeProviderKind::Vast,
            provider_instance_id: value
                .get("new_contract")
                .or_else(|| value.get("instance_id"))
                .or_else(|| value.get("id"))
                .and_then(value_to_string)
                .unwrap_or_else(|| launch.lease.id.clone()),
            status: ComputeLeaseStatus::Provisioning,
            public_ip: None,
            direct_port: None,
            message: value
                .get("msg")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            raw: value,
        })
    }

    async fn get_instance(
        &self,
        provider_instance_id: &str,
    ) -> Result<ProviderInstance, ComputeError> {
        let response = self
            .client
            .get(self.url(&format!("/api/v0/instances/{provider_instance_id}/")))
            .bearer_auth(&self.api_key)
            .send()
            .await?
            .error_for_status()?;
        let value = response.json::<Value>().await?;
        Ok(parse_vast_instance(provider_instance_id, value))
    }

    async fn terminate_instance(&self, provider_instance_id: &str) -> Result<(), ComputeError> {
        self.client
            .delete(self.url(&format!("/api/v0/instances/{provider_instance_id}/")))
            .bearer_auth(&self.api_key)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

pub struct RunPodProvider;

#[async_trait]
impl ComputeProvider for RunPodProvider {
    fn kind(&self) -> ComputeProviderKind {
        ComputeProviderKind::Runpod
    }

    fn production_ready(&self) -> bool {
        false
    }

    async fn search_offers(
        &self,
        _request: &ComputeQuoteRequest,
        _model: &OpenModelSpec,
    ) -> Result<Vec<ComputeOffer>, ComputeError> {
        Err(ComputeError::ProviderUnavailable("runpod"))
    }

    async fn create_instance(
        &self,
        _offer: &ComputeOffer,
        _launch: &ProviderLaunchSpec,
    ) -> Result<ProviderInstance, ComputeError> {
        Err(ComputeError::ProviderUnavailable("runpod"))
    }

    async fn get_instance(
        &self,
        _provider_instance_id: &str,
    ) -> Result<ProviderInstance, ComputeError> {
        Err(ComputeError::ProviderUnavailable("runpod"))
    }

    async fn terminate_instance(&self, _provider_instance_id: &str) -> Result<(), ComputeError> {
        Err(ComputeError::ProviderUnavailable("runpod"))
    }
}

pub struct LambdaProvider;

#[async_trait]
impl ComputeProvider for LambdaProvider {
    fn kind(&self) -> ComputeProviderKind {
        ComputeProviderKind::Lambda
    }

    fn production_ready(&self) -> bool {
        false
    }

    async fn search_offers(
        &self,
        _request: &ComputeQuoteRequest,
        _model: &OpenModelSpec,
    ) -> Result<Vec<ComputeOffer>, ComputeError> {
        Err(ComputeError::ProviderUnavailable("lambda"))
    }

    async fn create_instance(
        &self,
        _offer: &ComputeOffer,
        _launch: &ProviderLaunchSpec,
    ) -> Result<ProviderInstance, ComputeError> {
        Err(ComputeError::ProviderUnavailable("lambda"))
    }

    async fn get_instance(
        &self,
        _provider_instance_id: &str,
    ) -> Result<ProviderInstance, ComputeError> {
        Err(ComputeError::ProviderUnavailable("lambda"))
    }

    async fn terminate_instance(&self, _provider_instance_id: &str) -> Result<(), ComputeError> {
        Err(ComputeError::ProviderUnavailable("lambda"))
    }
}

pub fn vast_search_body(request: &ComputeQuoteRequest, model: &OpenModelSpec) -> Value {
    let mut body = json!({
        "limit": 100,
        "type": if request.allow_interruptible { "bid" } else { "ondemand" },
        "verified": { "eq": true },
        "rentable": { "eq": true },
        "rented": { "eq": false },
        "num_gpus": { "gte": model.min_gpu_count },
        "gpu_ram": { "gte": (model.min_vram_gb / model.min_gpu_count as f64 * 1024.0).ceil() as u64 },
        "disk_space": { "gte": model.disk_gb },
        "direct_port_count": { "gte": 1 },
    });
    if let Some(max) = request.max_compute_usd {
        body["dph_total"] = json!({ "lte": max.max(0.01) });
    }
    if !request.region_allowlist.is_empty() {
        body["geolocation"] = json!({ "in": request.region_allowlist });
    }
    body
}

pub fn vast_create_body(offer: &ComputeOffer, launch: &ProviderLaunchSpec) -> Value {
    let model = &launch.quote.model_id;
    let hf_model = &launch.quote.model_label;
    let env = json!({
        "AM_LEASE_ID": launch.lease.id,
        "AM_PROVIDER": launch.lease.provider.as_str(),
        "AM_MODEL_ID": model,
        "AM_MODEL_LABEL": hf_model,
        "AM_GATEWAY_URL": launch.gateway_public_url,
        "AM_LEASE_API_TOKEN": launch.lease_api_token,
        "AM_GPU_COUNT": offer.gpu_count.to_string(),
        "-p 8000:8000": "1",
    });
    let mut body = json!({
        "image": launch.runner_image,
        "disk": offer.disk_gb.max(launch.quote.offer.disk_gb).ceil() as u64,
        "runtype": "args",
        "env": env,
        "args_str": runner_args(&launch.quote.model_id),
    });
    if let Some(login) = launch.registry_login.as_deref() {
        body["image_login"] = json!(login);
    }
    body
}

fn runner_args(model: &str) -> String {
    format!("--model {model} --host 0.0.0.0 --port 8000 --api-key $AM_LEASE_API_TOKEN")
}

pub fn parse_vast_offers(value: &Value) -> Vec<ComputeOffer> {
    let offers_value = value
        .get("offers")
        .or_else(|| value.get("results"))
        .unwrap_or(value);
    let offers: Vec<Value> = match offers_value {
        Value::Array(items) => items.clone(),
        Value::Object(_) => vec![offers_value.clone()],
        _ => Vec::new(),
    };
    offers
        .into_iter()
        .filter_map(|offer| parse_vast_offer(&offer))
        .collect()
}

fn parse_vast_offer(value: &Value) -> Option<ComputeOffer> {
    let provider_offer_id = value
        .get("id")
        .or_else(|| value.get("ask_contract_id"))
        .and_then(value_to_string)?;
    let price = number_at(value, &["dph_total"])
        .or_else(|| number_at(value, &["search", "totalHour"]))
        .or_else(|| number_at(value, &["instance", "totalHour"]))?;
    Some(ComputeOffer {
        id: format!("vast:{provider_offer_id}"),
        provider: ComputeProviderKind::Vast,
        provider_offer_id,
        machine_id: value.get("machine_id").and_then(value_to_string),
        gpu_name: value
            .get("gpu_name")
            .and_then(Value::as_str)
            .unwrap_or("GPU")
            .to_string(),
        gpu_count: value.get("num_gpus").and_then(Value::as_u64).unwrap_or(1) as u32,
        gpu_vram_gb: value.get("gpu_ram").and_then(Value::as_f64).unwrap_or(0.0) / 1024.0,
        cpu_cores: value.get("cpu_cores_effective").and_then(Value::as_f64),
        memory_gb: value
            .get("cpu_ram")
            .and_then(Value::as_f64)
            .map(|mb| mb / 1024.0),
        disk_gb: value
            .get("disk_space")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        region: value
            .get("geolocation")
            .or_else(|| value.get("geolocode"))
            .and_then(value_to_string),
        price_per_hour_usd: price,
        reliability: value
            .get("reliability2")
            .or_else(|| value.get("reliability"))
            .and_then(Value::as_f64),
        verified: value
            .get("verification")
            .and_then(Value::as_str)
            .is_some_and(|status| status == "verified")
            || value
                .get("verified")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        rentable: value
            .get("rentable")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        direct_ports: value
            .get("direct_port_count")
            .and_then(Value::as_u64)
            .map(|count| count as u32),
        bandwidth_gbps: value
            .get("inet_down")
            .and_then(Value::as_f64)
            .map(|mbps| mbps / 1000.0),
        expected_cold_start_secs: None,
        score: None,
        raw: value.clone(),
    })
}

fn parse_vast_instance(provider_instance_id: &str, value: Value) -> ProviderInstance {
    let status = match value
        .get("actual_status")
        .or_else(|| value.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "running" => ComputeLeaseStatus::Running,
        "loading" | "starting" | "pending" => ComputeLeaseStatus::Provisioning,
        "exited" | "stopped" => ComputeLeaseStatus::Terminated,
        _ => ComputeLeaseStatus::Provisioning,
    };
    ProviderInstance {
        provider: ComputeProviderKind::Vast,
        provider_instance_id: provider_instance_id.to_string(),
        status,
        public_ip: value
            .get("public_ipaddr")
            .or_else(|| value.get("ssh_host"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        direct_port: value
            .get("direct_port")
            .or_else(|| value.get("ssh_port"))
            .and_then(Value::as_u64)
            .map(|port| port as u16),
        message: value
            .get("status_msg")
            .or_else(|| value.get("msg"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        raw: value,
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn number_at(value: &Value, path: &[&str]) -> Option<f64> {
    let mut cursor = value;
    for part in path {
        cursor = cursor.get(*part)?;
    }
    cursor.as_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scorer_filters_and_orders_compatible_offers() {
        let model = find_model("deepseek-coder-v2-lite").unwrap();
        let cheap_good = offer("1", 24.0, 1, 120.0, 0.6, 0.99, 2);
        let too_small = offer("2", 8.0, 1, 120.0, 0.1, 0.99, 2);
        let no_ports = offer("3", 24.0, 1, 120.0, 0.2, 0.99, 0);
        let scored = score_offers(&model, vec![too_small, no_ports, cheap_good.clone()]);
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].provider_offer_id, cheap_good.provider_offer_id);
    }

    #[test]
    fn wallet_reserve_capture_refund_roundtrip() {
        let mut ledger = WalletLedger::new("org");
        ledger.credit(10.0, Some("stripe-event".into())).unwrap();
        ledger.reserve(4.0, Some("lease".into())).unwrap();
        assert_eq!(ledger.balance().available_usd, 6.0);
        assert_eq!(ledger.balance().reserved_usd, 4.0);
        ledger.capture(1.5, Some("lease".into())).unwrap();
        assert_eq!(ledger.balance().reserved_usd, 2.5);
        ledger.refund(2.5, Some("lease".into())).unwrap();
        assert_eq!(ledger.balance().available_usd, 8.5);
        assert_eq!(ledger.balance().reserved_usd, 0.0);
    }

    #[test]
    fn wallet_rejects_over_reservation() {
        let mut ledger = WalletLedger::new("org");
        ledger.credit(1.0, None).unwrap();
        assert!(matches!(
            ledger.reserve(2.0, None),
            Err(ComputeError::InsufficientBalance { .. })
        ));
    }

    #[test]
    fn stripe_signature_verifies() {
        let payload = br#"{"id":"evt_1"}"#;
        let secret = "whsec_test";
        let ts = Utc::now().timestamp();
        let signed = format!("{ts}.{}", String::from_utf8_lossy(payload));
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(signed.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        let header = format!("t={ts},v1={sig}");
        verify_stripe_signature(payload, &header, secret, 300, Utc::now()).unwrap();
    }

    #[test]
    fn vast_offer_parser_reads_docs_shape() {
        let value = json!({
            "offers": [{
                "id": 123,
                "num_gpus": 1,
                "gpu_name": "RTX 4090",
                "gpu_ram": 24576,
                "disk_space": 200,
                "dph_total": 0.45,
                "rentable": true,
                "verification": "verified",
                "direct_port_count": 4,
                "reliability2": 0.99
            }]
        });
        let offers = parse_vast_offers(&value);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].gpu_vram_gb, 24.0);
    }

    fn offer(
        id: &str,
        gpu_vram_gb: f64,
        gpu_count: u32,
        disk_gb: f64,
        price: f64,
        reliability: f64,
        ports: u32,
    ) -> ComputeOffer {
        ComputeOffer {
            id: format!("vast:{id}"),
            provider: ComputeProviderKind::Vast,
            provider_offer_id: id.to_string(),
            machine_id: None,
            gpu_name: "RTX 4090".into(),
            gpu_count,
            gpu_vram_gb,
            cpu_cores: Some(16.0),
            memory_gb: Some(64.0),
            disk_gb,
            region: Some("US".into()),
            price_per_hour_usd: price,
            reliability: Some(reliability),
            verified: true,
            rentable: true,
            direct_ports: Some(ports),
            bandwidth_gbps: Some(1.0),
            expected_cold_start_secs: None,
            score: None,
            raw: json!({}),
        }
    }
}
