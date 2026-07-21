//! Repository-layer tests against a real (in-memory) SQLite with migrations and
//! foreign keys applied. Covers CRUD edge cases, enum persistence, agent
//! availability transitions, transcript ordering, and cascade deletes that the
//! service-level tests don't exercise directly.

use am_db::repos::{
    agent, agent_thread, agent_thread_message, agent_thread_repo, agent_turn, event, knowledge,
    memory, message, project, queued_turn, repo, session, task, task_budget_state, task_repo,
};
use am_db::Db;
use am_proto::*;

async fn db() -> Db {
    Db::connect_in_memory().await.unwrap()
}

async fn a_project(db: &Db) -> Project {
    project::create(
        &db.pool,
        NewProject {
            name: "P".into(),
            description: None,
        },
    )
    .await
    .unwrap()
}

async fn a_task(db: &Db, project_id: &str) -> Task {
    task::create(
        &db.pool,
        NewTask {
            project_id: project_id.to_string(),
            title: "T".into(),
            repo_id: None,
            description: Some("desc".into()),
            priority: TaskPriority::High,
            primary_agent: None,
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn task_defaults_to_draft_and_partial_update_preserves_fields() {
    let db = db().await;
    let p = a_project(&db).await;
    let t = a_task(&db, &p.id).await;
    assert_eq!(t.status, TaskStatus::Draft);
    assert_eq!(t.priority, TaskPriority::High);

    // Update only status; title/description/priority must be preserved.
    let updated = task::update(
        &db.pool,
        &t.id,
        TaskUpdate {
            status: Some(TaskStatus::Running),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(updated.status, TaskStatus::Running);
    assert_eq!(updated.title, "T");
    assert_eq!(updated.description.as_deref(), Some("desc"));
    assert_eq!(updated.priority, TaskPriority::High);
    assert!(updated.updated_at >= t.updated_at);

    // Updating a missing task is NotFound.
    assert!(matches!(
        task::update(&db.pool, "missing", TaskUpdate::default()).await,
        Err(am_db::DbError::NotFound)
    ));
}

#[tokio::test]
async fn list_for_status_orders_oldest_first_and_limits() {
    let db = db().await;
    let p = a_project(&db).await;
    let a = a_task(&db, &p.id).await;
    let b = a_task(&db, &p.id).await;
    for id in [&a.id, &b.id] {
        task::update(
            &db.pool,
            id,
            TaskUpdate {
                status: Some(TaskStatus::Queued),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    let queued = task::list_for_status(&db.pool, TaskStatus::Queued, 10)
        .await
        .unwrap();
    assert_eq!(queued.len(), 2);
    let limited = task::list_for_status(&db.pool, TaskStatus::Queued, 1)
        .await
        .unwrap();
    assert_eq!(limited.len(), 1);
}

#[tokio::test]
async fn repo_kind_persists_and_lookup_by_remote_works() {
    let db = db().await;
    let p = a_project(&db).await;
    let local = repo::create_local(&db.pool, &p.id, "local", "/tmp/x", "main")
        .await
        .unwrap();
    let gh = repo::create_github(
        &db.pool,
        &p.id,
        "gh",
        "/tmp/gh",
        "https://github.com/o/r.git",
        "main",
    )
    .await
    .unwrap();
    assert_eq!(local.kind, RepoKind::Local);
    assert_eq!(gh.kind, RepoKind::GitHub);

    // GitHub stored as "github" and parses back (guards the rename fix).
    let reloaded = repo::get(&db.pool, &gh.id).await.unwrap().unwrap();
    assert_eq!(reloaded.kind, RepoKind::GitHub);

    let by_remote = repo::get_by_project_remote(&db.pool, &p.id, "https://github.com/o/r.git")
        .await
        .unwrap();
    assert_eq!(by_remote.unwrap().id, gh.id);

    let all = repo::list_for_project(&db.pool, &p.id).await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn task_repo_replace_and_upsert_worktree() {
    let db = db().await;
    let p = a_project(&db).await;
    let t = a_task(&db, &p.id).await;
    let r1 = repo::create_local(&db.pool, &p.id, "r1", "/tmp/r1", "main")
        .await
        .unwrap();
    let r2 = repo::create_local(&db.pool, &p.id, "r2", "/tmp/r2", "main")
        .await
        .unwrap();

    task_repo::replace_repo(&db.pool, &t.id, &r1.id)
        .await
        .unwrap();
    let link = task_repo::get_for_task(&db.pool, &t.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(link.repo_id, r1.id);
    assert!(link.worktree_path.is_none());

    // Upsert fills in the worktree + base_ref.
    task_repo::upsert(
        &db.pool,
        &task_repo::TaskRepoLink {
            task_id: t.id.clone(),
            repo_id: r1.id.clone(),
            worktree_path: Some("/wt".into()),
            branch: Some("am/feature".into()),
            base_ref: Some("abc123".into()),
            workspace_backend: ExecutionBackend::Host,
        },
    )
    .await
    .unwrap();
    let link = task_repo::get_for_task(&db.pool, &t.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(link.base_ref.as_deref(), Some("abc123"));
    assert_eq!(link.branch.as_deref(), Some("am/feature"));
    assert_eq!(link.workspace_backend, ExecutionBackend::Host);

    // Replacing with another repo drops the old link entirely.
    task_repo::replace_repo(&db.pool, &t.id, &r2.id)
        .await
        .unwrap();
    let link = task_repo::get_for_task(&db.pool, &t.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(link.repo_id, r2.id);
    assert!(link.worktree_path.is_none());
}

#[tokio::test]
async fn agent_thread_repos_turns_messages_and_queue_roundtrip() {
    let db = db().await;
    let p = a_project(&db).await;
    let r1 = repo::create_local(&db.pool, &p.id, "r1", "/tmp/r1", "main")
        .await
        .unwrap();
    let r2 = repo::create_local(&db.pool, &p.id, "r2", "/tmp/r2", "main")
        .await
        .unwrap();

    let thread = agent_thread::create(
        &db.pool,
        NewAgentThread {
            project_id: Some(p.id.clone()),
            group_id: None,
            title: "Build the thing".into(),
            objective: Some("Implement the flow".into()),
            repo_ids: vec![],
            preferred_agent: Some(AgentKind::ClaudeCode),
            permission: Some("autonomous".into()),
            execution_backend: Some(ExecutionBackend::DockerSandbox),
            model: Some("opus".into()),
            reasoning: Some("high".into()),
            local_provider: None,
            local_base_url: None,
            task_budget: Some(TaskBudget::WeeklyPercent { limit_percent: 5 }),
            sort_order: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(thread.status, TaskStatus::Draft);
    assert_eq!(thread.permission, "autonomous");
    assert_eq!(thread.execution_backend, ExecutionBackend::DockerSandbox);
    assert_eq!(thread.model.as_deref(), Some("opus"));
    assert_eq!(thread.reasoning.as_deref(), Some("high"));
    assert_eq!(
        thread.task_budget,
        TaskBudget::WeeklyPercent { limit_percent: 5 }
    );

    let private_state = serde_json::json!({
        "weekly_baseline_percent": 42.0,
        "weekly_consumed_percent": 1.5,
        "reminder_sent": true
    });
    task_budget_state::save(&db.pool, &thread.id, &private_state)
        .await
        .unwrap();
    assert_eq!(
        task_budget_state::get(&db.pool, &thread.id).await.unwrap(),
        private_state
    );

    let group = agent_thread::create_group(
        &db.pool,
        NewWorkbenchSessionGroup {
            project_id: Some(p.id.clone()),
            name: "Checkout".into(),
            color: Some("teal".into()),
            sort_order: Some(1),
        },
    )
    .await
    .unwrap();
    let grouped = agent_thread::assign_group(&db.pool, &thread.id, Some(&group.id))
        .await
        .unwrap();
    assert_eq!(grouped.group_id.as_deref(), Some(group.id.as_str()));
    let groups = agent_thread::list_groups(&db.pool, Some(&p.id))
        .await
        .unwrap();
    assert_eq!(groups.len(), 1);

    let updated_thread = agent_thread::update(
        &db.pool,
        &thread.id,
        AgentThreadUpdate {
            model: Some("".into()),
            reasoning: Some("max".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(updated_thread.model.is_none());
    assert_eq!(updated_thread.reasoning.as_deref(), Some("max"));

    agent_thread_repo::replace_repos(&db.pool, &thread.id, &[r1.id.clone(), r2.id.clone()])
        .await
        .unwrap();
    agent_thread_repo::upsert(
        &db.pool,
        &thread.id,
        &r1.id,
        Some("/wt/r1"),
        Some("am/thread-r1"),
        Some("base1"),
        ExecutionBackend::DockerSandbox,
    )
    .await
    .unwrap();
    let links = agent_thread_repo::list_for_thread(&db.pool, &thread.id)
        .await
        .unwrap();
    assert_eq!(links.len(), 2);
    assert!(links
        .iter()
        .any(|link| link.repo_id == r1.id && link.base_ref.as_deref() == Some("base1")));
    assert!(links.iter().any(|link| {
        link.repo_id == r1.id && link.workspace_backend == ExecutionBackend::DockerSandbox
    }));

    let turn = agent_turn::create(
        &db.pool,
        &thread.id,
        AgentKind::Codex,
        "workspace_write",
        ExecutionBackend::DockerSandbox,
        Some("perpetual-test"),
        Some("gpt-oss:20b"),
        Some("medium"),
        Some(LocalModelProviderKind::Ollama),
        Some("http://127.0.0.1:11434"),
        ModelTargetKind::LocalProvider,
        None,
        None,
        None,
        None,
        Some("hash-local"),
        None,
    )
    .await
    .unwrap();
    agent_turn::set_agent_session_id(&db.pool, &turn.id, "thread-123")
        .await
        .unwrap();
    agent_turn::finish(&db.pool, &turn.id, SessionState::Completed)
        .await
        .unwrap();
    let turns = agent_turn::list_for_thread(&db.pool, &thread.id)
        .await
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].agent_session_id.as_deref(), Some("thread-123"));
    assert_eq!(turns[0].state, SessionState::Completed);
    assert_eq!(turns[0].execution_backend, ExecutionBackend::DockerSandbox);
    assert_eq!(turns[0].sandbox_name.as_deref(), Some("perpetual-test"));
    assert_eq!(turns[0].model.as_deref(), Some("gpt-oss:20b"));
    assert_eq!(
        turns[0].local_provider,
        Some(LocalModelProviderKind::Ollama)
    );
    assert_eq!(turns[0].target_hash.as_deref(), Some("hash-local"));

    agent_thread_message::insert(
        &db.pool,
        &AgentThreadEvent {
            id: "ev1".into(),
            thread_id: thread.id.clone(),
            turn_id: turn.id.clone(),
            role: "assistant".into(),
            kind: "assistant_text".into(),
            text: Some("Done".into()),
            client_message_id: None,
            data: serde_json::json!({ "ok": true }),
            ts: now(),
        },
    )
    .await
    .unwrap();
    let events = agent_thread_message::list_for_thread(&db.pool, &thread.id)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].text.as_deref(), Some("Done"));

    agent_thread_message::upsert(
        &db.pool,
        &AgentThreadEvent {
            id: "ev1".into(),
            thread_id: thread.id.clone(),
            turn_id: turn.id.clone(),
            role: "assistant".into(),
            kind: "assistant_text".into(),
            text: Some("Done streaming".into()),
            client_message_id: None,
            data: serde_json::json!({ "streaming": false }),
            ts: now(),
        },
    )
    .await
    .unwrap();
    let events = agent_thread_message::list_for_thread(&db.pool, &thread.id)
        .await
        .unwrap();
    assert_eq!(events.len(), 1, "streaming updates keep one row");
    assert_eq!(events[0].text.as_deref(), Some("Done streaming"));

    let first = queued_turn::enqueue(
        &db.pool,
        &thread.id,
        AgentKind::ClaudeCode,
        "workspace_write",
        "first",
        None,
    )
    .await
    .unwrap();
    let second = queued_turn::enqueue(
        &db.pool,
        &thread.id,
        AgentKind::Codex,
        "read_only",
        "second",
        None,
    )
    .await
    .unwrap();
    assert_ne!(first.id, second.id);
    let popped = queued_turn::pop_next(&db.pool, &thread.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(popped.message, "first");
    assert!(popped.echo_user_message);
    let remaining = queued_turn::list_for_thread(&db.pool, &thread.id)
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].message, "second");
    assert!(remaining[0].echo_user_message);

    let silent = queued_turn::enqueue_with_echo(
        &db.pool,
        &thread.id,
        AgentKind::ClaudeCode,
        "workspace_write",
        "silent",
        None,
        false,
        Some("client-silent"),
    )
    .await
    .unwrap();
    assert!(!silent.echo_user_message);
    assert_eq!(silent.client_message_id.as_deref(), Some("client-silent"));
    let listed = queued_turn::list_for_thread(&db.pool, &thread.id)
        .await
        .unwrap();
    assert!(listed
        .iter()
        .any(|turn| turn.message == "silent" && !turn.echo_user_message));

    queued_turn::enqueue(
        &db.pool,
        &thread.id,
        AgentKind::ClaudeCode,
        "read_only",
        "parallel-a",
        None,
    )
    .await
    .unwrap();
    queued_turn::enqueue(
        &db.pool,
        &thread.id,
        AgentKind::Codex,
        "read_only",
        "parallel-b",
        None,
    )
    .await
    .unwrap();
    let (left, right) = tokio::join!(
        queued_turn::pop_next(&db.pool, &thread.id),
        queued_turn::pop_next(&db.pool, &thread.id),
    );
    let left = left.unwrap().unwrap();
    let right = right.unwrap().unwrap();
    assert_ne!(
        left.id, right.id,
        "concurrent queue claims duplicated a turn"
    );

    // An interrupted turn is reinserted ahead of follow-ups that arrived
    // while it was running. The persisted order, not insertion timing, must
    // determine the next claim.
    let interrupted = queued_turn::enqueue_with_echo(
        &db.pool,
        &thread.id,
        AgentKind::ClaudeCode,
        "workspace_write",
        "interrupted original",
        None,
        false,
        Some("client-original"),
    )
    .await
    .unwrap();
    let followup = queued_turn::enqueue(
        &db.pool,
        &thread.id,
        AgentKind::Codex,
        "workspace_write",
        "later follow-up",
        None,
    )
    .await
    .unwrap();
    let mut ordered_ids = vec![interrupted.id.clone()];
    ordered_ids.extend(
        queued_turn::list_for_thread(&db.pool, &thread.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|turn| turn.id != interrupted.id)
            .map(|turn| turn.id),
    );
    queued_turn::reorder(&db.pool, &thread.id, &ordered_ids)
        .await
        .unwrap();
    let popped = queued_turn::pop_next(&db.pool, &thread.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(popped.id, interrupted.id);
    assert_eq!(popped.client_message_id.as_deref(), Some("client-original"));
    assert!(!popped.echo_user_message);
    assert!(queued_turn::list_for_thread(&db.pool, &thread.id)
        .await
        .unwrap()
        .iter()
        .any(|turn| turn.id == followup.id));
}

#[tokio::test]
async fn agent_availability_transitions() {
    let db = db().await;
    let reset = now() + chrono::Duration::hours(2);

    // Limit then make available; install_status/version carry across.
    agent::upsert(
        &db.pool,
        &agent::AgentRecord {
            kind: AgentKind::ClaudeCode,
            install_status: "installed".into(),
            version: Some("1.2.3".into()),
            availability: AvailabilityState::Available,
            reset_at: None,
            last_checked: Some(now()),
            limit_strikes: 0,
        },
    )
    .await
    .unwrap();

    let limited = agent::mark_limited(&db.pool, AgentKind::ClaudeCode, Some(reset), 0)
        .await
        .unwrap();
    assert_eq!(limited.availability, AvailabilityState::Limited);
    assert!(limited.reset_at.is_some());
    assert_eq!(limited.version.as_deref(), Some("1.2.3"));

    let available = agent::mark_available(&db.pool, AgentKind::ClaudeCode)
        .await
        .unwrap();
    assert_eq!(available.availability, AvailabilityState::Available);
    assert!(available.reset_at.is_none());
    assert_eq!(available.version.as_deref(), Some("1.2.3"));
}

#[tokio::test]
async fn session_lifecycle_and_transcript_ordering() {
    let db = db().await;
    let p = a_project(&db).await;
    let t = a_task(&db, &p.id).await;

    let s = session::create(
        &db.pool,
        &t.id,
        AgentKind::ClaudeCode,
        ExecutionBackend::Host,
        None,
        None,
        None,
        None,
        None,
        ModelTargetKind::FrontierDefault,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(s.state, SessionState::Running);

    session::set_agent_session_id(&db.pool, &s.id, "prov-123")
        .await
        .unwrap();
    session::finish(&db.pool, &s.id, SessionState::Completed)
        .await
        .unwrap();
    let reloaded = session::get(&db.pool, &s.id).await.unwrap().unwrap();
    assert_eq!(reloaded.state, SessionState::Completed);
    assert_eq!(reloaded.agent_session_id.as_deref(), Some("prov-123"));
    assert_eq!(reloaded.execution_backend, ExecutionBackend::Host);
    assert!(reloaded.local_provider.is_none());
    assert!(reloaded.ended_at.is_some());

    // Two messages; list comes back in insertion order.
    for (i, kind) in ["session_started", "assistant_text"].iter().enumerate() {
        message::insert(
            &db.pool,
            &SessionEvent {
                id: format!("m{i}"),
                session_id: s.id.clone(),
                task_id: t.id.clone(),
                role: "assistant".into(),
                kind: kind.to_string(),
                text: Some(format!("line {i}")),
                data: serde_json::json!({ "n": i }),
                ts: now(),
            },
        )
        .await
        .unwrap();
    }
    let events = message::list_for_session(&db.pool, &s.id, &t.id)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, "session_started");
    assert_eq!(events[1].text.as_deref(), Some("line 1"));
    assert_eq!(events[1].data["n"], serde_json::json!(1));
    assert_eq!(events[0].task_id, t.id);
}

#[tokio::test]
async fn activity_is_scoped_and_newest_first() {
    let db = db().await;
    let p = a_project(&db).await;
    for kind in ["a.one", "a.two", "a.three"] {
        event::record(
            &db.pool,
            NewActivity {
                project_id: Some(p.id.clone()),
                task_id: None,
                kind: kind.into(),
                payload: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    }
    let scoped = event::list_for_project(&db.pool, &p.id, 50).await.unwrap();
    assert_eq!(scoped.len(), 3);
    // Newest first.
    assert_eq!(scoped[0].kind, "a.three");

    let recent = event::list_recent(&db.pool, 2).await.unwrap();
    assert_eq!(recent.len(), 2);
}

#[tokio::test]
async fn delete_session_and_reconcile_orphans() {
    let db = db().await;
    let p = a_project(&db).await;
    let t = a_task(&db, &p.id).await;
    task::update(
        &db.pool,
        &t.id,
        TaskUpdate {
            status: Some(TaskStatus::Running),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let s = session::create(
        &db.pool,
        &t.id,
        AgentKind::ClaudeCode,
        ExecutionBackend::Host,
        None,
        None,
        None,
        None,
        None,
        ModelTargetKind::FrontierDefault,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    message::insert(
        &db.pool,
        &SessionEvent {
            id: "m0".into(),
            session_id: s.id.clone(),
            task_id: t.id.clone(),
            role: "assistant".into(),
            kind: "assistant_text".into(),
            text: Some("hi".into()),
            data: serde_json::Value::Null,
            ts: now(),
        },
    )
    .await
    .unwrap();

    // Orphan reconciliation: leftover running work is queued to resume.
    assert_eq!(
        session::mark_orphans_interrupted(&db.pool).await.unwrap(),
        1
    );
    assert_eq!(task::pause_orphaned_running(&db.pool).await.unwrap(), 1);
    assert_eq!(
        session::get(&db.pool, &s.id).await.unwrap().unwrap().state,
        SessionState::Interrupted
    );
    assert_eq!(
        task::get(&db.pool, &t.id).await.unwrap().unwrap().status,
        TaskStatus::Queued
    );

    // Delete removes the session and its transcript (messages cascade).
    session::delete(&db.pool, &s.id).await.unwrap();
    assert!(session::get(&db.pool, &s.id).await.unwrap().is_none());
    assert!(message::list_for_session(&db.pool, &s.id, &t.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn deleting_a_project_cascades() {
    let db = db().await;
    let p = a_project(&db).await;
    let t = a_task(&db, &p.id).await;
    let r = repo::create_local(&db.pool, &p.id, "r", "/tmp/r", "main")
        .await
        .unwrap();
    task_repo::replace_repo(&db.pool, &t.id, &r.id)
        .await
        .unwrap();
    let s = session::create(
        &db.pool,
        &t.id,
        AgentKind::Codex,
        ExecutionBackend::Host,
        None,
        Some("gpt-oss:20b"),
        Some("medium"),
        Some(LocalModelProviderKind::LmStudio),
        Some("http://127.0.0.1:1234"),
        ModelTargetKind::LocalProvider,
        None,
        None,
        None,
        None,
        Some("hash-lmstudio"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(s.model.as_deref(), Some("gpt-oss:20b"));
    assert_eq!(s.local_provider, Some(LocalModelProviderKind::LmStudio));
    assert_eq!(s.target_hash.as_deref(), Some("hash-lmstudio"));
    knowledge::create(
        &db.pool,
        NewKnowledgeDoc {
            project_id: p.id.clone(),
            title: "d".into(),
            body: "b".into(),
        },
    )
    .await
    .unwrap();
    memory::create(
        &db.pool,
        NewMemoryNote {
            project_id: p.id.clone(),
            task_id: Some(t.id.clone()),
            body: "m".into(),
        },
    )
    .await
    .unwrap();

    project::delete(&db.pool, &p.id).await.unwrap();

    assert!(task::get(&db.pool, &t.id).await.unwrap().is_none());
    assert!(repo::get(&db.pool, &r.id).await.unwrap().is_none());
    assert!(session::get(&db.pool, &s.id).await.unwrap().is_none());
    assert!(task_repo::get_for_task(&db.pool, &t.id)
        .await
        .unwrap()
        .is_none());
    assert!(knowledge::list_for_project(&db.pool, &p.id)
        .await
        .unwrap()
        .is_empty());
    assert!(memory::list_for_task(&db.pool, &t.id)
        .await
        .unwrap()
        .is_empty());
}
