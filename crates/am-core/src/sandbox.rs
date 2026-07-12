use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use am_agents::{find_binary, RuntimeLimits, SessionRuntime};
use am_proto::{
    AgentKind, ExecutionBackend, SandboxLoginPrompt, SandboxPolicy, SandboxRuntimeStatus,
};
use tokio::sync::Mutex;

use crate::admission::{AdmissionController, AdmissionPermit};
use crate::{AppCore, CoreError};

const SANDBOX_POLICY_KEY: &str = "sandbox_policy";
const SANDBOX_NAME_PREFIX: &str = "perpetual-";
const RUNTIME_RUN_CACHE_TTL: Duration = Duration::from_secs(60);

pub(crate) struct SandboxManager {
    admission: AdmissionController,
    active_names: Mutex<HashSet<String>>,
    runtime_status: Mutex<Option<CachedRuntimeStatus>>,
}

pub(crate) struct SandboxLease {
    pub(crate) name: String,
    _permit: AdmissionPermit,
}

struct CachedRuntimeStatus {
    status: SandboxRuntimeStatus,
    checked_at: Instant,
}

impl SandboxManager {
    pub(crate) fn new(max_concurrent: usize) -> Self {
        Self {
            admission: AdmissionController::new(max_concurrent),
            active_names: Mutex::new(HashSet::new()),
            runtime_status: Mutex::new(None),
        }
    }

    /// Apply the policy's sandbox cap; growing admits nothing retroactively
    /// (sandbox acquisition never queues), shrinking drains naturally.
    pub(crate) fn resize(&self, max_concurrent: usize) {
        self.admission.resize(max_concurrent);
    }

    pub(crate) async fn try_acquire(&self, name: String) -> Result<SandboxLease, CoreError> {
        let permit = self.admission.try_acquire(None).ok_or_else(|| {
            CoreError::Other("maximum concurrent Docker sandboxes reached".into())
        })?;
        self.active_names.lock().await.insert(name.clone());
        Ok(SandboxLease {
            name,
            _permit: permit,
        })
    }

    pub(crate) async fn release(&self, lease: Option<SandboxLease>) {
        if let Some(lease) = lease {
            self.active_names.lock().await.remove(&lease.name);
            drop(lease);
        }
    }

    pub(crate) async fn active_count(&self) -> usize {
        self.active_names.lock().await.len()
    }

    pub(crate) async fn cached_runtime_status(&self) -> Option<SandboxRuntimeStatus> {
        let cached = self.runtime_status.lock().await;
        let cached = cached.as_ref()?;
        if cached.checked_at.elapsed() > RUNTIME_RUN_CACHE_TTL {
            return None;
        }
        if !cached.status.installed
            || !cached.status.authenticated
            || !cached.status.codex_authenticated
        {
            return None;
        }
        Some(cached.status.clone())
    }

    pub(crate) async fn remember_runtime_status(&self, status: SandboxRuntimeStatus) {
        *self.runtime_status.lock().await = Some(CachedRuntimeStatus {
            status,
            checked_at: Instant::now(),
        });
    }

    pub(crate) async fn clear_runtime_status(&self) {
        *self.runtime_status.lock().await = None;
    }
}

impl AppCore {
    pub async fn get_sandbox_policy(&self) -> Result<SandboxPolicy, CoreError> {
        let Some(raw) = am_db::repos::settings::get(&self.db.pool, SANDBOX_POLICY_KEY).await?
        else {
            return Ok(SandboxPolicy::default());
        };
        serde_json::from_str(&raw).map_err(|e| CoreError::Other(e.to_string()))
    }

    pub async fn set_sandbox_policy(
        &self,
        policy: SandboxPolicy,
    ) -> Result<SandboxPolicy, CoreError> {
        let normalized = normalize_policy(policy);
        let value =
            serde_json::to_string(&normalized).map_err(|e| CoreError::Other(e.to_string()))?;
        am_db::repos::settings::set(&self.db.pool, SANDBOX_POLICY_KEY, &value).await?;
        self.sandboxes.resize(normalized.max_concurrent_sandboxes);
        Ok(normalized)
    }

    pub async fn detect_sandbox_runtime(&self) -> Result<SandboxRuntimeStatus, CoreError> {
        self.sandbox_runtime_status(false).await
    }

    async fn sandbox_runtime_status(
        &self,
        allow_cached: bool,
    ) -> Result<SandboxRuntimeStatus, CoreError> {
        if allow_cached {
            if let Some(mut cached) = self.sandboxes.cached_runtime_status().await {
                cached.active_count = self.sandboxes.active_count().await;
                return Ok(cached);
            }
        }

        let binary = tokio::task::spawn_blocking(|| find_binary("sbx"))
            .await
            .ok()
            .flatten();
        let Some(binary) = binary else {
            let status = SandboxRuntimeStatus {
                installed: false,
                authenticated: false,
                codex_authenticated: false,
                version: None,
                binary_path: None,
                active_count: self.sandboxes.active_count().await,
                error: Some("sbx CLI not found".to_string()),
                codex_error: None,
            };
            self.sandboxes.remember_runtime_status(status.clone()).await;
            return Ok(status);
        };

        let binary_for_version = binary.clone();
        let version =
            tokio::task::spawn_blocking(move || am_agents::binary_version(&binary_for_version))
                .await
                .ok()
                .flatten();
        let binary_for_probe = binary.clone();
        let probe = tokio::task::spawn_blocking(move || probe_sbx(&binary_for_probe))
            .await
            .unwrap_or_default();
        let binary_for_codex = binary.clone();
        let codex_authenticated =
            tokio::task::spawn_blocking(move || codex_sandbox_authenticated(&binary_for_codex))
                .await
                .unwrap_or(false);

        // Distinguish the two failure modes the user can actually act on: the
        // sandboxd daemon being down vs. not being signed in to Docker.
        let error = if !probe.daemon_running {
            Some(
                "Docker sandbox daemon isn't running — open Docker Desktop, then retry".to_string(),
            )
        } else if !probe.authenticated {
            Some("sbx is running but not signed in to Docker — run `sbx login`".to_string())
        } else {
            None
        };
        let codex_error = if probe.authenticated && !codex_authenticated {
            Some(
                "Codex is not signed in for Docker Sandboxes — run `sbx secret set -g openai --oauth`"
                    .to_string(),
            )
        } else {
            None
        };

        let status = SandboxRuntimeStatus {
            installed: true,
            authenticated: probe.authenticated,
            codex_authenticated,
            version,
            binary_path: Some(binary.to_string_lossy().to_string()),
            active_count: self.sandboxes.active_count().await,
            error,
            codex_error,
        };
        self.sandboxes.remember_runtime_status(status.clone()).await;
        Ok(status)
    }

    /// Begin Docker's device-code sign-in (`sbx login`). Returns the one-time
    /// code and activation URL as soon as `sbx` prints them, opens that URL in
    /// the user's browser, and lets the login finish in the background — the
    /// caller's existing readiness polling flips to authenticated on success.
    pub async fn sandbox_login(&self) -> Result<SandboxLoginPrompt, CoreError> {
        use tokio::io::AsyncBufReadExt;

        let binary = tokio::task::spawn_blocking(|| find_binary("sbx"))
            .await
            .ok()
            .flatten()
            .ok_or_else(|| CoreError::Other("sbx CLI not found".into()))?;
        self.sandboxes.clear_runtime_status().await;
        // Every sbx command needs the daemon; start it before logging in.
        let binary_for_daemon = binary.clone();
        tokio::task::spawn_blocking(move || ensure_sbx_daemon(&binary_for_daemon))
            .await
            .ok();

        let mut child = tokio::process::Command::new(&binary)
            .arg("login")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| CoreError::Other(format!("failed to start sbx login: {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CoreError::Other("sbx login produced no output".into()))?;
        let mut lines = tokio::io::BufReader::new(stdout).lines();

        // Read the device-code preamble, bounded so a hung login can't block.
        let mut code: Option<String> = None;
        let mut url: Option<String> = None;
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(30));
        tokio::pin!(deadline);
        while url.is_none() {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        if let Some(parsed) = parse_login_code(&line) {
                            code = Some(parsed);
                        }
                        if let Some(parsed) = parse_login_url(&line) {
                            url = Some(parsed);
                        }
                    }
                    _ => break,
                },
                _ = &mut deadline => break,
            }
        }

        let Some(url) = url else {
            let _ = child.start_kill();
            return Err(CoreError::Other(
                "sbx login did not return a sign-in URL".into(),
            ));
        };
        // Prefer the explicit code line; fall back to the user_code in the URL.
        let code = code.or_else(|| code_from_url(&url)).unwrap_or_default();

        open_url(&url);

        // Keep the login process alive until the user finishes approving; drop
        // its remaining output. The readiness poll surfaces the result.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });

        Ok(SandboxLoginPrompt { code, url })
    }

    /// Begin sbx's OpenAI OAuth flow for Codex inside Docker sandboxes. This is
    /// separate from Docker sign-in and from the user's host Codex login.
    pub async fn codex_sandbox_login(&self) -> Result<SandboxLoginPrompt, CoreError> {
        use tokio::io::AsyncBufReadExt;

        let binary = tokio::task::spawn_blocking(|| find_binary("sbx"))
            .await
            .ok()
            .flatten()
            .ok_or_else(|| CoreError::Other("sbx CLI not found".into()))?;
        self.sandboxes.clear_runtime_status().await;
        let binary_for_daemon = binary.clone();
        tokio::task::spawn_blocking(move || ensure_sbx_daemon(&binary_for_daemon))
            .await
            .ok();

        let mut child = tokio::process::Command::new(&binary)
            .args(["secret", "set", "-g", "openai", "--oauth"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| CoreError::Other(format!("failed to start Codex sandbox sign-in: {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CoreError::Other("Codex sandbox sign-in produced no stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| CoreError::Other("Codex sandbox sign-in produced no stderr".into()))?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let stdout_tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = stdout_tx.send(line);
            }
        });
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx.send(line);
            }
        });

        let mut code: Option<String> = None;
        let mut url: Option<String> = None;
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(30));
        tokio::pin!(deadline);
        while url.is_none() {
            tokio::select! {
                line = rx.recv() => match line {
                    Some(line) => {
                        if let Some(parsed) = parse_login_code(&line) {
                            code = Some(parsed);
                        }
                        if let Some(parsed) = parse_login_url(&line) {
                            url = Some(parsed);
                        }
                    }
                    None => break,
                },
                _ = &mut deadline => break,
            }
        }

        let Some(url) = url else {
            let _ = child.start_kill();
            return Err(CoreError::Other(
                "Codex sandbox sign-in did not return a sign-in URL".into(),
            ));
        };
        let code = code.or_else(|| code_from_url(&url)).unwrap_or_default();
        open_url(&url);

        tokio::spawn(async move {
            let _ = child.wait().await;
        });

        Ok(SandboxLoginPrompt { code, url })
    }

    pub(crate) async fn session_runtime(
        &self,
        agent: AgentKind,
        backend: ExecutionBackend,
        sandbox_name: Option<String>,
    ) -> Result<(SessionRuntime, Option<SandboxLease>), CoreError> {
        let policy = self.get_sandbox_policy().await.unwrap_or_default();
        let limits = RuntimeLimits {
            startup_timeout: std::time::Duration::from_secs(120),
            run_timeout: std::time::Duration::from_secs(policy.run_timeout_secs),
            idle_timeout: std::time::Duration::from_secs(policy.idle_timeout_secs),
            stop_grace: std::time::Duration::from_secs(policy.stop_grace_secs),
        };
        match backend {
            ExecutionBackend::Host => Ok((SessionRuntime::Host { limits }, None)),
            // Cloud runs are provider-hosted: they are launched by the cloud
            // handoff engine, never as a local session process.
            ExecutionBackend::Cloud => Err(CoreError::Other(
                "Cloud runs are launched through the cloud handoff, not as a local session".into(),
            )),
            ExecutionBackend::DockerSandbox => {
                if agent != AgentKind::Codex {
                    return Err(CoreError::Other(format!(
                        "{} isn't available in Docker sandboxes yet. Run it on Host, or use Codex in Docker.",
                        agent.label()
                    )));
                }
                let runtime = self.sandbox_runtime_status(true).await?;
                if !runtime.installed {
                    return Err(CoreError::Other(
                        "Docker Sandbox requires the sbx CLI".into(),
                    ));
                }
                if !runtime.authenticated {
                    return Err(CoreError::Other(
                        "Docker Sandbox is installed but not authenticated".into(),
                    ));
                }
                if !runtime.codex_authenticated {
                    return Err(CoreError::Other(
                        "Codex is not signed in for Docker Sandboxes. Sign in with `sbx secret set -g openai --oauth`, then retry.".into(),
                    ));
                }
                // The admission controller enforces the policy cap atomically;
                // resize first so policy edits apply to this acquisition.
                self.sandboxes.resize(policy.max_concurrent_sandboxes);
                let name = sandbox_name.unwrap_or_else(|| sandbox_name_for("run", &uuid_tail()));
                let lease = self.sandboxes.try_acquire(name.clone()).await?;
                Ok((
                    SessionRuntime::DockerSandbox {
                        name,
                        cpus: policy.cpus,
                        memory: policy.memory,
                        network_preset: policy.network_preset,
                        limits,
                    },
                    Some(lease),
                ))
            }
        }
    }

    pub(crate) fn sandbox_name_for(owner: &str, run_id: &str) -> String {
        sandbox_name_for(owner, run_id)
    }

    pub(crate) fn reconcile_stale_sandboxes(&self) {
        reconcile_owned_sandboxes();
    }

    pub(crate) async fn cleanup_task_sandboxes(&self, task_id: &str) {
        let Ok(sessions) = am_db::repos::session::list_for_task(&self.db.pool, task_id).await
        else {
            return;
        };
        cleanup_sandbox_names(
            sessions
                .into_iter()
                .filter_map(|session| session.sandbox_name)
                .collect(),
        )
        .await;
    }

    pub(crate) async fn cleanup_thread_sandboxes(&self, thread_id: &str) {
        let Ok(turns) = am_db::repos::agent_turn::list_for_thread(&self.db.pool, thread_id).await
        else {
            return;
        };
        cleanup_sandbox_names(
            turns
                .into_iter()
                .filter_map(|turn| turn.sandbox_name)
                .collect(),
        )
        .await;
    }
}

/// Best-effort removal of every app-owned (`perpetual-*`) sandbox still
/// present according to `sbx ls`. Used both to clear startup stragglers and to
/// sweep up after session cancellation on shutdown. Touches only names carrying
/// our prefix, never user-managed sandboxes. Blocking — run via
/// `spawn_blocking` from async contexts.
pub(crate) fn reconcile_owned_sandboxes() {
    let Some(binary) = find_binary("sbx") else {
        return;
    };
    let Ok(output) = Command::new(&binary)
        .args(["ls", "--quiet"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    for name in parse_owned_sandbox_names(&output.stdout) {
        let _ = Command::new(&binary)
            .args(["rm", "--force", &name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

async fn cleanup_sandbox_names(names: Vec<String>) {
    let mut names: Vec<String> = names
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|name| name.starts_with(SANDBOX_NAME_PREFIX))
        .collect();
    if names.is_empty() {
        return;
    }
    names.sort();
    let _ = tokio::task::spawn_blocking(move || {
        let Some(binary) = find_binary("sbx") else {
            return;
        };
        for name in names {
            remove_named_sandbox(&binary, &name);
        }
    })
    .await;
}

fn remove_named_sandbox(binary: &std::path::Path, name: &str) {
    let commands: &[&[&str]] = &[&["rm", "--force"], &["rm", "-f"], &["rm"], &["stop"]];
    for prefix in commands {
        let status = Command::new(binary)
            .args(*prefix)
            .arg(name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if matches!(status, Ok(status) if status.success()) {
            am_agents::forget_sandbox(name);
            return;
        }
    }
}

fn normalize_policy(mut policy: SandboxPolicy) -> SandboxPolicy {
    policy.max_concurrent_sandboxes = policy.max_concurrent_sandboxes.clamp(1, 64);
    policy.cpus = policy.cpus.clamp(1, 16);
    if policy.memory.trim().is_empty() {
        policy.memory = "4g".to_string();
    }
    policy.network_preset = normalize_network_preset(&policy.network_preset).to_string();
    policy.run_timeout_secs = policy.run_timeout_secs.clamp(300, 86_400);
    policy.idle_timeout_secs = policy.idle_timeout_secs.clamp(60, policy.run_timeout_secs);
    policy.stop_grace_secs = policy.stop_grace_secs.clamp(3, 300);
    policy
}

#[derive(Default)]
struct SbxProbe {
    daemon_running: bool,
    authenticated: bool,
}

/// Best-effort: make sure the sandboxd daemon is up, then report whether it is
/// running and whether the user is signed in to Docker. Blocking — every sbx
/// command needs the daemon, and Docker Desktop does not start it for us.
fn probe_sbx(binary: &std::path::Path) -> SbxProbe {
    ensure_sbx_daemon(binary);
    let daemon_running = sbx_daemon_running(binary);
    SbxProbe {
        daemon_running,
        authenticated: daemon_running && sbx_authenticated(binary),
    }
}

/// Start the sandboxd daemon if it isn't already running, then poll briefly for
/// it to come up. `sbx daemon start` runs in the foreground, so spawn it
/// detached (its own process group) so app shutdown can't tear the daemon down.
fn ensure_sbx_daemon(binary: &std::path::Path) {
    if sbx_daemon_running(binary) {
        return;
    }
    let mut cmd = Command::new(binary);
    cmd.args(["daemon", "start"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    if cmd.spawn().is_err() {
        return;
    }
    for _ in 0..16 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if sbx_daemon_running(binary) {
            return;
        }
    }
}

/// Extract the device code from a line like
/// `Your one-time device confirmation code is: BTBV-VPRK`.
fn parse_login_code(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("code is:")?;
    let code = rest.trim().trim_end_matches('.').trim();
    (!code.is_empty()).then(|| code.to_string())
}

/// Extract the activation URL from any line containing an https link.
fn parse_login_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    (!url.is_empty()).then_some(url)
}

/// Pull the `user_code` query value out of the activation URL as a fallback.
fn code_from_url(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("user_code=")?;
    let code: String = rest.chars().take_while(|&c| c != '&').collect();
    (!code.is_empty()).then_some(code)
}

/// Open a URL in the user's default browser, best-effort across platforms.
fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let program = "xdg-open";

    let _ = Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// `sbx daemon status` exits 0 whether running or stopped, so parse its output.
fn sbx_daemon_running(binary: &std::path::Path) -> bool {
    let output = Command::new(binary)
        .args(["daemon", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    matches!(output, Ok(o) if String::from_utf8_lossy(&o.stdout).to_lowercase().contains("running"))
}

/// Signed in to Docker. `sbx ls` returns non-zero (401) when the daemon is up
/// but no Docker session exists; there is no `sbx auth` subcommand.
fn sbx_authenticated(binary: &std::path::Path) -> bool {
    matches!(
        Command::new(binary)
            .args(["ls", "--quiet"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
        Ok(status) if status.success()
    )
}

fn codex_sandbox_authenticated(binary: &std::path::Path) -> bool {
    let output = Command::new(binary)
        .args(["secret", "ls", "-g", "--service", "openai"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    combined.contains("openai") && !combined.contains("no secrets found")
}

fn normalize_network_preset(value: &str) -> &'static str {
    match value.trim() {
        "open" | "allow-all" | "allow_all" => "open",
        "locked_down" | "locked-down" | "deny-all" | "deny_all" | "restricted" | "offline" => {
            "locked_down"
        }
        _ => "balanced",
    }
}

fn sandbox_name_for(owner: &str, run_id: &str) -> String {
    let raw = format!("{SANDBOX_NAME_PREFIX}{owner}-{run_id}");
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(63)
        .collect()
}

fn parse_owned_sandbox_names(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with(SANDBOX_NAME_PREFIX))
        .map(ToOwned::to_owned)
        .collect()
}

fn uuid_tail() -> String {
    am_proto::new_id()
        .split('-')
        .next()
        .unwrap_or("run")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_names_are_bounded_and_prefixed() {
        let name = sandbox_name_for("thread_with_chars", "1234567890abcdefghijklmnopqrstuvwxyz");
        assert!(name.starts_with(SANDBOX_NAME_PREFIX));
        assert!(name.len() <= 63);
        assert!(name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    }

    #[test]
    fn policy_defaults_match_expected_limits() {
        let policy = SandboxPolicy::default();
        assert_eq!(policy.default_backend, ExecutionBackend::Host);
        assert_eq!(policy.max_concurrent_sandboxes, 2);
        assert_eq!(policy.cpus, 2);
        assert_eq!(policy.memory, "4g");
        assert_eq!(policy.network_preset, "balanced");
    }

    #[test]
    fn stale_reconciliation_only_targets_owned_names() {
        let names = parse_owned_sandbox_names(
            b"perpetual-thread-1\npersonal-sandbox\n perpetual-task-2 \n",
        );
        assert_eq!(names, vec!["perpetual-thread-1", "perpetual-task-2"]);
    }

    #[test]
    fn parses_sbx_login_device_code_and_url() {
        let code_line = "Your one-time device confirmation code is: BTBV-VPRK";
        let url_line =
            "Open this URL to sign in: https://login.docker.com/activate?user_code=BTBV-VPRK";
        assert_eq!(parse_login_code(code_line).as_deref(), Some("BTBV-VPRK"));
        assert_eq!(parse_login_code(url_line), None);
        assert_eq!(
            parse_login_url(url_line).as_deref(),
            Some("https://login.docker.com/activate?user_code=BTBV-VPRK")
        );
        assert_eq!(
            code_from_url("https://login.docker.com/activate?user_code=BTBV-VPRK&x=1").as_deref(),
            Some("BTBV-VPRK")
        );
    }

    #[test]
    fn normalizes_network_preset_aliases() {
        assert_eq!(normalize_network_preset("allow-all"), "open");
        assert_eq!(normalize_network_preset("deny_all"), "locked_down");
        assert_eq!(normalize_network_preset("restricted"), "locked_down");
        assert_eq!(normalize_network_preset("unknown"), "balanced");
    }
}
