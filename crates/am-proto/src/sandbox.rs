use serde::{Deserialize, Serialize};

use crate::ExecutionBackend;

/// User-configurable policy for Docker-backed agent sandboxes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxPolicy {
    #[serde(default)]
    pub default_backend: ExecutionBackend,
    #[serde(default = "default_max_concurrent_sandboxes")]
    pub max_concurrent_sandboxes: usize,
    #[serde(default = "default_cpus")]
    pub cpus: u32,
    #[serde(default = "default_memory")]
    pub memory: String,
    #[serde(default = "default_network_preset")]
    pub network_preset: String,
    #[serde(default = "default_run_timeout_secs")]
    pub run_timeout_secs: u64,
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    #[serde(default = "default_stop_grace_secs")]
    pub stop_grace_secs: u64,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            default_backend: ExecutionBackend::Host,
            max_concurrent_sandboxes: default_max_concurrent_sandboxes(),
            cpus: default_cpus(),
            memory: default_memory(),
            network_preset: default_network_preset(),
            run_timeout_secs: default_run_timeout_secs(),
            idle_timeout_secs: default_idle_timeout_secs(),
            stop_grace_secs: default_stop_grace_secs(),
        }
    }
}

fn default_max_concurrent_sandboxes() -> usize {
    2
}

fn default_cpus() -> u32 {
    2
}

fn default_memory() -> String {
    "4g".to_string()
}

fn default_network_preset() -> String {
    "balanced".to_string()
}

fn default_run_timeout_secs() -> u64 {
    7_200
}

fn default_idle_timeout_secs() -> u64 {
    900
}

fn default_stop_grace_secs() -> u64 {
    30
}

/// Runtime readiness for Docker's `sbx` sandbox CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRuntimeStatus {
    pub installed: bool,
    pub authenticated: bool,
    #[serde(default)]
    pub codex_authenticated: bool,
    pub version: Option<String>,
    pub binary_path: Option<String>,
    pub active_count: usize,
    pub error: Option<String>,
    #[serde(default)]
    pub codex_error: Option<String>,
}

/// The device-code prompt returned when `sbx login` (Docker's OAuth 2.0 device
/// authorization flow) begins: the user opens `url` and confirms `code`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxLoginPrompt {
    pub code: String,
    pub url: String,
}
