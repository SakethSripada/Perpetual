//! Managed subprocess: spawns the agent in an OS-level containment boundary so
//! stopping a session terminates the agent and any tool subprocesses it spawned.
//! Unix uses process groups; Windows uses a Job Object with kill-on-close.

use std::path::Path;
use std::process::Stdio;

use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

#[cfg(windows)]
use std::io;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

pub struct ManagedChild {
    child: Child,
    #[cfg(unix)]
    pgid: i32,
    #[cfg(windows)]
    job: Option<OwnedHandle>,
}

impl ManagedChild {
    /// Spawn `program` with `args` in `cwd`. The prompt and all values are passed
    /// as discrete arguments — never through a shell — so they cannot be
    /// interpreted as commands. Inherits the environment so the CLI can read its
    /// own subscription auth (keychain/OAuth).
    pub fn spawn(program: &Path, args: &[String], cwd: &Path) -> std::io::Result<Self> {
        Self::spawn_with_env(program, args, cwd, &[])
    }

    /// Spawn with extra child-only environment variables. Use this for scoped
    /// provider credentials that should not be persisted in config files.
    pub fn spawn_with_env(
        program: &Path,
        args: &[String],
        cwd: &Path,
        envs: &[(String, String)],
    ) -> std::io::Result<Self> {
        Self::spawn_inner(program, args, cwd, envs, false)
    }

    /// Like [`Self::spawn_with_env`] but with a piped stdin, for bidirectional
    /// JSON-RPC transports (Codex app-server) that receive on stdout and send
    /// requests/approval replies on stdin.
    pub fn spawn_with_env_piped_stdin(
        program: &Path,
        args: &[String],
        cwd: &Path,
        envs: &[(String, String)],
    ) -> std::io::Result<Self> {
        Self::spawn_inner(program, args, cwd, envs, true)
    }

    fn spawn_inner(
        program: &Path,
        args: &[String],
        cwd: &Path,
        envs: &[(String, String)],
        pipe_stdin: bool,
    ) -> std::io::Result<Self> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(if pipe_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in envs {
            cmd.env(key, value);
        }

        #[cfg(unix)]
        {
            // 0 => the child becomes leader of a new process group equal to its pid.
            cmd.process_group(0);
        }

        let child = cmd.spawn()?;

        #[cfg(unix)]
        let pgid = child.id().map(|id| id as i32).unwrap_or(-1);

        #[cfg(windows)]
        let job = match create_kill_on_close_job(&child) {
            Ok(job) => Some(job),
            Err(error) => {
                tracing::warn!(%error, "failed to attach child process to Windows job object");
                None
            }
        };

        Ok(Self {
            child,
            #[cfg(unix)]
            pgid,
            #[cfg(windows)]
            job,
        })
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Wait for the process to exit.
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    /// Send SIGTERM to the whole process group (graceful).
    pub fn terminate_group(&mut self) {
        #[cfg(unix)]
        {
            if self.pgid > 1 {
                unsafe {
                    libc::kill(-self.pgid, libc::SIGTERM);
                }
                return;
            }
        }
        #[cfg(windows)]
        {
            // Closing a kill-on-close job is the closest Windows equivalent to
            // terminating the whole process tree. There is no reliable generic
            // graceful signal for arbitrary console and GUI agent subprocesses.
            self.job.take();
        }
        let _ = self.child.start_kill();
    }

    /// Send SIGKILL to the whole process group (forceful).
    pub fn kill_group(&mut self) {
        #[cfg(unix)]
        {
            if self.pgid > 1 {
                unsafe {
                    libc::kill(-self.pgid, libc::SIGKILL);
                }
                return;
            }
        }
        #[cfg(windows)]
        {
            self.job.take();
        }
        let _ = self.child.start_kill();
    }
}

#[cfg(windows)]
fn create_kill_on_close_job(child: &Child) -> io::Result<OwnedHandle> {
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe {
        let Some(child_handle) = child.raw_handle() else {
            return Err(io::Error::other("child process handle is unavailable"));
        };
        let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(raw.cast());

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = SetInformationJobObject(
            job.as_raw_handle().cast(),
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }

        let assigned = AssignProcessToJobObject(job.as_raw_handle().cast(), child_handle.cast());
        if assigned == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(job)
    }
}
