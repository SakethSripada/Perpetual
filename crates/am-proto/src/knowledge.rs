use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A project knowledge document (markdown body). Long-form reference material
/// the team and agents can read and search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDoc {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input payload for creating a knowledge document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewKnowledgeDoc {
    pub project_id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// Partial update payload for a knowledge document. `None` fields are unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeDocUpdate {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}
