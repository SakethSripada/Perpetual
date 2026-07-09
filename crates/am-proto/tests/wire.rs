//! Wire-format contract tests. These lock the serialized shapes that the
//! hand-mirrored TypeScript types in `src/lib/types.ts` and the EventBridge in
//! `src/app/EventBridge.tsx` depend on. If a rename or tag changes, these fail
//! before the frontend silently breaks.

use am_proto::*;
use serde_json::json;

/// Every enum's `as_str` must round-trip through `parse`, and serde must agree
/// with both (snake_case rename).
macro_rules! assert_enum_contract {
    ($ty:ty, [$($variant:expr => $wire:literal),+ $(,)?]) => {{
        $(
            // as_str <-> parse round-trip
            assert_eq!($variant.as_str(), $wire, concat!(stringify!($ty), " as_str"));
            assert_eq!(<$ty>::parse($wire), Some($variant), concat!(stringify!($ty), " parse"));
            // serde agrees with as_str
            assert_eq!(
                serde_json::to_value($variant).unwrap(),
                json!($wire),
                concat!(stringify!($ty), " serialize")
            );
            assert_eq!(
                serde_json::from_value::<$ty>(json!($wire)).unwrap(),
                $variant,
                concat!(stringify!($ty), " deserialize")
            );
        )+
        // Unknown strings don't parse.
        assert_eq!(<$ty>::parse("definitely-not-a-variant"), None);
    }};
}

#[test]
fn agent_kind_contract() {
    assert_enum_contract!(AgentKind, [
        AgentKind::ClaudeCode => "claude_code",
        AgentKind::Codex => "codex",
        AgentKind::Gemini => "gemini",
        AgentKind::Cursor => "cursor",
        AgentKind::OpenCode => "open_code",
    ]);
    // Labels are stable UI strings.
    assert_eq!(AgentKind::ClaudeCode.label(), "Claude Code");
}

#[test]
fn task_status_contract() {
    assert_enum_contract!(TaskStatus, [
        TaskStatus::Draft => "draft",
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::RunningInCloud => "running_in_cloud",
        TaskStatus::AwaitingApproval => "awaiting_approval",
        TaskStatus::WaitingForLimit => "waiting_for_limit",
        TaskStatus::WaitingForNetwork => "waiting_for_network",
        TaskStatus::Paused => "paused",
        TaskStatus::Review => "review",
        TaskStatus::Done => "done",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    ]);
}

#[test]
fn task_priority_contract() {
    assert_enum_contract!(TaskPriority, [
        TaskPriority::Low => "low",
        TaskPriority::Medium => "medium",
        TaskPriority::High => "high",
        TaskPriority::Urgent => "urgent",
    ]);
    assert_eq!(TaskPriority::default(), TaskPriority::Medium);
}

#[test]
fn repo_kind_contract() {
    assert_enum_contract!(RepoKind, [
        RepoKind::Local => "local",
        RepoKind::GitHub => "github",
    ]);
}

#[test]
fn session_state_contract() {
    assert_enum_contract!(SessionState, [
        SessionState::Running => "running",
        SessionState::Completed => "completed",
        SessionState::Interrupted => "interrupted",
        SessionState::Failed => "failed",
    ]);
}

#[test]
fn availability_state_contract() {
    assert_enum_contract!(AvailabilityState, [
        AvailabilityState::Unknown => "unknown",
        AvailabilityState::Available => "available",
        AvailabilityState::Limited => "limited",
    ]);
}

#[test]
fn execution_backend_contract() {
    assert_enum_contract!(ExecutionBackend, [
        ExecutionBackend::Host => "host",
        ExecutionBackend::DockerSandbox => "docker_sandbox",
    ]);
    assert_eq!(ExecutionBackend::default(), ExecutionBackend::Host);
}

#[test]
fn local_model_provider_contract() {
    assert_enum_contract!(LocalModelProviderKind, [
        LocalModelProviderKind::Ollama => "ollama",
        LocalModelProviderKind::LmStudio => "lm_studio",
    ]);
    assert_eq!(
        LocalModelProviderKind::Ollama.codex_oss_provider(),
        "ollama"
    );
    assert_eq!(
        LocalModelProviderKind::LmStudio.codex_oss_provider(),
        "lmstudio"
    );
}

#[test]
fn app_event_is_tagged_with_type_and_data() {
    // The frontend switches on `ev.type` and reads `ev.data`.
    let task = Task {
        id: "t1".into(),
        project_id: "p1".into(),
        title: "Title".into(),
        description: None,
        status: TaskStatus::Running,
        priority: TaskPriority::High,
        primary_agent: Some(AgentKind::ClaudeCode),
        model: None,
        model_target: ModelTargetKind::FrontierDefault,
        compute_lease_id: None,
        compute_provider: None,
        estimated_compute_cost_usd: None,
        fallback_model_target: None,
        created_at: now(),
        updated_at: now(),
    };
    let value = serde_json::to_value(AppEvent::TaskUpdated(task.clone())).unwrap();
    assert_eq!(value["type"], json!("task_updated"));
    assert_eq!(value["data"]["id"], json!("t1"));
    assert_eq!(value["data"]["status"], json!("running"));
    assert_eq!(value["data"]["primary_agent"], json!("claude_code"));

    // Channel name the UI listens on must not drift.
    assert_eq!(AppEvent::CHANNEL, "am://event");

    // Round-trips back to the same variant.
    let back: AppEvent = serde_json::from_value(value).unwrap();
    assert!(matches!(back, AppEvent::TaskUpdated(t) if t.id == "t1"));
}

#[test]
fn optional_fields_serialize_as_null_not_omitted() {
    // The TS `Task` declares `description: string | null`; serde keeps the key.
    let task = Task {
        id: "t".into(),
        project_id: "p".into(),
        title: "x".into(),
        description: None,
        status: TaskStatus::Draft,
        priority: TaskPriority::Low,
        primary_agent: None,
        model: None,
        model_target: ModelTargetKind::FrontierDefault,
        compute_lease_id: None,
        compute_provider: None,
        estimated_compute_cost_usd: None,
        fallback_model_target: None,
        created_at: now(),
        updated_at: now(),
    };
    let value = serde_json::to_value(&task).unwrap();
    assert!(value.as_object().unwrap().contains_key("description"));
    assert_eq!(value["description"], json!(null));
    assert_eq!(value["primary_agent"], json!(null));
}

#[test]
fn new_task_defaults_apply_from_partial_json() {
    // The frontend may omit priority; serde default must fill Medium.
    let input: NewTask = serde_json::from_value(json!({
        "project_id": "p1",
        "title": "Do the thing"
    }))
    .unwrap();
    assert_eq!(input.priority, TaskPriority::Medium);
    assert!(input.repo_id.is_none());
    assert!(input.primary_agent.is_none());
}
