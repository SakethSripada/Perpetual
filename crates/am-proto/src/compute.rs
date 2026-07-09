use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{new_id, now, LocalModelProviderKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeProviderKind {
    Vast,
    Runpod,
    Lambda,
}

impl ComputeProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Vast => "vast",
            Self::Runpod => "runpod",
            Self::Lambda => "lambda",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Vast => "Vast.ai",
            Self::Runpod => "RunPod",
            Self::Lambda => "Lambda Cloud",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "vast" | "vast_ai" => Self::Vast,
            "runpod" | "run_pod" => Self::Runpod,
            "lambda" | "lambda_cloud" => Self::Lambda,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeLeaseStatus {
    Quoted,
    PaymentPending,
    Reserved,
    Provisioning,
    ImagePulling,
    ModelDownloading,
    Loading,
    Ready,
    Running,
    Draining,
    Expired,
    Terminated,
    Failed,
}

impl ComputeLeaseStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Quoted => "quoted",
            Self::PaymentPending => "payment_pending",
            Self::Reserved => "reserved",
            Self::Provisioning => "provisioning",
            Self::ImagePulling => "image_pulling",
            Self::ModelDownloading => "model_downloading",
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Draining => "draining",
            Self::Expired => "expired",
            Self::Terminated => "terminated",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "quoted" => Self::Quoted,
            "payment_pending" => Self::PaymentPending,
            "reserved" => Self::Reserved,
            "provisioning" => Self::Provisioning,
            "image_pulling" => Self::ImagePulling,
            "model_downloading" => Self::ModelDownloading,
            "loading" => Self::Loading,
            "ready" => Self::Ready,
            "running" => Self::Running,
            "draining" => Self::Draining,
            "expired" => Self::Expired,
            "terminated" => Self::Terminated,
            "failed" => Self::Failed,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Expired | Self::Terminated | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTargetKind {
    FrontierDefault,
    LocalProvider,
    RentedCompute,
}

impl Default for ModelTargetKind {
    fn default() -> Self {
        Self::FrontierDefault
    }
}

impl ModelTargetKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FrontierDefault => "frontier_default",
            Self::LocalProvider => "local_provider",
            Self::RentedCompute => "rented_compute",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::FrontierDefault => "Frontier default",
            Self::LocalProvider => "Local provider",
            Self::RentedCompute => "Rented compute",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "frontier_default" | "default" | "cloud" => Self::FrontierDefault,
            "local_provider" | "local" => Self::LocalProvider,
            "rented_compute" | "rented" | "remote_model" => Self::RentedCompute,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRuntimeTarget {
    #[serde(default)]
    pub kind: ModelTargetKind,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub local_provider: Option<LocalModelProviderKind>,
    #[serde(default)]
    pub local_base_url: Option<String>,
    #[serde(default)]
    pub compute_lease_id: Option<String>,
    #[serde(default)]
    pub compute_provider: Option<ComputeProviderKind>,
    #[serde(default)]
    pub endpoint_base_url: Option<String>,
    #[serde(default)]
    pub endpoint_token_configured: bool,
}

impl Default for ModelRuntimeTarget {
    fn default() -> Self {
        Self {
            kind: ModelTargetKind::FrontierDefault,
            model: None,
            local_provider: None,
            local_base_url: None,
            compute_lease_id: None,
            compute_provider: None,
            endpoint_base_url: None,
            endpoint_token_configured: false,
        }
    }
}

impl ModelRuntimeTarget {
    pub fn frontier(model: Option<String>) -> Self {
        Self {
            kind: ModelTargetKind::FrontierDefault,
            model,
            ..Default::default()
        }
    }

    pub fn local(
        provider: LocalModelProviderKind,
        model: String,
        base_url: Option<String>,
    ) -> Self {
        Self {
            kind: ModelTargetKind::LocalProvider,
            model: Some(model),
            local_provider: Some(provider),
            local_base_url: base_url,
            ..Default::default()
        }
    }

    pub fn rented(
        provider: ComputeProviderKind,
        lease_id: String,
        model: String,
        endpoint_base_url: Option<String>,
        endpoint_token_configured: bool,
    ) -> Self {
        Self {
            kind: ModelTargetKind::RentedCompute,
            model: Some(model),
            compute_lease_id: Some(lease_id),
            compute_provider: Some(provider),
            endpoint_base_url,
            endpoint_token_configured,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenModelSpec {
    pub id: String,
    pub label: String,
    pub family: String,
    pub huggingface_model_id: String,
    #[serde(default)]
    pub description: Option<String>,
    pub runtime: ModelRuntimeTargetKind,
    pub min_vram_gb: f64,
    pub recommended_vram_gb: f64,
    pub min_gpu_count: u32,
    pub disk_gb: u32,
    pub download_gb: f64,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub quantization: Option<String>,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_coding_agent: bool,
    #[serde(default)]
    pub vllm_args: Vec<String>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRuntimeTargetKind {
    VllmOpenAi,
}

impl ModelRuntimeTargetKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::VllmOpenAi => "vllm_openai",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeOffer {
    pub id: String,
    pub provider: ComputeProviderKind,
    pub provider_offer_id: String,
    #[serde(default)]
    pub machine_id: Option<String>,
    pub gpu_name: String,
    pub gpu_count: u32,
    pub gpu_vram_gb: f64,
    #[serde(default)]
    pub cpu_cores: Option<f64>,
    #[serde(default)]
    pub memory_gb: Option<f64>,
    pub disk_gb: f64,
    #[serde(default)]
    pub region: Option<String>,
    pub price_per_hour_usd: f64,
    #[serde(default)]
    pub reliability: Option<f64>,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub rentable: bool,
    #[serde(default)]
    pub direct_ports: Option<u32>,
    #[serde(default)]
    pub bandwidth_gbps: Option<f64>,
    #[serde(default)]
    pub expected_cold_start_secs: Option<u32>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeQuote {
    pub id: String,
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    pub provider: ComputeProviderKind,
    pub model_id: String,
    pub model_label: String,
    pub offer: ComputeOffer,
    pub estimated_startup_secs: u32,
    pub estimated_hourly_usd: f64,
    pub estimated_minimum_usd: f64,
    #[serde(default)]
    pub max_compute_usd: Option<f64>,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl ComputeQuote {
    pub fn new(model: &OpenModelSpec, offer: ComputeOffer, ttl_secs: i64) -> Self {
        let created_at = now();
        Self {
            id: new_id(),
            org_id: None,
            user_id: None,
            provider: offer.provider,
            model_id: model.id.clone(),
            model_label: model.label.clone(),
            estimated_startup_secs: offer
                .expected_cold_start_secs
                .unwrap_or_else(|| estimate_startup_secs(model.download_gb, offer.bandwidth_gbps)),
            estimated_hourly_usd: offer.price_per_hour_usd,
            estimated_minimum_usd: (offer.price_per_hour_usd / 2.0).max(0.25),
            max_compute_usd: None,
            offer,
            expires_at: created_at + chrono::Duration::seconds(ttl_secs),
            created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeLease {
    pub id: String,
    #[serde(default)]
    pub quote_id: Option<String>,
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    pub provider: ComputeProviderKind,
    #[serde(default)]
    pub provider_instance_id: Option<String>,
    pub model_id: String,
    pub model_label: String,
    pub status: ComputeLeaseStatus,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub gpu_summary: Option<String>,
    pub price_per_hour_usd: f64,
    pub max_compute_usd: f64,
    #[serde(default)]
    pub estimated_cost_usd: Option<f64>,
    #[serde(default)]
    pub endpoint_base_url: Option<String>,
    #[serde(default)]
    pub endpoint_token_configured: bool,
    #[serde(default)]
    pub fallback_target: Option<ModelRuntimeTarget>,
    #[serde(default)]
    pub status_message: Option<String>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub ready_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub terminated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ComputeLease {
    pub fn from_quote(quote: &ComputeQuote, max_compute_usd: f64) -> Self {
        let ts = now();
        Self {
            id: new_id(),
            quote_id: Some(quote.id.clone()),
            org_id: quote.org_id.clone(),
            user_id: quote.user_id.clone(),
            provider: quote.provider,
            provider_instance_id: None,
            model_id: quote.model_id.clone(),
            model_label: quote.model_label.clone(),
            status: ComputeLeaseStatus::Reserved,
            region: quote.offer.region.clone(),
            gpu_summary: Some(format!(
                "{} x {} ({:.0} GB each)",
                quote.offer.gpu_count, quote.offer.gpu_name, quote.offer.gpu_vram_gb
            )),
            price_per_hour_usd: quote.estimated_hourly_usd,
            max_compute_usd,
            estimated_cost_usd: Some(0.0),
            endpoint_base_url: None,
            endpoint_token_configured: false,
            fallback_target: None,
            status_message: None,
            started_at: None,
            ready_at: None,
            expires_at: Some(
                ts + chrono::Duration::seconds(seconds_at_cap(
                    quote.estimated_hourly_usd,
                    max_compute_usd,
                ) as i64),
            ),
            terminated_at: None,
            created_at: ts,
            updated_at: ts,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpendPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub auto_purchase_enabled: bool,
    #[serde(default)]
    pub tos_accepted_at: Option<DateTime<Utc>>,
    #[serde(default = "default_max_per_run_usd")]
    pub max_per_run_usd: f64,
    #[serde(default = "default_daily_cap_usd")]
    pub daily_cap_usd: f64,
    #[serde(default = "default_monthly_cap_usd")]
    pub monthly_cap_usd: f64,
    #[serde(default = "default_true")]
    pub kill_at_cap: bool,
    #[serde(default = "default_providers")]
    pub provider_allowlist: Vec<ComputeProviderKind>,
    #[serde(default)]
    pub model_allowlist: Vec<String>,
    #[serde(default)]
    pub allow_org_lease_sharing: bool,
}

impl Default for SpendPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_purchase_enabled: false,
            tos_accepted_at: None,
            max_per_run_usd: default_max_per_run_usd(),
            daily_cap_usd: default_daily_cap_usd(),
            monthly_cap_usd: default_monthly_cap_usd(),
            kill_at_cap: true,
            provider_allowlist: default_providers(),
            model_allowlist: Vec::new(),
            allow_org_lease_sharing: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletBalance {
    pub org_id: String,
    pub available_usd: f64,
    pub reserved_usd: f64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalletTransactionKind {
    Credit,
    Reserve,
    Capture,
    Refund,
    Adjustment,
}

impl WalletTransactionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Credit => "credit",
            Self::Reserve => "reserve",
            Self::Capture => "capture",
            Self::Refund => "refund",
            Self::Adjustment => "adjustment",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "credit" => Self::Credit,
            "reserve" => Self::Reserve,
            "capture" => Self::Capture,
            "refund" => Self::Refund,
            "adjustment" => Self::Adjustment,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletTransaction {
    pub id: String,
    pub org_id: String,
    pub kind: WalletTransactionKind,
    pub amount_usd: f64,
    #[serde(default)]
    pub lease_id: Option<String>,
    #[serde(default)]
    pub stripe_event_id: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RentedComputePolicy {
    #[serde(default)]
    pub control_plane_url: Option<String>,
    #[serde(default)]
    pub control_plane_token_configured: bool,
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub spend: SpendPolicy,
    #[serde(default)]
    pub default_target: ModelRuntimeTarget,
    #[serde(default)]
    pub fallback_target: ModelRuntimeTarget,
}

impl Default for RentedComputePolicy {
    fn default() -> Self {
        Self {
            control_plane_url: None,
            control_plane_token_configured: false,
            org_id: None,
            user_id: None,
            spend: SpendPolicy::default(),
            default_target: ModelRuntimeTarget::default(),
            fallback_target: ModelRuntimeTarget::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeSetupStatus {
    pub control_plane_configured: bool,
    pub authenticated: bool,
    #[serde(default)]
    pub wallet_balance: Option<WalletBalance>,
    #[serde(default)]
    pub production_providers: Vec<ComputeProviderKind>,
    #[serde(default)]
    pub hidden_providers: Vec<ComputeProviderKind>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeQuoteRequest {
    pub model_id: String,
    #[serde(default)]
    pub provider_allowlist: Vec<ComputeProviderKind>,
    #[serde(default)]
    pub max_compute_usd: Option<f64>,
    #[serde(default)]
    pub allow_interruptible: bool,
    #[serde(default)]
    pub region_allowlist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeQuoteResponse {
    pub quotes: Vec<ComputeQuote>,
    pub model: OpenModelSpec,
    pub wallet: Option<WalletBalance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateComputeLeaseRequest {
    pub quote_id: String,
    pub max_compute_usd: f64,
    #[serde(default)]
    pub allow_auto_purchase: bool,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub fallback_target: Option<ModelRuntimeTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RentedEndpoint {
    pub lease_id: String,
    pub provider: ComputeProviderKind,
    pub model: String,
    pub base_url: String,
    pub bearer_token: String,
    pub expires_at: Option<DateTime<Utc>>,
}

fn estimate_startup_secs(download_gb: f64, bandwidth_gbps: Option<f64>) -> u32 {
    let bandwidth = bandwidth_gbps.unwrap_or(1.0).max(0.25);
    let download_secs = ((download_gb * 8.0) / bandwidth) as u32;
    (download_secs + 180).clamp(180, 3600)
}

fn seconds_at_cap(price_per_hour_usd: f64, max_compute_usd: f64) -> u64 {
    if price_per_hour_usd <= 0.0 {
        return 0;
    }
    ((max_compute_usd / price_per_hour_usd) * 3600.0).max(0.0) as u64
}

fn default_true() -> bool {
    true
}

fn default_max_per_run_usd() -> f64 {
    12.0
}

fn default_daily_cap_usd() -> f64 {
    50.0
}

fn default_monthly_cap_usd() -> f64 {
    500.0
}

fn default_providers() -> Vec<ComputeProviderKind> {
    vec![ComputeProviderKind::Vast]
}
