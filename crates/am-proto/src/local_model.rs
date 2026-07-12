use serde::{Deserialize, Serialize};

/// Local model hosts Perpetual can use for offline fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalModelProviderKind {
    Ollama,
    LmStudio,
}

impl LocalModelProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LocalModelProviderKind::Ollama => "ollama",
            LocalModelProviderKind::LmStudio => "lm_studio",
        }
    }

    pub fn codex_oss_provider(&self) -> &'static str {
        match self {
            LocalModelProviderKind::Ollama => "ollama",
            LocalModelProviderKind::LmStudio => "lmstudio",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            LocalModelProviderKind::Ollama => "Ollama",
            LocalModelProviderKind::LmStudio => "LM Studio",
        }
    }

    pub fn default_base_url(&self) -> &'static str {
        match self {
            LocalModelProviderKind::Ollama => "http://127.0.0.1:11434",
            LocalModelProviderKind::LmStudio => "http://127.0.0.1:1234",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ollama" => LocalModelProviderKind::Ollama,
            "lm_studio" | "lmstudio" => LocalModelProviderKind::LmStudio,
            _ => return None,
        })
    }
}

/// A model known to a local provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalModelInfo {
    pub id: String,
    pub name: String,
    pub family: Option<String>,
    pub parameter_size: Option<String>,
    pub quantization: Option<String>,
    pub size: Option<u64>,
    pub loaded: bool,
}

/// Probe result for a local model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelStatus {
    pub provider: LocalModelProviderKind,
    pub label: String,
    pub base_url: String,
    pub server_running: bool,
    pub cli_installed: bool,
    pub cli_path: Option<String>,
    pub authenticated: bool,
    pub version: Option<String>,
    pub models: Vec<LocalModelInfo>,
    pub error: Option<String>,
}

/// A ranked local model fallback target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelTarget {
    pub provider: LocalModelProviderKind,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
}

/// User-configurable policy for network recovery and local fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelPolicy {
    #[serde(default = "default_true")]
    pub auto_resume_cloud: bool,
    #[serde(default = "default_true")]
    pub use_local_fallback: bool,
    #[serde(default = "default_true")]
    pub switch_back_to_cloud: bool,
    #[serde(default = "default_probe_interval")]
    pub probe_interval_secs: u64,
    #[serde(default = "default_offline_grace")]
    pub offline_grace_secs: u64,
    #[serde(default = "default_stable_successes")]
    pub stable_successes: u32,
    #[serde(default = "default_ollama_base_url")]
    pub ollama_base_url: String,
    #[serde(default = "default_lm_studio_base_url")]
    pub lm_studio_base_url: String,
    #[serde(default)]
    pub lm_studio_api_token_configured: bool,
    /// Accepted by `set_local_model_policy`, saved to keyring, never persisted
    /// in settings or returned from `get_local_model_policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lm_studio_api_token: Option<String>,
    #[serde(default)]
    pub targets: Vec<LocalModelTarget>,
}

/// Request to import/create an Ollama model from a local Modelfile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaImportInput {
    pub model: String,
    pub modelfile_path: String,
}

impl Default for LocalModelPolicy {
    fn default() -> Self {
        Self {
            auto_resume_cloud: true,
            use_local_fallback: true,
            switch_back_to_cloud: true,
            probe_interval_secs: default_probe_interval(),
            offline_grace_secs: default_offline_grace(),
            stable_successes: default_stable_successes(),
            ollama_base_url: default_ollama_base_url(),
            lm_studio_base_url: default_lm_studio_base_url(),
            lm_studio_api_token_configured: false,
            lm_studio_api_token: None,
            targets: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_probe_interval() -> u64 {
    30
}

fn default_offline_grace() -> u64 {
    10
}

fn default_stable_successes() -> u32 {
    2
}

fn default_ollama_base_url() -> String {
    LocalModelProviderKind::Ollama
        .default_base_url()
        .to_string()
}

fn default_lm_studio_base_url() -> String {
    LocalModelProviderKind::LmStudio
        .default_base_url()
        .to_string()
}
