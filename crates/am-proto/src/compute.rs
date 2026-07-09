use serde::{Deserialize, Serialize};

/// Persisted provider discriminator kept for compatibility with task/thread
/// rows that were created by the larger AgentManager app.
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

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "vast" | "vast_ai" => Self::Vast,
            "runpod" | "run_pod" => Self::Runpod,
            "lambda" | "lambda_cloud" => Self::Lambda,
            _ => return None,
        })
    }
}

/// Model target selected for an agent run. The VS Code extension supports
/// frontier defaults and local providers; `RentedCompute` remains parsable so
/// stale rows fail gracefully at launch time instead of corrupting reads.
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

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "frontier_default" | "default" | "cloud" => Self::FrontierDefault,
            "local_provider" | "local" => Self::LocalProvider,
            "rented_compute" | "rented" | "remote_model" => Self::RentedCompute,
            _ => return None,
        })
    }
}
