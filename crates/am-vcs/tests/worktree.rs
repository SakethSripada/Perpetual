//! Integration test: exercises the real git worktree + diff flow against a
//! throwaway repository, proving task changes are isolated and surfaced.

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn worktree_isolates_and_diffs_changes() {
    let tmp = std::env::temp_dir().join(format!("am-vcs-test-{}", std::process::id()));
    let repo = tmp.join("repo");
    let worktrees = tmp.join("worktrees");
    std::fs::create_dir_all(&repo).unwrap();

    // Initialize a repo with one commit on a known branch.
    git(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);

    // Validate + capture base.
    let info = am_vcs::validate_repo(&repo.to_string_lossy()).expect("valid repo");
    assert_eq!(info.default_branch, "main");
    let base = am_vcs::head_sha(&repo).expect("head sha");

    // Create an isolated worktree (under a separate app-data-like dir).
    let wt = worktrees.join("task-1");
    am_vcs::create_worktree(&repo, &wt, "am/task-1", &base).expect("worktree");
    assert!(wt.join("README.md").exists());

    // Make changes in the worktree only.
    std::fs::write(wt.join("README.md"), "hello\nworld\n").unwrap();
    std::fs::write(wt.join("new.txt"), "brand new file\n").unwrap();

    let diff = am_vcs::worktree_diff(&wt, &base, am_vcs::MAX_DIFF_BYTES).expect("diff");
    let paths: Vec<&str> = diff.files.iter().map(|f| f.path.as_str()).collect();
    assert!(
        paths.contains(&"README.md"),
        "modified file present: {paths:?}"
    );
    assert!(paths.contains(&"new.txt"), "new file present: {paths:?}");
    assert!(
        diff.patch.contains("brand new file"),
        "patch shows new content"
    );
    assert_eq!(diff.branch.as_deref(), Some("am/task-1"));

    // The user's original repo working tree is untouched.
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "hello\n"
    );
    assert!(!repo.join("new.txt").exists());

    // Cleanup removes the worktree.
    am_vcs::remove_worktree(&repo, &wt).expect("remove");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn clone_repo_creates_managed_checkout() {
    let tmp = std::env::temp_dir().join(format!("am-vcs-clone-test-{}", std::process::id()));
    let source = tmp.join("source");
    let managed = tmp.join("managed").join("repo");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&source).unwrap();

    git(&source, &["init", "-b", "main"]);
    std::fs::write(source.join("README.md"), "hello\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-m", "init"]);

    let info = am_vcs::clone_repo(&source.to_string_lossy(), &managed, None).expect("clone");
    assert_eq!(info.default_branch, "main");
    assert!(managed.join("README.md").exists());

    let again = am_vcs::clone_repo(&source.to_string_lossy(), &managed, None).expect("reuse");
    assert_eq!(again.toplevel, info.toplevel);

    let _ = std::fs::remove_dir_all(&tmp);
}
