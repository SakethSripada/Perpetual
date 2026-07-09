use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Current GitHub OAuth connection state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubAuthStatus {
    pub configured: bool,
    pub authenticated: bool,
    pub login: Option<String>,
    pub avatar_url: Option<String>,
    pub error: Option<String>,
}

/// Device-flow codes returned by GitHub for browser-based authorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubDeviceFlow {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_at: DateTime<Utc>,
    pub interval_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubDevicePollState {
    Pending,
    SlowDown,
    Authorized,
    Expired,
    Denied,
    Error,
}

/// Result of polling GitHub for device-flow authorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubDevicePoll {
    pub state: GithubDevicePollState,
    pub status: Option<GithubAuthStatus>,
    pub interval_seconds: Option<u64>,
    pub error: Option<String>,
}

/// A GitHub repository visible to the authenticated user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubRepository {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    pub private: bool,
    pub html_url: String,
    pub clone_url: String,
    pub ssh_url: String,
    pub default_branch: String,
    pub updated_at: Option<DateTime<Utc>>,
}

/// A GitHub pull request opened for a task branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubPullRequest {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub head: String,
    pub base: String,
    pub head_sha: String,
}
