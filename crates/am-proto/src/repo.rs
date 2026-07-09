use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::RepoKind;

/// A repository connected to a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub kind: RepoKind,
    /// Canonical working path on disk (the user's repo for `local`).
    pub local_path: Option<String>,
    pub remote_url: Option<String>,
    pub default_branch: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for connecting a local repository by path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewLocalRepo {
    pub project_id: String,
    pub path: String,
}

/// Input for connecting a GitHub repository into an app-managed clone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewGithubRepo {
    pub project_id: String,
    pub name: String,
    pub full_name: String,
    pub clone_url: String,
    pub ssh_url: String,
    pub default_branch: String,
}
