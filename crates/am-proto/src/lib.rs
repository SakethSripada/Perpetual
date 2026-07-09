//! Shared domain types for AgentManager.
//!
//! This crate is intentionally dependency-light (serde only, no DB / no Tauri)
//! so it can be shared across the orchestrator core, the persistence layer, the
//! Tauri shell, and — later — a headless daemon and remote workers.

mod agent_model;
mod agent_thread;
mod approval;
mod cloud;
mod compute;
mod enums;
mod event;
mod git_automation;
mod github;
mod knowledge;
mod limit_policy;
mod local_model;
mod memory;
mod policy;
mod project;
mod repo;
mod sandbox;
mod search;
mod session;
mod task;
mod task_context;
mod work_graph;

pub use agent_model::*;
pub use agent_thread::*;
pub use approval::*;
pub use cloud::*;
pub use compute::*;
pub use enums::*;
pub use event::*;
pub use git_automation::*;
pub use github::*;
pub use knowledge::*;
pub use limit_policy::*;
pub use local_model::*;
pub use memory::*;
pub use policy::*;
pub use project::*;
pub use repo::*;
pub use sandbox::*;
pub use search::*;
pub use session::*;
pub use task::*;
pub use task_context::*;
pub use work_graph::*;

/// Generate a new opaque identifier (UUID v4, stored as TEXT).
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Current wall-clock time in UTC.
pub fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}
