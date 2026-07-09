//! Incremental repository context indexing.
//!
//! Feeds `repo_context_index`, the per-file summary table that context packets
//! select from. Indexing is layered so repeat refreshes are near-free:
//!
//! 1. **Git fast path** — if `HEAD` and a digest of `git status --porcelain`
//!    match the last recorded walk (and the walk is recent), skip entirely.
//! 2. **Stat short-circuit** — during a walk, a file whose size and mtime match
//!    the stored row is skipped without being read or hashed.
//! 3. **Batched writes** — all changed rows, deletions, and the repo walk
//!    state land in one transaction instead of a statement per file.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

use am_db::repos::work_graph::{NewRepoContextFile, RepoContextMeta};
use am_proto::{now, WorkNodeRepoBinding};

use crate::{AppCore, CoreError};

pub(crate) const MAX_CONTEXT_INDEX_FILES_PER_REPO: usize = 2000;
const MAX_CONTEXT_INDEX_FILE_BYTES: u64 = 96 * 1024;
/// With a matching git state, trust the index this long before re-walking
/// (covers gitignored files that `git status` can't see changing).
const GIT_FAST_PATH_MAX_AGE_SECS: i64 = 600;

struct GitState {
    head_commit: String,
    dirty_digest: String,
}

struct RepoScan {
    upserts: Vec<NewRepoContextFile>,
    seen_paths: Vec<String>,
    file_count: i64,
}

impl AppCore {
    /// Bring the context index for each bound repo up to date. Cheap when
    /// nothing changed; proportional to the change set otherwise.
    pub(crate) async fn refresh_context_index(
        &self,
        repos: &[WorkNodeRepoBinding],
    ) -> Result<(), CoreError> {
        for binding in repos {
            let root = match binding.worktree_path.as_ref() {
                Some(path) => PathBuf::from(path),
                None => {
                    let Some(repo) =
                        am_db::repos::repo::get(&self.db.pool, &binding.repo_id).await?
                    else {
                        continue;
                    };
                    let Some(path) = repo.local_path else {
                        continue;
                    };
                    PathBuf::from(path)
                }
            };
            if !root.is_dir() {
                continue;
            }
            self.refresh_repo_index(&binding.repo_id, root).await?;
        }
        Ok(())
    }

    async fn refresh_repo_index(&self, repo_id: &str, root: PathBuf) -> Result<(), CoreError> {
        let git_root = root.clone();
        let git = tokio::task::spawn_blocking(move || read_git_state(&git_root))
            .await
            .ok()
            .flatten();

        let state = am_db::repos::work_graph::get_repo_index_state(&self.db.pool, repo_id).await?;
        if let (Some(state), Some(git)) = (state.as_ref(), git.as_ref()) {
            let unchanged = state.head_commit.as_deref() == Some(git.head_commit.as_str())
                && state.dirty_digest.as_deref() == Some(git.dirty_digest.as_str());
            let fresh = (now() - state.last_walk_at).num_seconds() < GIT_FAST_PATH_MAX_AGE_SECS;
            if unchanged && fresh {
                return Ok(());
            }
        }

        let existing: HashMap<String, RepoContextMeta> =
            am_db::repos::work_graph::list_repo_context_meta(&self.db.pool, repo_id)
                .await?
                .into_iter()
                .map(|meta| (meta.path.clone(), meta))
                .collect();

        let scan_root = root.clone();
        let scan_existing = existing.clone();
        let scan = tokio::task::spawn_blocking(move || scan_repo(&scan_root, &scan_existing))
            .await
            .map_err(|err| CoreError::Other(format!("context scan panicked: {err}")))?;

        let deleted: Vec<String> = existing
            .keys()
            .filter(|path| !scan.seen_paths.iter().any(|seen| seen == *path))
            .cloned()
            .collect();

        if scan.upserts.is_empty() && deleted.is_empty() && git.is_none() && state.is_some() {
            return Ok(());
        }
        am_db::repos::work_graph::apply_repo_context_changes(
            &self.db.pool,
            repo_id,
            &scan.upserts,
            &deleted,
            git.as_ref().map(|g| g.head_commit.as_str()),
            git.as_ref().map(|g| g.dirty_digest.as_str()),
            scan.file_count,
        )
        .await?;
        Ok(())
    }
}

/// HEAD commit plus a digest of the working-tree status. Two identical
/// readings mean tracked content hasn't changed. `None` when `root` isn't a
/// git repo or git isn't available (callers then rely on stat short-circuits).
fn read_git_state(root: &Path) -> Option<GitState> {
    let head = git_output(root, &["rev-parse", "HEAD"])?;
    // --untracked-files=all so a new file inside an untracked directory still
    // changes the digest.
    let status = git_output(root, &["status", "--porcelain", "--untracked-files=all"])?;
    Some(GitState {
        head_commit: head.trim().to_string(),
        dirty_digest: stable_hex_hash(status.as_bytes()),
    })
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Walk the repo and produce rows for files that are new or changed. Files
/// whose size+mtime match the stored row are marked seen without a read.
fn scan_repo(root: &Path, existing: &HashMap<String, RepoContextMeta>) -> RepoScan {
    let mut scan = RepoScan {
        upserts: Vec::new(),
        seen_paths: Vec::new(),
        file_count: 0,
    };
    for path in collect_context_files(root) {
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_CONTEXT_INDEX_FILE_BYTES {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        let mtime_ms = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);

        scan.file_count += 1;
        if let Some(prev) = existing.get(&relative) {
            if prev.size_bytes == metadata.len() as i64 && prev.mtime_ms == mtime_ms {
                scan.seen_paths.push(relative);
                continue;
            }
        }

        let Ok(raw) = fs::read(&path) else {
            continue;
        };
        if raw.iter().take(4096).any(|byte| *byte == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&raw);
        scan.upserts.push(NewRepoContextFile {
            language: language_for_path(&relative),
            symbols_json: serde_json::to_string(&symbols_for_source(&text))
                .unwrap_or_else(|_| "[]".into()),
            summary: summarize_source_file(&relative, &text),
            size_bytes: metadata.len() as i64,
            mtime_ms,
            content_hash: stable_hex_hash(&raw),
            path: relative.clone(),
        });
        scan.seen_paths.push(relative);
    }
    scan
}

fn collect_context_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        if depth > 8 || out.len() >= MAX_CONTEXT_INDEX_FILES_PER_REPO {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_CONTEXT_INDEX_FILES_PER_REPO {
                break;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if ignored_context_name(&name) {
                continue;
            }
            if path.is_dir() {
                walk(&path, out, depth + 1);
            } else if context_file_candidate(&path) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out, 0);
    out
}

fn ignored_context_name(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | ".next"
            | ".turbo"
            | ".cache"
            | "coverage"
            | "__pycache__"
    )
}

fn context_file_candidate(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    matches!(
        ext,
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "swift"
            | "rb"
            | "php"
            | "cs"
            | "cpp"
            | "c"
            | "h"
            | "hpp"
            | "sql"
            | "md"
            | "toml"
            | "json"
            | "yaml"
            | "yml"
            | "css"
            | "scss"
            | "html"
    )
}

fn summarize_source_file(path: &str, text: &str) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Path: {path}"));
    if let Some(language) = language_for_path(path) {
        lines.push(format!("Language: {language}"));
    }
    let symbols = symbols_for_source(text);
    if !symbols.is_empty() {
        lines.push(format!("Symbols: {}", symbols.join(", ")));
    }
    let body = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(24)
        .collect::<Vec<_>>()
        .join("\n");
    lines.push(crate::work_graph::truncate(&body, 1_000));
    lines.join("\n")
}

fn symbols_for_source(text: &str) -> Vec<String> {
    let mut symbols = Vec::new();
    for line in text.lines().take(400) {
        let trimmed = line.trim_start();
        let name = if let Some(rest) = trimmed.strip_prefix("fn ") {
            rest.split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
        } else if let Some(rest) = trimmed.strip_prefix("pub fn ") {
            rest.split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
        } else if let Some(rest) = trimmed.strip_prefix("function ") {
            rest.split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
        } else if let Some(rest) = trimmed.strip_prefix("export function ") {
            rest.split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
        } else if let Some(rest) = trimmed.strip_prefix("class ") {
            rest.split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
        } else if let Some(rest) = trimmed.strip_prefix("export class ") {
            rest.split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
        } else {
            None
        };
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            symbols.push(name.to_string());
        }
        if symbols.len() >= 12 {
            break;
        }
    }
    symbols
}

fn language_for_path(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    Some(match ext {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "java" | "kt" => "JVM",
        "sql" => "SQL",
        "md" => "Markdown",
        "json" => "JSON",
        "toml" => "TOML",
        "yaml" | "yml" => "YAML",
        "css" | "scss" => "Stylesheet",
        "html" => "HTML",
        _ => return None,
    })
}

pub(crate) fn stable_hex_hash(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_skips_unchanged_files_without_reading() {
        let dir = std::env::temp_dir().join(format!("am-index-test-{}", am_proto::new_id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.rs");
        fs::write(&file, "fn main() { println!(\"hi\"); }\n").unwrap();
        let metadata = fs::metadata(&file).unwrap();
        let mtime_ms = metadata
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Cold scan indexes the file.
        let cold = scan_repo(&dir, &HashMap::new());
        assert_eq!(cold.upserts.len(), 1);
        assert_eq!(cold.upserts[0].path, "main.rs");
        assert_eq!(cold.upserts[0].mtime_ms, mtime_ms);

        // Warm scan with matching size+mtime performs zero upserts.
        let existing: HashMap<String, RepoContextMeta> = [(
            "main.rs".to_string(),
            RepoContextMeta {
                path: "main.rs".into(),
                size_bytes: metadata.len() as i64,
                mtime_ms,
                content_hash: cold.upserts[0].content_hash.clone(),
            },
        )]
        .into_iter()
        .collect();
        let warm = scan_repo(&dir, &existing);
        assert!(warm.upserts.is_empty(), "unchanged file was re-indexed");
        assert_eq!(warm.seen_paths, vec!["main.rs".to_string()]);

        // A content change (different size) re-indexes.
        fs::write(&file, "fn main() { println!(\"hello world\"); }\n").unwrap();
        let changed = scan_repo(&dir, &existing);
        assert_eq!(changed.upserts.len(), 1);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_reports_vanished_files_via_seen_paths() {
        let dir = std::env::temp_dir().join(format!("am-index-del-{}", am_proto::new_id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep.rs"), "fn keep() {}\n").unwrap();

        let existing: HashMap<String, RepoContextMeta> = [(
            "gone.rs".to_string(),
            RepoContextMeta {
                path: "gone.rs".into(),
                size_bytes: 10,
                mtime_ms: 1,
                content_hash: "x".into(),
            },
        )]
        .into_iter()
        .collect();

        let scan = scan_repo(&dir, &existing);
        assert!(scan.seen_paths.contains(&"keep.rs".to_string()));
        assert!(!scan.seen_paths.contains(&"gone.rs".to_string()));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn summaries_capture_path_language_and_symbols() {
        let summary = summarize_source_file(
            "src/auth.ts",
            "export function login() {}\nexport class SessionStore {}\n",
        );
        assert!(summary.contains("Path: src/auth.ts"));
        assert!(summary.contains("Language: TypeScript"));
        assert!(summary.contains("login"));
        assert!(summary.contains("SessionStore"));
    }
}
