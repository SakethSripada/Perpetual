//! Repository functions grouped by aggregate. Each takes a `&SqlitePool`.

pub mod agent;
pub mod agent_thread;
pub mod agent_thread_message;
pub mod agent_thread_repo;
pub mod agent_turn;
pub mod cloud_run;
pub mod event;
pub mod knowledge;
pub mod memory;
pub mod message;
pub mod project;
pub mod queued_turn;
pub mod repo;
pub mod search;
pub mod session;
pub mod settings;
pub mod task;
pub mod task_context;
pub mod task_repo;
pub mod work_graph;
