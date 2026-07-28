//! Version-control + workspace management.
//!
//! Drives the `git` CLI via argument arrays only (never a shell string, so no
//! injection is possible). All write operations target **app-controlled**
//! worktree paths derived from task UUIDs under the app-data directory — the
//! user's repository working tree and checked-out branch are never modified.
//! Diffs are computed on demand and capped in size for efficiency.

use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use am_proto::{FileChange, TaskDiff};

mod git;

pub use git::{create_worktree, remove_worktree, worktree_diff, worktree_diff_with_excludes};

/// Maximum diff patch size returned to the UI (2 MiB). Larger diffs are
/// truncated to keep memory and IPC bounded.
pub const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum VcsError {
    #[error("path does not exist: {0}")]
    MissingPath(String),
    #[error("not a git repository: {0}")]
    NotARepo(String),
    #[error("repository has no commits yet")]
    EmptyRepo,
    #[error("invalid repository path: {0}")]
    InvalidPath(String),
    #[error("git command failed: {0}")]
    Git(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Metadata about a validated local repository.
#[derive(Debug, Clone)]
pub struct RepoInfo {
    /// Canonical top-level directory of the working tree.
    pub toplevel: PathBuf,
    /// The branch currently checked out (used as the base for worktrees).
    pub default_branch: String,
    /// Directory name, used as a display name.
    pub name: String,
}

/// Run a git command in `cwd` with the given args, returning trimmed stdout.
/// Uses an argument array — never a shell — so user-supplied values cannot be
/// interpreted as commands.
fn git(cwd: &Path, args: &[&str]) -> Result<String, VcsError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| VcsError::Io(e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(VcsError::Git(stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_owned(cwd: &Path, args: &[String]) -> Result<String, VcsError> {
    let output = git_owned_output(cwd, args)?;
    Ok(String::from_utf8_lossy(&output).trim().to_string())
}

fn git_owned_raw(cwd: &Path, args: &[String]) -> Result<String, VcsError> {
    let output = git_owned_output(cwd, args)?;
    Ok(String::from_utf8_lossy(&output).to_string())
}

fn git_owned_output(cwd: &Path, args: &[String]) -> Result<Vec<u8>, VcsError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| VcsError::Io(e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(VcsError::Git(stderr));
    }
    Ok(output.stdout)
}

/// Clone a repository into an app-managed destination and return its metadata.
///
/// If `dest` already exists, it is validated and returned without recloning so
/// multiple projects can share the same managed checkout. `auth_header`, when
/// supplied, is passed via process-local git config only; it is not written into
/// the remote URL or repository config.
pub fn clone_repo(
    remote_url: &str,
    dest: &Path,
    auth_header: Option<&str>,
) -> Result<RepoInfo, VcsError> {
    if dest.exists() {
        let info = validate_repo_path(dest)?;
        ensure_origin_matches(&info.toplevel, remote_url)?;
        return Ok(info);
    }

    let parent = dest
        .parent()
        .ok_or_else(|| VcsError::InvalidPath(dest.display().to_string()))?;
    std::fs::create_dir_all(parent).map_err(|e| VcsError::Io(e.to_string()))?;

    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| VcsError::InvalidPath(dest.display().to_string()))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{name}.tmp-{}-{nonce}", std::process::id()));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).map_err(|e| VcsError::Io(e.to_string()))?;
    }

    let mut command = Command::new("git");
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("clone")
        .arg("--")
        .arg(remote_url)
        .arg(&tmp);

    if let Some(header) = auth_header.filter(|value| !value.trim().is_empty()) {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
            .env("GIT_CONFIG_VALUE_0", header);
    }

    let output = command.output().map_err(|e| VcsError::Io(e.to_string()))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(VcsError::Git(stderr));
    }

    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        VcsError::Io(e.to_string())
    })?;

    let info = validate_repo_path(dest)?;
    ensure_origin_matches(&info.toplevel, remote_url)?;
    Ok(info)
}

fn validate_repo_path(path: &Path) -> Result<RepoInfo, VcsError> {
    validate_repo(&path.to_string_lossy())
}

fn ensure_origin_matches(repo: &Path, remote_url: &str) -> Result<(), VcsError> {
    let origin = git(repo, &["config", "--get", "remote.origin.url"])?;
    if origin == remote_url {
        Ok(())
    } else {
        Err(VcsError::Git(format!(
            "managed clone already exists with a different origin: {origin}"
        )))
    }
}

/// Validate that `path` is a local git repository and gather metadata. Read-only.
pub fn validate_repo(path: &str) -> Result<RepoInfo, VcsError> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(VcsError::MissingPath(path.to_string()));
    }
    // Confirm it is inside a work tree.
    let inside = git(p, &["rev-parse", "--is-inside-work-tree"])
        .map_err(|_| VcsError::NotARepo(path.to_string()))?;
    if inside != "true" {
        return Err(VcsError::NotARepo(path.to_string()));
    }

    let toplevel = PathBuf::from(git(p, &["rev-parse", "--show-toplevel"])?);

    // Must have at least one commit to base a worktree on.
    if git(&toplevel, &["rev-parse", "--verify", "HEAD"]).is_err() {
        return Err(VcsError::EmptyRepo);
    }

    let default_branch = git(&toplevel, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_else(|_| "HEAD".to_string());

    let name = toplevel
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());

    Ok(RepoInfo {
        toplevel,
        default_branch,
        name,
    })
}

/// Resolve the current HEAD commit SHA of a repository (the base a worktree is
/// branched from).
pub fn head_sha(repo: &Path) -> Result<String, VcsError> {
    git(repo, &["rev-parse", "HEAD"])
}

/// Create an app-managed standalone clone at `workspace_path` and check out a
/// private branch at `base_sha`. This is used for container-backed runs so the
/// mounted workspace contains its own `.git` directory instead of a worktree
/// pointer into the user's source repository metadata.
pub fn create_clone_workspace(
    repo: &Path,
    workspace_path: &Path,
    branch: &str,
    base_sha: &str,
) -> Result<(), VcsError> {
    if workspace_path.exists() {
        return Ok(());
    }
    if let Some(parent) = workspace_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| VcsError::Io(e.to_string()))?;
    }

    let output = Command::new("git")
        .arg("clone")
        .arg("--no-hardlinks")
        .arg("--")
        .arg(repo)
        .arg(workspace_path)
        .output()
        .map_err(|e| VcsError::Io(e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(VcsError::Git(stderr));
    }

    git(workspace_path, &["checkout", "-B", branch, base_sha])?;
    Ok(())
}

/// Stage and commit all pending worktree changes. Returns the new HEAD when a
/// commit was created, or `None` if the worktree was already clean.
pub fn commit_all(repo: &Path, message: &str) -> Result<Option<String>, VcsError> {
    commit_all_with_excludes(repo, message, &[])
}

/// Stage and commit all pending worktree changes except the given root-relative
/// pathspecs. This is used to keep Perpetual-generated context files out of
/// pull-request commits.
pub fn commit_all_with_excludes(
    repo: &Path,
    message: &str,
    exclude_paths: &[&str],
) -> Result<Option<String>, VcsError> {
    let _ = git(repo, &["reset", "--mixed"]);

    let mut add_args = vec![
        "add".to_string(),
        "-A".to_string(),
        "--".to_string(),
        ".".to_string(),
    ];
    add_args.extend(exclude_paths.iter().map(|path| format!(":(exclude){path}")));
    git_owned(repo, &add_args)?;

    let mut status_args = vec![
        "status".to_string(),
        "--porcelain".to_string(),
        "--".to_string(),
        ".".to_string(),
    ];
    status_args.extend(exclude_paths.iter().map(|path| format!(":(exclude){path}")));
    let status = git_owned(repo, &status_args)?;
    if status.trim().is_empty() {
        return Ok(None);
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("-c")
        .arg("user.name=Perpetual")
        .arg("-c")
        .arg("user.email=perpetual@local")
        .arg("commit")
        .arg("-m")
        .arg(message)
        .output()
        .map_err(|e| VcsError::Io(e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(VcsError::Git(stderr));
    }

    Ok(Some(head_sha(repo)?))
}

/// Return the subset of `paths` that already have uncommitted changes in
/// `repo`. Callers use this before applying a managed worktree patch so local
/// edits in the visible repository are never overwritten.
pub fn dirty_paths(repo: &Path, paths: &[String]) -> Result<Vec<String>, VcsError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut dirty = Vec::new();
    for path in paths {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("status")
            .arg("--porcelain")
            .arg("--")
            .arg(path)
            .output()
            .map_err(|e| VcsError::Io(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(VcsError::Git(stderr));
        }
        if !String::from_utf8_lossy(&output.stdout).trim().is_empty() {
            dirty.push(path.clone());
        }
    }
    Ok(dirty)
}

/// Build an uncapped binary patch for a managed worktree against `base_sha`.
pub fn worktree_patch_with_excludes(
    worktree_path: &Path,
    base_sha: &str,
    exclude_paths: &[&str],
) -> Result<String, VcsError> {
    if !worktree_path.exists() {
        return Ok(String::new());
    }
    let _ = git(worktree_path, &["add", "-N", "."]);
    let mut args = vec![
        "diff".to_string(),
        "--binary".to_string(),
        base_sha.to_string(),
    ];
    if !exclude_paths.is_empty() {
        args.push("--".to_string());
        args.push(".".to_string());
        args.extend(exclude_paths.iter().map(|path| format!(":(exclude){path}")));
    }
    git_owned_raw(worktree_path, &args)
}

/// Apply a patch into the user's visible repo. The patch is applied to the
/// working tree only (not committed and not staged by us).
pub fn apply_patch_to_repo(repo: &Path, patch: &str) -> Result<(), VcsError> {
    run_apply_patch(repo, patch, false)
}

/// Verify a patch can apply with the same strategy as [`apply_patch_to_repo`]
/// without writing anything.
pub fn check_patch_applies(repo: &Path, patch: &str) -> Result<(), VcsError> {
    run_apply_patch(repo, patch, true)
}

/// Materialize `patch` at its advertised base and intentionally replace only
/// the declared paths in `repo`. Existing paths are copied to `backup_dir`
/// first and restored if any replacement fails. The retained backup makes an
/// explicit remote-wins action recoverable without rewriting Git history.
pub fn overwrite_patch_paths(
    repo: &Path,
    base_ref: &str,
    patch: &str,
    paths: &[String],
    scratch_worktree: &Path,
    backup_dir: &Path,
) -> Result<(), VcsError> {
    if patch.trim().is_empty() || paths.is_empty() {
        return Ok(());
    }
    validate_repo_path(repo)?;
    if scratch_worktree.exists() {
        return Err(VcsError::InvalidPath(
            scratch_worktree.display().to_string(),
        ));
    }
    let mut safe_paths = Vec::new();
    for path in paths {
        let relative = safe_relative_path(path)?;
        if !safe_paths.contains(&relative) {
            safe_paths.push(relative);
        }
    }
    if let Some(parent) = scratch_worktree.parent() {
        std::fs::create_dir_all(parent).map_err(|err| VcsError::Io(err.to_string()))?;
    }
    std::fs::create_dir_all(backup_dir).map_err(|err| VcsError::Io(err.to_string()))?;

    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("worktree")
        .arg("add")
        .arg("--detach")
        .arg("--")
        .arg(scratch_worktree)
        .arg(base_ref)
        .output()
        .map_err(|err| VcsError::Io(err.to_string()))?;
    if !output.status.success() {
        return Err(VcsError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    let result = (|| {
        apply_patch_to_repo(scratch_worktree, patch)?;
        let mut processed = Vec::<(PathBuf, bool)>::new();
        for relative in &safe_paths {
            let target = repo.join(relative);
            let source = scratch_worktree.join(relative);
            let backup = backup_dir.join(relative);
            let existed = path_exists(&target);
            if existed {
                copy_file_entry(&target, &backup)?;
            }
            let replace = (|| {
                remove_file_entry(&target)?;
                if path_exists(&source) {
                    copy_file_entry(&source, &target)?;
                }
                Ok(())
            })();
            processed.push((relative.clone(), existed));
            if let Err(error) = replace {
                for (done, had_original) in processed.into_iter().rev() {
                    let restore_target = repo.join(&done);
                    let _ = remove_file_entry(&restore_target);
                    if had_original {
                        let _ = copy_file_entry(&backup_dir.join(done), &restore_target);
                    }
                }
                return Err(error);
            }
        }
        Ok(())
    })();

    let cleanup = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg("--")
        .arg(scratch_worktree)
        .output();
    if result.is_ok() {
        if let Ok(output) = cleanup {
            if !output.status.success() {
                return Err(VcsError::Git(
                    String::from_utf8_lossy(&output.stderr).trim().to_string(),
                ));
            }
        }
    }
    result
}

fn safe_relative_path(value: &str) -> Result<PathBuf, VcsError> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || path
            .components()
            .next()
            .is_some_and(|component| component.as_os_str() == ".git")
    {
        return Err(VcsError::InvalidPath(value.to_string()));
    }
    Ok(path.to_path_buf())
}

fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn remove_file_entry(path: &Path) -> Result<(), VcsError> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        return Err(VcsError::InvalidPath(format!(
            "refusing to replace directory {}",
            path.display()
        )));
    }
    std::fs::remove_file(path).map_err(|err| VcsError::Io(err.to_string()))
}

fn copy_file_entry(source: &Path, target: &Path) -> Result<(), VcsError> {
    let metadata =
        std::fs::symlink_metadata(source).map_err(|err| VcsError::Io(err.to_string()))?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        return Err(VcsError::InvalidPath(format!(
            "refusing to copy directory {}",
            source.display()
        )));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|err| VcsError::Io(err.to_string()))?;
    }
    if metadata.file_type().is_symlink() {
        let link = std::fs::read_link(source).map_err(|err| VcsError::Io(err.to_string()))?;
        create_symlink(&link, target)
    } else {
        std::fs::copy(source, target)
            .map(|_| ())
            .map_err(|err| VcsError::Io(err.to_string()))
    }
}

#[cfg(unix)]
fn create_symlink(source: &Path, target: &Path) -> Result<(), VcsError> {
    std::os::unix::fs::symlink(source, target).map_err(|err| VcsError::Io(err.to_string()))
}

#[cfg(windows)]
fn create_symlink(source: &Path, target: &Path) -> Result<(), VcsError> {
    std::os::windows::fs::symlink_file(source, target).map_err(|err| VcsError::Io(err.to_string()))
}

fn run_apply_patch(repo: &Path, patch: &str, check_only: bool) -> Result<(), VcsError> {
    if patch.trim().is_empty() {
        return Ok(());
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("apply")
        .args(check_only.then_some("--check"))
        .arg("--3way")
        .arg("--whitespace=nowarn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| VcsError::Io(e.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(patch.as_bytes())
            .map_err(|e| VcsError::Io(e.to_string()))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| VcsError::Io(e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(VcsError::Git(stderr));
    }
    Ok(())
}

/// Push the current HEAD to `origin/<branch>`. Authentication, when supplied,
/// is process-local and is not persisted into repository config.
pub fn push_branch(repo: &Path, branch: &str, auth_header: Option<&str>) -> Result<(), VcsError> {
    let refspec = format!("HEAD:refs/heads/{branch}");
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("push")
        .arg("origin")
        .arg(refspec);

    if let Some(header) = auth_header.filter(|value| !value.trim().is_empty()) {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
            .env("GIT_CONFIG_VALUE_0", header);
    }

    let output = command.output().map_err(|e| VcsError::Io(e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(VcsError::Git(stderr));
    }
    Ok(())
}

/// The SHA `origin/<branch>` points at on the remote, without fetching.
/// Returns `None` when the branch doesn't exist remotely. Used to observe
/// cloud-run progress cheaply.
pub fn remote_branch_sha(
    repo: &Path,
    branch: &str,
    auth_header: Option<&str>,
) -> Result<Option<String>, VcsError> {
    let refspec = format!("refs/heads/{branch}");
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("ls-remote")
        .arg("origin")
        .arg(&refspec);
    apply_auth(&mut command, auth_header);
    let output = command.output().map_err(|e| VcsError::Io(e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(VcsError::Git(stderr));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .split_whitespace()
        .next()
        .filter(|sha| !sha.is_empty())
        .map(str::to_string))
}

/// Fetch `origin/<branch>` into the repo.
pub fn fetch_branch(repo: &Path, branch: &str, auth_header: Option<&str>) -> Result<(), VcsError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("fetch")
        .arg("origin")
        .arg(branch);
    apply_auth(&mut command, auth_header);
    let output = command.output().map_err(|e| VcsError::Io(e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(VcsError::Git(stderr));
    }
    Ok(())
}

/// Fast-forward the worktree to the previously fetched `FETCH_HEAD`. Refuses
/// divergent histories (returns `Err`) so callers can surface a review state
/// instead of overwriting local work.
pub fn fast_forward_to_fetch_head(repo: &Path) -> Result<(), VcsError> {
    git(repo, &["merge", "--ff-only", "FETCH_HEAD"]).map(|_| ())
}

/// `git log --oneline from..to`, empty when `from` is `None` (no baseline).
pub fn commits_between(repo: &Path, from: Option<&str>, to: &str) -> Result<Vec<String>, VcsError> {
    let range = match from {
        Some(from) => format!("{from}..{to}"),
        None => to.to_string(),
    };
    let out = git(
        repo,
        &["log", "--oneline", "--no-decorate", "-n", "50", &range],
    )?;
    Ok(out.lines().map(str::to_string).collect())
}

fn apply_auth(command: &mut Command, auth_header: Option<&str>) {
    if let Some(header) = auth_header.filter(|value| !value.trim().is_empty()) {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
            .env("GIT_CONFIG_VALUE_0", header);
    }
}

/// Helper used by the diff module and tests to parse `--numstat` output.
pub(crate) fn parse_numstat(numstat: &str) -> Vec<(u32, u32, String)> {
    numstat
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let add = parts.next()?;
            let del = parts.next()?;
            let path = parts.next()?;
            // Binary files show '-' for counts.
            let additions = add.parse::<u32>().unwrap_or(0);
            let deletions = del.parse::<u32>().unwrap_or(0);
            Some((additions, deletions, path.to_string()))
        })
        .collect()
}

/// Map a git status letter to our file-change status string.
pub(crate) fn status_label(letter: char) -> &'static str {
    match letter {
        'A' => "added",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "copied",
        _ => "modified",
    }
}

/// Build a [`FileChange`] list by combining name-status and numstat output.
pub(crate) fn merge_changes(name_status: &str, numstat: &str) -> Vec<FileChange> {
    let stats = parse_numstat(numstat);
    name_status
        .lines()
        .filter_map(|line| {
            let parts = line.split('\t').collect::<Vec<_>>();
            let status = parts.first().copied()?;
            let path = parts.last()?.to_string();
            let letter = status.chars().next().unwrap_or('M');
            let (additions, deletions) = stats
                .iter()
                .find(|(_, _, p)| *p == path)
                .map(|(a, d, _)| (*a, *d))
                .unwrap_or((0, 0));
            Some(FileChange {
                path,
                status: status_label(letter).to_string(),
                additions,
                deletions,
            })
        })
        .collect()
}

/// An empty diff for a task that has no worktree yet.
pub fn empty_diff() -> TaskDiff {
    TaskDiff::default()
}

#[cfg(test)]
mod apply_tests {
    use super::*;

    fn temp_repo(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "perpetual-vcs-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn run(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(repo: &Path) {
        run(repo, &["init"]);
        run(repo, &["config", "user.name", "Test"]);
        run(repo, &["config", "user.email", "test@example.com"]);
        std::fs::write(repo.join("file.txt"), "base\n").unwrap();
        run(repo, &["add", "."]);
        run(repo, &["commit", "-m", "base"]);
    }

    #[test]
    fn worktree_diff_excludes_generated_context_files() {
        let repo = temp_repo("exclude");
        init_repo(&repo);
        let base = head_sha(&repo).unwrap();
        std::fs::write(repo.join("AGENTS.md"), "generated\n").unwrap();
        std::fs::write(repo.join("file.txt"), "changed\n").unwrap();

        let diff =
            worktree_diff_with_excludes(&repo, &base, MAX_DIFF_BYTES, &["AGENTS.md"]).unwrap();
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "file.txt");
        assert!(!diff.patch.contains("AGENTS.md"));

        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn dirty_overlap_blocks_before_apply() {
        let repo = temp_repo("dirty");
        let worktree = repo.with_file_name(format!(
            "{}-worktree",
            repo.file_name().unwrap().to_string_lossy()
        ));
        init_repo(&repo);
        let base = head_sha(&repo).unwrap();
        create_worktree(&repo, &worktree, "am-test-dirty", &base).unwrap();
        std::fs::write(worktree.join("file.txt"), "managed\n").unwrap();
        let diff = worktree_diff(&worktree, &base, MAX_DIFF_BYTES).unwrap();
        let paths = diff
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();

        std::fs::write(repo.join("file.txt"), "local\n").unwrap();
        let dirty = dirty_paths(&repo, &paths).unwrap();
        assert_eq!(dirty, vec!["file.txt".to_string()]);

        let _ = remove_worktree(&repo, &worktree);
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(worktree);
    }

    #[test]
    fn binary_worktree_patch_applies_to_visible_repo() {
        let repo = temp_repo("binary-apply");
        let worktree = repo.with_file_name(format!(
            "{}-worktree",
            repo.file_name().unwrap().to_string_lossy()
        ));
        init_repo(&repo);
        let base = head_sha(&repo).unwrap();
        create_worktree(&repo, &worktree, "am-test-binary", &base).unwrap();
        let bytes = (0..=255).cycle().take(2048).collect::<Vec<_>>();
        std::fs::write(worktree.join("asset.bin"), &bytes).unwrap();

        let patch = worktree_patch_with_excludes(&worktree, &base, &[]).unwrap();
        check_patch_applies(&repo, &patch).unwrap();
        apply_patch_to_repo(&repo, &patch).unwrap();

        assert_eq!(std::fs::read(repo.join("asset.bin")).unwrap(), bytes);

        let _ = remove_worktree(&repo, &worktree);
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(worktree);
    }

    #[test]
    fn explicit_overwrite_keeps_a_recoverable_backup() {
        let repo = temp_repo("overwrite");
        let producer = repo.with_file_name(format!(
            "{}-producer",
            repo.file_name().unwrap().to_string_lossy()
        ));
        let scratch = repo.with_file_name(format!(
            "{}-scratch",
            repo.file_name().unwrap().to_string_lossy()
        ));
        let backup = repo.with_file_name(format!(
            "{}-backup",
            repo.file_name().unwrap().to_string_lossy()
        ));
        init_repo(&repo);
        let base = head_sha(&repo).unwrap();
        create_worktree(&repo, &producer, "am-test-overwrite", &base).unwrap();
        std::fs::write(producer.join("file.txt"), "remote\n").unwrap();
        let patch = worktree_patch_with_excludes(&producer, &base, &[]).unwrap();

        std::fs::write(repo.join("file.txt"), "local\n").unwrap();
        overwrite_patch_paths(
            &repo,
            &base,
            &patch,
            &["file.txt".into()],
            &scratch,
            &backup,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(repo.join("file.txt")).unwrap(),
            "remote\n"
        );
        assert_eq!(
            std::fs::read_to_string(backup.join("file.txt")).unwrap(),
            "local\n"
        );

        let _ = remove_worktree(&repo, &producer);
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(producer);
        let _ = std::fs::remove_dir_all(scratch);
        let _ = std::fs::remove_dir_all(backup);
    }
}
