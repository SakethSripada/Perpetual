use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::detect::find_binary;
use crate::process::ManagedChild;
use crate::AgentError;

const SANDBOX_EXISTENCE_CACHE_TTL: Duration = Duration::from_secs(600);
const NETWORK_POLICY_CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
pub struct RuntimeLimits {
    pub startup_timeout: Duration,
    pub run_timeout: Duration,
    pub idle_timeout: Duration,
    pub stop_grace: Duration,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(120),
            run_timeout: Duration::from_secs(7_200),
            idle_timeout: Duration::from_secs(900),
            stop_grace: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone)]
pub enum SessionRuntime {
    Host {
        limits: RuntimeLimits,
    },
    DockerSandbox {
        name: String,
        cpus: u32,
        memory: String,
        network_preset: String,
        limits: RuntimeLimits,
    },
}

impl Default for SessionRuntime {
    fn default() -> Self {
        Self::Host {
            limits: RuntimeLimits::default(),
        }
    }
}

impl SessionRuntime {
    pub fn limits(&self) -> RuntimeLimits {
        match self {
            SessionRuntime::Host { limits } | SessionRuntime::DockerSandbox { limits, .. } => {
                *limits
            }
        }
    }

    pub fn sandbox_name(&self) -> Option<&str> {
        match self {
            SessionRuntime::DockerSandbox { name, .. } => Some(name),
            SessionRuntime::Host { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SandboxCleanup {
    pub sbx_binary: PathBuf,
    pub name: String,
}

pub async fn spawn_for_runtime(
    host_bin: &str,
    sandbox_agent: &str,
    host_args: &[String],
    cwd: &Path,
    runtime: &SessionRuntime,
) -> Result<ManagedChild, AgentError> {
    spawn_for_runtime_with_env(host_bin, sandbox_agent, host_args, cwd, runtime, &[]).await
}

pub async fn spawn_for_runtime_with_env(
    host_bin: &str,
    sandbox_agent: &str,
    host_args: &[String],
    cwd: &Path,
    runtime: &SessionRuntime,
    envs: &[(String, String)],
) -> Result<ManagedChild, AgentError> {
    match runtime {
        SessionRuntime::Host { .. } => {
            let binary = find_binary(host_bin).ok_or_else(|| agent_not_installed(host_bin))?;
            let child = ManagedChild::spawn_with_env(&binary, host_args, cwd, envs)
                .map_err(|e| AgentError::Spawn(e.to_string()))?;
            Ok(child)
        }
        SessionRuntime::DockerSandbox {
            name,
            cpus,
            memory,
            network_preset,
            ..
        } => {
            let sbx_binary =
                find_binary("sbx").ok_or_else(|| AgentError::Spawn("sbx CLI not found".into()))?;
            let ensure = DockerSandboxEnsure {
                sbx_binary: sbx_binary.clone(),
                name: name.clone(),
                cpus: *cpus,
                memory: memory.clone(),
                network_preset: network_preset.clone(),
                sandbox_agent: sandbox_agent.to_string(),
                cwd: cwd.to_path_buf(),
            };
            tokio::task::spawn_blocking(move || ensure_sandbox(ensure))
                .await
                .map_err(|e| AgentError::Spawn(e.to_string()))??;

            let args = docker_exec_args(name, sandbox_agent, host_args, cwd);
            let child = ManagedChild::spawn_with_env(&sbx_binary, &args, cwd, envs)
                .map_err(|e| AgentError::Spawn(e.to_string()))?;
            Ok(child)
        }
    }
}

/// Spawn a host binary with a piped stdin, for bidirectional JSON-RPC transports
/// (Codex app-server live approval). Only the Host runtime is supported; the
/// Docker sandbox runs in bypass mode where live approval does not apply.
pub async fn spawn_host_piped_stdin(
    host_bin: &str,
    args: &[String],
    cwd: &Path,
    envs: &[(String, String)],
) -> Result<ManagedChild, AgentError> {
    let binary = find_binary(host_bin).ok_or_else(|| agent_not_installed(host_bin))?;
    ManagedChild::spawn_with_env_piped_stdin(&binary, args, cwd, envs)
        .map_err(|e| AgentError::Spawn(e.to_string()))
}

#[derive(Debug)]
struct DockerSandboxEnsure {
    sbx_binary: PathBuf,
    name: String,
    cpus: u32,
    memory: String,
    network_preset: String,
    sandbox_agent: String,
    cwd: PathBuf,
}

fn ensure_sandbox(input: DockerSandboxEnsure) -> Result<(), AgentError> {
    configure_network_policy(&input.sbx_binary, &input.network_preset);
    if sandbox_known_recent(&input.name) {
        return Ok(());
    }
    if sandbox_exists(&input.sbx_binary, &input.name) {
        remember_sandbox(&input.name);
        return Ok(());
    }

    let args = docker_create_args(
        &input.name,
        input.cpus,
        &input.memory,
        &input.sandbox_agent,
        &input.cwd,
    );
    let output = Command::new(&input.sbx_binary)
        .args(&args)
        .current_dir(&input.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| AgentError::Spawn(e.to_string()))?;
    if output.status.success() {
        remember_sandbox(&input.name);
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let diagnostics = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.trim().to_string(),
        (true, false) => stderr.trim().to_string(),
        (false, false) => format!("{}\n{}", stdout.trim(), stderr.trim()),
    };
    Err(AgentError::Spawn(with_diagnostics(
        "failed to prepare Docker sandbox",
        &diagnostics,
    )))
}

pub fn forget_sandbox(name: &str) {
    if let Ok(mut cache) = sandbox_cache().lock() {
        cache.remove(name);
    }
}

fn sandbox_cache() -> &'static Mutex<HashMap<String, Instant>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sandbox_known_recent(name: &str) -> bool {
    let Ok(cache) = sandbox_cache().lock() else {
        return false;
    };
    cache
        .get(name)
        .is_some_and(|checked_at| checked_at.elapsed() < SANDBOX_EXISTENCE_CACHE_TTL)
}

fn remember_sandbox(name: &str) {
    if let Ok(mut cache) = sandbox_cache().lock() {
        cache.insert(name.to_string(), Instant::now());
    }
}

fn sandbox_exists(sbx_binary: &Path, name: &str) -> bool {
    let output = Command::new(sbx_binary)
        .args(["ls", "--quiet"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    matches!(output, Ok(output) if output.status.success() && String::from_utf8_lossy(&output.stdout).lines().any(|line| line.trim() == name))
}

fn docker_create_args(
    name: &str,
    cpus: u32,
    memory: &str,
    sandbox_agent: &str,
    cwd: &Path,
) -> Vec<String> {
    vec![
        "run".to_string(),
        "--detached".to_string(),
        "--name".to_string(),
        name.to_string(),
        "--cpus".to_string(),
        cpus.to_string(),
        "--memory".to_string(),
        memory.to_string(),
        sandbox_agent.to_string(),
        cwd.to_string_lossy().to_string(),
    ]
}

fn docker_exec_args(
    name: &str,
    sandbox_agent: &str,
    host_args: &[String],
    cwd: &Path,
) -> Vec<String> {
    let mut args = vec![
        "exec".to_string(),
        "--workdir".to_string(),
        cwd.to_string_lossy().to_string(),
        name.to_string(),
        sandbox_agent.to_string(),
    ];
    args.extend(host_args.iter().cloned());
    args
}

fn configure_network_policy(sbx_binary: &Path, network_preset: &str) {
    let Some(preset) = network_policy_arg(network_preset) else {
        return;
    };
    if network_policy_known_recent(preset) {
        return;
    }
    let output = Command::new(sbx_binary)
        .args(["policy", "set-default", preset])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            remember_network_policy(preset);
        } else {
            tracing::debug!(
                stderr = %String::from_utf8_lossy(&output.stderr),
                "sbx policy set-default did not apply"
            );
        }
    }
}

fn network_policy_cache() -> &'static Mutex<Option<(String, Instant)>> {
    static CACHE: OnceLock<Mutex<Option<(String, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn network_policy_known_recent(preset: &str) -> bool {
    let Ok(cache) = network_policy_cache().lock() else {
        return false;
    };
    cache.as_ref().is_some_and(|(known, checked_at)| {
        known == preset && checked_at.elapsed() < NETWORK_POLICY_CACHE_TTL
    })
}

fn remember_network_policy(preset: &str) {
    if let Ok(mut cache) = network_policy_cache().lock() {
        *cache = Some((preset.to_string(), Instant::now()));
    }
}

fn network_policy_arg(network_preset: &str) -> Option<&'static str> {
    match network_preset.trim() {
        "" | "balanced" => Some("balanced"),
        "open" | "allow-all" | "allow_all" => Some("allow-all"),
        "locked_down" | "locked-down" | "deny-all" | "deny_all" | "restricted" | "offline" => {
            Some("deny-all")
        }
        _ => None,
    }
}

pub async fn cleanup_sandbox(cleanup: Option<SandboxCleanup>) {
    let Some(cleanup) = cleanup else {
        return;
    };
    let commands: &[&[&str]] = &[&["rm", "--force"], &["rm", "-f"], &["rm"], &["stop"]];
    for prefix in commands {
        let status = Command::new(&cleanup.sbx_binary)
            .args(*prefix)
            .arg(&cleanup.name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if matches!(status, Ok(status) if status.success()) {
            forget_sandbox(&cleanup.name);
            return;
        }
    }
}

pub fn push_diagnostic_line(buf: &std::sync::Arc<std::sync::Mutex<String>>, line: &str) {
    const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
    if let Ok(mut buf) = buf.lock() {
        buf.push_str(line);
        buf.push('\n');
        if buf.len() > MAX_DIAGNOSTIC_BYTES {
            let keep_from = buf.len() - MAX_DIAGNOSTIC_BYTES;
            let split = buf[keep_from..]
                .find('\n')
                .map(|idx| keep_from + idx + 1)
                .unwrap_or(keep_from);
            buf.drain(..split);
        }
    }
}

pub fn buffered_diagnostics(
    stdout: &std::sync::Arc<std::sync::Mutex<String>>,
    stderr: &std::sync::Arc<std::sync::Mutex<String>>,
) -> String {
    let stdout = stdout.lock().map(|buf| buf.clone()).unwrap_or_default();
    let stderr = stderr.lock().map(|buf| buf.clone()).unwrap_or_default();
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

pub fn with_diagnostics(message: &str, diagnostics: &str) -> String {
    let diagnostics = diagnostics.trim();
    if diagnostics.is_empty() {
        message.to_string()
    } else {
        format!("{message}\n\n{diagnostics}")
    }
}

fn agent_not_installed(host_bin: &str) -> AgentError {
    match host_bin {
        "claude" => AgentError::NotInstalled(crate::AgentKind::ClaudeCode),
        "codex" => AgentError::NotInstalled(crate::AgentKind::Codex),
        _ => AgentError::Spawn(format!("{host_bin} CLI not found")),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{docker_create_args, docker_exec_args, network_policy_arg};

    #[test]
    fn maps_network_presets_to_sbx_policy_values() {
        assert_eq!(network_policy_arg(""), Some("balanced"));
        assert_eq!(network_policy_arg("balanced"), Some("balanced"));
        assert_eq!(network_policy_arg("open"), Some("allow-all"));
        assert_eq!(network_policy_arg("allow_all"), Some("allow-all"));
        assert_eq!(network_policy_arg("locked_down"), Some("deny-all"));
        assert_eq!(network_policy_arg("offline"), Some("deny-all"));
        assert_eq!(network_policy_arg("custom"), None);
    }

    #[test]
    fn builds_detached_sandbox_create_args() {
        let args = docker_create_args(
            "agentmanager-task-123",
            2,
            "4g",
            "codex",
            Path::new("/tmp/worktree"),
        );
        assert_eq!(
            args,
            vec![
                "run",
                "--detached",
                "--name",
                "agentmanager-task-123",
                "--cpus",
                "2",
                "--memory",
                "4g",
                "codex",
                "/tmp/worktree"
            ]
        );
    }

    #[test]
    fn builds_exec_args_without_pty_separator() {
        let host_args = vec![
            "exec".to_string(),
            "--json".to_string(),
            "Hello".to_string(),
        ];
        let args = docker_exec_args(
            "agentmanager-task-123",
            "codex",
            &host_args,
            Path::new("/tmp/worktree"),
        );
        assert_eq!(
            args,
            vec![
                "exec",
                "--workdir",
                "/tmp/worktree",
                "agentmanager-task-123",
                "codex",
                "exec",
                "--json",
                "Hello"
            ]
        );
        assert!(!args.iter().any(|arg| arg == "--"));
        assert!(!args.iter().any(|arg| arg == "-t" || arg == "--tty"));
    }
}
