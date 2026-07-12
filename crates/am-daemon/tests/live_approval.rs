//! End-to-end live-approval tests against the REAL `claude` / `codex` CLIs.
//!
//! These drive the full approval round-trip: a real Codex agent runs in Ask or
//! Edit mode over the app-server transport; gated actions publish
//! `AppEvent::ApprovalRequested`; a background approver resolves each one; and
//! we assert the run proceeds to a terminal state. This exercises the in-memory
//! approval registry and the Codex app-server `approvalPolicy` selection.
//!
//! `#[ignore]`d by default (real CLIs, auth, network, subscription cost). Run:
//!
//!   cargo test -p am-daemon --test live_approval -- --ignored --nocapture --test-threads=1
//!
//! Each test auto-skips (passing) when its agent isn't installed/authenticated.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use am_agents::PermissionPolicy;
use am_core::AppCore;
use am_proto::{
    AgentKind, AppEvent, ApprovalDecision, NewLocalRepo, NewProject, NewTask, TaskPriority,
    TaskStatus,
};
use tokio::sync::broadcast::Receiver;

fn nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn tmp(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?} failed");
}

fn dummy_repo() -> PathBuf {
    let dir = tmp("am-live-appr-repo");
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "# Dummy\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "initial"]);
    dir
}

async fn wait_for_session_end(
    rx: &mut Receiver<am_proto::SequencedEvent>,
    task_id: &str,
    secs: u64,
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(am_proto::SequencedEvent {
                event: AppEvent::Session(se),
                ..
            })) if se.task_id == task_id && se.kind == "session_ended" => return true,
            Ok(Ok(_)) => continue,
            Ok(Err(_)) | Err(_) => return false,
        }
    }
}

async fn settle_status(core: &AppCore, task_id: &str) -> TaskStatus {
    for _ in 0..30 {
        let status = core.get_task(task_id).await.unwrap().unwrap().status;
        if status != TaskStatus::Running {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    core.get_task(task_id).await.unwrap().unwrap().status
}

/// Shared body. Runs `agent` in `permission` mode on a task that forces a shell
/// command, while a background approver allows every pending approval. Asserts
/// the run reaches a terminal state, and (when `expect_prompt`) that at least one
/// approval actually surfaced through the registry.
async fn run_live_approval(agent: AgentKind, permission: PermissionPolicy, expect_prompt: bool) {
    let core = AppCore::new(&tmp("am-live-appr-data")).await.unwrap();

    match core
        .detect_agents()
        .await
        .unwrap()
        .iter()
        .find(|a| a.kind == agent)
    {
        Some(s) if s.installed && s.authenticated => {}
        other => {
            eprintln!("SKIP: {} not ready: {other:?}", agent.label());
            return;
        }
    }

    let repo_path = dummy_repo();
    let project = core
        .create_project(NewProject {
            name: "LiveApproval".into(),
            description: None,
        })
        .await
        .unwrap();
    let repo = core
        .connect_local_repo(NewLocalRepo {
            project_id: project.id.clone(),
            path: repo_path.to_string_lossy().to_string(),
        })
        .await
        .unwrap();
    let task = core
        .create_task(NewTask {
            project_id: project.id.clone(),
            title: "Run a shell command".into(),
            repo_id: Some(repo.id.clone()),
            description: Some(
                "Use ONLY the shell/Bash tool (do NOT use Write, Edit, or any file tool). \
                 Run exactly this single command and report its output, then stop: \
                 python3 -c 'print(\"AgentManager-OK\")'"
                    .into(),
            ),
            priority: TaskPriority::Medium,
            primary_agent: Some(agent),
            ..Default::default()
        })
        .await
        .unwrap();

    // Background approver: poll the registry and allow everything until the run
    // ends. Polling (not the broadcast) so we can never miss a request to a lag.
    let done = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(AtomicUsize::new(0));
    let approver = {
        let core = core.clone();
        let done = done.clone();
        let seen = seen.clone();
        tokio::spawn(async move {
            while !done.load(Ordering::Relaxed) {
                for req in core.list_pending_approvals().await {
                    eprintln!(
                        "  approving: agent={} kind={} tool={} cmd={:?}",
                        req.agent.as_str(),
                        req.kind.as_str(),
                        req.tool_name,
                        req.command
                    );
                    let _ = core
                        .resolve_approval(&req.id, ApprovalDecision::Allow)
                        .await;
                    seen.fetch_add(1, Ordering::Relaxed);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    };

    let mut rx = core.events.subscribe();
    core.run_task(&task.id, agent, permission)
        .await
        .expect("run_task should start a session");

    let ended = wait_for_session_end(&mut rx, &task.id, 300).await;
    done.store(true, Ordering::Relaxed);
    let _ = approver.await;
    assert!(ended, "{} {permission:?} run did not end", agent.label());

    let status = settle_status(&core, &task.id).await;
    let count = seen.load(Ordering::Relaxed);
    let events = core.list_session_events(&task.id).await.unwrap();
    let kinds: Vec<String> = events
        .iter()
        .map(|e| match e.text.as_deref() {
            Some(t) if !t.is_empty() => {
                format!("{}({})", e.kind, t.chars().take(40).collect::<String>())
            }
            _ => e.kind.clone(),
        })
        .collect();
    eprintln!(
        "RESULT {} {permission:?}: {count} approval(s) surfaced, terminal status {status:?}\n  transcript: {kinds:?}",
        agent.label()
    );

    // A usage limit is an acceptable outcome (it still exercised the pipeline).
    if status == TaskStatus::WaitingForLimit {
        eprintln!("OK {} {permission:?}: usage-limited", agent.label());
        let _ = std::fs::remove_dir_all(&repo_path);
        return;
    }

    if expect_prompt {
        assert!(
            count > 0,
            "{} {permission:?}: expected at least one live approval, saw none (status {status:?})",
            agent.label()
        );
        // Having approved the command, the agent should have actually run it.
        assert!(
            kinds.iter().any(|k| k.starts_with("tool_use")),
            "{} {permission:?}: approved but no tool_use in transcript: {kinds:?}",
            agent.label()
        );
    }

    eprintln!("OK {} {permission:?}", agent.label());
    let _ = std::fs::remove_dir_all(&repo_path);
}

#[tokio::test]
#[ignore = "requires the real codex CLI; run with --ignored"]
async fn codex_ask_mode_prompts() {
    // `untrusted` policy: Codex asks before every command.
    run_live_approval(AgentKind::Codex, PermissionPolicy::Ask, true).await;
}

#[tokio::test]
#[ignore = "requires the real codex CLI; run with --ignored"]
async fn codex_edit_mode_runs_over_app_server() {
    // `on-request` policy: a plain in-workspace command need not escalate, so we
    // only assert the app-server transport drives the run to completion.
    run_live_approval(AgentKind::Codex, PermissionPolicy::WorkspaceWrite, false).await;
}
