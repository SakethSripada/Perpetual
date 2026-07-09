use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The primary organizational unit. Holds tasks, repositories, knowledge, and
/// activity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input payload for creating a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewProject {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}
