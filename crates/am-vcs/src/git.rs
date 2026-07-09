use std::path::Path;

use am_proto::TaskDiff;

use crate::{git, head_sha, merge_changes, VcsError};

/// Create an isolated git worktree at `worktree_path` on a new `branch` based at
/// `base_sha`. The worktree path must be app-controlled (under app-data). The
/// user's working tree is untouched — only a `.git/worktrees/<name>` admin entry
/// is added, which is normal git metadata.
pub fn create_worktree(
    repo: &Path,
    worktree_path: &Path,
    branch: &str,
    base_sha: &str,
) -> Result<(), VcsError> {
    if worktree_path.exists() {
        return Ok(());
    }
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| VcsError::Io(e.to_string()))?;
    }
    let wt = worktree_path.to_string_lossy();
    git(
        repo,
        &["worktree", "add", "-b", branch, wt.as_ref(), base_sha],
    )?;
    Ok(())
}

/// Remove a previously-created worktree and prune its admin entry. Only ever
/// called on app-controlled paths.
pub fn remove_worktree(repo: &Path, worktree_path: &Path) -> Result<(), VcsError> {
    let wt = worktree_path.to_string_lossy();
    // --force because the worktree may contain uncommitted agent changes.
    let _ = git(repo, &["worktree", "remove", "--force", wt.as_ref()]);
    let _ = git(repo, &["worktree", "prune"]);
    Ok(())
}

/// Compute the diff of a worktree against its base commit, including newly
/// created (untracked) files. Read-mostly: uses intent-to-add on the worktree's
/// own private index so new files appear in the diff without staging content.
pub fn worktree_diff(
    worktree_path: &Path,
    base_sha: &str,
    max_bytes: usize,
) -> Result<TaskDiff, VcsError> {
    worktree_diff_with_excludes(worktree_path, base_sha, max_bytes, &[])
}

/// Like [`worktree_diff`], but omits root-relative pathspecs from the file list
/// and patch. Used for generated context files that should never be applied
/// back into the user's visible repository.
pub fn worktree_diff_with_excludes(
    worktree_path: &Path,
    base_sha: &str,
    max_bytes: usize,
    exclude_paths: &[&str],
) -> Result<TaskDiff, VcsError> {
    if !worktree_path.exists() {
        return Ok(TaskDiff::default());
    }

    // Make untracked files visible to `git diff` without committing/staging them.
    let _ = git(worktree_path, &["add", "-N", "."]);

    let name_status = git_diff_output(worktree_path, &["--name-status", base_sha], exclude_paths)?;
    let numstat = git_diff_output(worktree_path, &["--numstat", base_sha], exclude_paths)?;
    let files = merge_changes(&name_status, &numstat);

    let mut patch = git_diff_output(worktree_path, &[base_sha], exclude_paths)?;
    if patch.len() > max_bytes {
        // Truncate at a char boundary.
        let mut end = max_bytes;
        while end > 0 && !patch.is_char_boundary(end) {
            end -= 1;
        }
        patch.truncate(end);
        patch.push_str("\n\n… diff truncated (too large to display) …\n");
    }

    let branch = git(
        worktree_path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .ok();

    Ok(TaskDiff {
        files,
        patch,
        repo_id: None,
        repo_name: None,
        remote_url: None,
        branch,
        base_ref: Some(base_sha.to_string()),
        head_ref: head_sha(worktree_path).ok(),
        worktree_path: Some(worktree_path.to_string_lossy().to_string()),
    })
}

fn git_diff_output(cwd: &Path, args: &[&str], exclude_paths: &[&str]) -> Result<String, VcsError> {
    let mut owned = vec!["diff".to_string()];
    owned.extend(args.iter().map(|arg| (*arg).to_string()));
    if !exclude_paths.is_empty() {
        owned.push("--".to_string());
        owned.push(".".to_string());
        owned.extend(exclude_paths.iter().map(|path| format!(":(exclude){path}")));
    }
    crate::git_owned_raw(cwd, &owned)
}

#[cfg(test)]
mod tests {
    use crate::{merge_changes, parse_numstat, status_label};

    #[test]
    fn parses_numstat() {
        let out = "3\t1\tsrc/a.rs\n10\t0\tsrc/b.rs\n-\t-\timg.png";
        let parsed = parse_numstat(out);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], (3, 1, "src/a.rs".to_string()));
        assert_eq!(parsed[2], (0, 0, "img.png".to_string()));
    }

    #[test]
    fn merges_name_status_and_numstat() {
        let name_status = "A\tsrc/new.rs\nM\tsrc/old.rs\nD\tsrc/gone.rs";
        let numstat = "20\t0\tsrc/new.rs\n5\t3\tsrc/old.rs\n0\t8\tsrc/gone.rs";
        let changes = merge_changes(name_status, numstat);
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].status, "added");
        assert_eq!(changes[0].additions, 20);
        assert_eq!(changes[1].status, "modified");
        assert_eq!(changes[2].status, "deleted");
        assert_eq!(changes[2].deletions, 8);
    }

    #[test]
    fn status_letters() {
        assert_eq!(status_label('A'), "added");
        assert_eq!(status_label('M'), "modified");
        assert_eq!(status_label('D'), "deleted");
    }
}
