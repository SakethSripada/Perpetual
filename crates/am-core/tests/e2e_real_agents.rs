//! End-to-end tests that drive the FULL stack against the real, locally
//! installed `claude` / `codex` / Cursor CLIs and Docker `sbx` sandboxes:
//! detection → spawn → stream-json parse → normalized events → isolated
//! worktree/clone → diff → terminal task state.
//!
//! These are `#[ignore]`d by default because they require the CLIs (auth +
//! network + real subscription cost) and take seconds-to-minutes. Run with:
//!
//!   cargo test -p am-core --test e2e_real_agents -- --ignored --nocapture --test-threads=1
//!
//! Run them serially (`--test-threads=1`): each spawns a real agent process, so
//! parallel runs just contend for CPU and the subscription. Each test
//! auto-skips (passing) if its agent isn't installed/authenticated, so the
//! suite stays safe on machines without the CLIs. A run that hits a real usage
//! limit is treated as a valid outcome — it exercises the limit-detection path.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use am_agents::PermissionPolicy;
use am_core::AppCore;
use am_proto::{
    AgentKind, AppEvent, ExecutionBackend, NewLocalRepo, NewProject, NewTask, TaskPriority,
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
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("{prefix}-{}-{}-{seq}", std::process::id(), nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Files the orchestrator renders into every worktree (the agent-independent
/// context). They show up in a diff regardless of what the agent does, so
/// "no agent changes" means the diff contains only these.
const CONTEXT_FILES: &[&str] = &["TASK_CONTEXT.md", "CLAUDE.md", "AGENTS.md"];

fn is_context_file(path: &str) -> bool {
    CONTEXT_FILES.iter().any(|c| path.ends_with(c))
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
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn sbx_owned_names() -> Vec<String> {
    let output = Command::new("sbx").args(["ls", "--quiet"]).output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with("agentmanager-"))
        .map(ToOwned::to_owned)
        .collect()
}

/// A throwaway git repo with one committed file, so a worktree can branch off it.
fn dummy_repo() -> PathBuf {
    let dir = tmp("am-e2e-repo");
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "# Dummy project\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "initial"]);
    dir
}

/// Wait for the given task's `session_ended` event, or time out.
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
    // The status update is published just before session_ended; give the consume
    // loop a brief moment to persist it.
    for _ in 0..30 {
        let status = core.get_task(task_id).await.unwrap().unwrap().status;
        if status != TaskStatus::Running {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    core.get_task(task_id).await.unwrap().unwrap().status
}

/// Shared body: connect a dummy repo, run a file-creation task on `agent`, and
/// assert the full pipeline produced a transcript, a Review state, and a diff.
async fn run_file_creation(agent: AgentKind) {
    let core = AppCore::new(&tmp("am-e2e-data")).await.unwrap();

    let detected = core.detect_agents().await.unwrap();
    let status = detected.iter().find(|a| a.kind == agent);
    match status {
        Some(s) if s.installed && s.authenticated => {}
        other => {
            eprintln!("SKIP: {} not ready: {other:?}", agent.label());
            return;
        }
    }

    let repo_path = dummy_repo();
    let project = core
        .create_project(NewProject {
            name: "E2E".into(),
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
            title: "Create HELLO.txt".into(),
            repo_id: Some(repo.id.clone()),
            description: Some(
                "Create a new file named HELLO.txt at the repository root containing exactly the \
                 line: Hello from AgentManager. Do not modify any other files."
                    .into(),
            ),
            priority: TaskPriority::Medium,
            primary_agent: Some(agent),
            ..Default::default()
        })
        .await
        .unwrap();

    // Subscribe BEFORE running so no events are missed.
    let mut rx = core.events.subscribe();
    core.run_task(&task.id, agent, PermissionPolicy::WorkspaceWrite)
        .await
        .expect("run_task should start a session");

    let ended = wait_for_session_end(&mut rx, &task.id, 300).await;
    assert!(
        ended,
        "{} session did not end within timeout",
        agent.label()
    );

    let status = settle_status(&core, &task.id).await;
    let events = core.list_session_events(&task.id).await.unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();

    // The session was spawned and streamed regardless of outcome.
    assert!(
        kinds.contains(&"session_started"),
        "missing session_started: {kinds:?}"
    );
    assert!(
        kinds.contains(&"session_ended"),
        "missing session_ended: {kinds:?}"
    );

    match status {
        // Happy path: the agent ran, produced text, and made the requested edit
        // in the isolated worktree.
        TaskStatus::Review => {
            assert!(
                kinds.contains(&"assistant_text"),
                "missing assistant_text: {kinds:?}"
            );
            let diff = core.task_diff(&task.id).await.unwrap();
            assert!(
                diff.files.iter().any(|f| f.path.ends_with("HELLO.txt")),
                "expected HELLO.txt in the worktree diff, got: {:?}",
                diff.files
            );
            eprintln!(
                "OK {}: completed — {} events, {} changed file(s)",
                agent.label(),
                events.len(),
                diff.files.len()
            );
        }
        // The agent's subscription is usage-limited: this still validates the
        // full pipeline AND the limit-detection path (the killer loop).
        TaskStatus::WaitingForLimit => {
            assert!(
                kinds.contains(&"usage_limit"),
                "WaitingForLimit without a usage_limit event: {kinds:?}"
            );
            eprintln!(
                "OK {}: usage limit correctly detected end-to-end",
                agent.label()
            );
        }
        other => panic!("unexpected terminal status {other:?}; transcript: {kinds:?}"),
    }
    let _ = std::fs::remove_dir_all(&repo_path);
}

#[tokio::test]
#[ignore = "requires the real claude CLI; run with --ignored"]
async fn claude_creates_a_file_end_to_end() {
    run_file_creation(AgentKind::ClaudeCode).await;
}

#[tokio::test]
#[ignore = "requires the real codex CLI; run with --ignored"]
async fn codex_creates_a_file_end_to_end() {
    run_file_creation(AgentKind::Codex).await;
}

#[tokio::test]
#[ignore = "requires the real Cursor CLI; run with --ignored"]
async fn cursor_creates_a_file_end_to_end() {
    run_file_creation(AgentKind::Cursor).await;
}

/// Shared body: run a read-only Codex task inside a Docker sandbox and assert
/// the full pipeline produced a transcript and a terminal event. The sandbox is
/// expected to persist for resume until app shutdown.
/// Auto-skips (passing) when sbx or the agent isn't ready.
async fn run_docker_sandbox(agent: AgentKind) {
    assert_eq!(
        agent,
        AgentKind::Codex,
        "Docker Sandbox is Codex-only for now"
    );
    let core = AppCore::new(&tmp("am-e2e-data")).await.unwrap();
    let status = core.detect_sandbox_runtime().await.unwrap();
    if !(status.installed && status.authenticated) {
        eprintln!("SKIP: Docker sbx not ready: {status:?}");
        return;
    }
    if !status.codex_authenticated {
        eprintln!("SKIP: Codex sandbox auth not ready: {status:?}");
        return;
    }
    let detected = core.detect_agents().await.unwrap();
    match detected.iter().find(|a| a.kind == agent) {
        Some(s) if s.installed && s.authenticated => {}
        other => {
            eprintln!("SKIP: {} not ready: {other:?}", agent.label());
            return;
        }
    }

    let repo_path = dummy_repo();
    let project = core
        .create_project(NewProject {
            name: "E2E-sandbox".into(),
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
            title: "Sandbox smoke".into(),
            repo_id: Some(repo.id.clone()),
            description: Some(
                "Briefly describe this repository and do not modify any files.".into(),
            ),
            priority: TaskPriority::Low,
            primary_agent: Some(agent),
            ..Default::default()
        })
        .await
        .unwrap();

    let before = sbx_owned_names();
    let mut rx = core.events.subscribe();
    core.run_task_with_backend(
        &task.id,
        agent,
        PermissionPolicy::ReadOnly,
        Some(ExecutionBackend::DockerSandbox),
    )
    .await
    .expect("docker sandbox run should start");

    assert!(
        wait_for_session_end(&mut rx, &task.id, 300).await,
        "{} docker sandbox session did not end within timeout",
        agent.label()
    );

    let status = settle_status(&core, &task.id).await;
    let events = core.list_session_events(&task.id).await.unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert!(
        kinds.contains(&"session_started"),
        "{} missing session_started: {kinds:?}",
        agent.label()
    );
    assert!(
        kinds.contains(&"session_ended"),
        "{} missing session_ended: {kinds:?}",
        agent.label()
    );
    match status {
        // The agent ran and streamed a response back through the sandbox.
        TaskStatus::Review => assert!(
            kinds.contains(&"assistant_text"),
            "{} produced no assistant_text in the sandbox: {kinds:?}",
            agent.label()
        ),
        // A usage limit still exercises the full sandbox pipeline.
        TaskStatus::WaitingForLimit => assert!(
            kinds.contains(&"usage_limit"),
            "{} WaitingForLimit without a usage_limit event: {kinds:?}",
            agent.label()
        ),
        other => panic!(
            "{} unexpected terminal status {other:?}; transcript: {kinds:?}",
            agent.label()
        ),
    }

    // The named sandbox survives the turn so `codex resume` can work across
    // follow-up turns.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after_run = sbx_owned_names();
    assert!(
        after_run.iter().any(|name| !before.contains(name)),
        "{}: expected a persistent app-owned Docker sandbox after run",
        agent.label()
    );
    core.shutdown().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after_shutdown = sbx_owned_names();
    assert_eq!(
        before, after_shutdown,
        "app shutdown should clean up owned sandboxes"
    );
    let _ = std::fs::remove_dir_all(&repo_path);
}

#[tokio::test]
#[ignore = "requires authenticated Docker sbx + codex CLI; run with --ignored"]
async fn codex_docker_sandbox_run_persists_until_shutdown() {
    run_docker_sandbox(AgentKind::Codex).await;
}

#[tokio::test]
#[ignore = "requires the real claude CLI; run with --ignored"]
async fn claude_plan_only_makes_no_changes() {
    let core = AppCore::new(&tmp("am-e2e-data")).await.unwrap();
    let detected = core.detect_agents().await.unwrap();
    match detected.iter().find(|a| a.kind == AgentKind::ClaudeCode) {
        Some(s) if s.installed && s.authenticated => {}
        other => {
            eprintln!("SKIP: claude not ready: {other:?}");
            return;
        }
    }

    let repo_path = dummy_repo();
    let project = core
        .create_project(NewProject {
            name: "E2E-plan".into(),
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
            title: "Summarize the repo".into(),
            repo_id: Some(repo.id.clone()),
            description: Some(
                "Briefly describe what is in this repository. Do not edit anything.".into(),
            ),
            priority: TaskPriority::Low,
            primary_agent: Some(AgentKind::ClaudeCode),
            ..Default::default()
        })
        .await
        .unwrap();

    let mut rx = core.events.subscribe();
    core.run_task(&task.id, AgentKind::ClaudeCode, PermissionPolicy::ReadOnly)
        .await
        .expect("run_task");
    assert!(
        wait_for_session_end(&mut rx, &task.id, 300).await,
        "no session end"
    );

    // Read-only: the agent must not have edited the codebase. The only files in
    // the diff may be the orchestrator-rendered context files.
    let diff = core.task_diff(&task.id).await.unwrap();
    let agent_edits: Vec<&String> = diff
        .files
        .iter()
        .map(|f| &f.path)
        .filter(|p| !is_context_file(p))
        .collect();
    assert!(
        agent_edits.is_empty(),
        "plan-only run produced agent edits: {agent_edits:?}"
    );
    eprintln!("OK Claude Code: plan-only made no code changes");
    let _ = std::fs::remove_dir_all(&repo_path);
}
