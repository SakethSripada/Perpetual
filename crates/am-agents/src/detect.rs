//! Agent binary discovery. Resolves a CLI from standard install locations,
//! editor extension bundles, PATH, and the user's shell lookup. Only existing
//! regular files are returned, and always as absolute paths.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const BINARY_CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct CachedBinary {
    path: Option<PathBuf>,
    checked_at: Instant,
}

fn binary_cache() -> &'static Mutex<HashMap<String, CachedBinary>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedBinary>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Candidate install directories for agent CLIs, in priority order.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home_dir() {
        dirs.push(home.join(".claude/local"));
        dirs.push(home.join(".codex/bin"));
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".npm-global/bin"));
        dirs.push(home.join(".bun/bin"));
        dirs.push(home.join(".deno/bin"));
        dirs.push(home.join(".volta/bin"));

        #[cfg(windows)]
        {
            dirs.push(home.join("AppData/Roaming/npm"));
            dirs.push(home.join("AppData/Local/Programs"));
            dirs.push(home.join("AppData/Local/Microsoft/WinGet/Packages"));
            dirs.push(home.join("AppData/Local/Volta/bin"));
            dirs.push(home.join("AppData/Local/pnpm"));
        }
    }

    #[cfg(not(windows))]
    {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/usr/bin"));
    }

    dirs.extend(path_dirs());
    dedupe_paths(dirs)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            let mut home = PathBuf::from(drive);
            home.push(path);
            Some(home)
        })
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

fn extension_roots() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    vec![
        home.join(".vscode/extensions"),
        home.join(".vscode-insiders/extensions"),
        home.join(".cursor/extensions"),
        home.join(".windsurf/extensions"),
    ]
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashMap::<String, ()>::new();
    let mut out = Vec::new();
    for path in paths {
        let key = normalize_key(&path);
        if seen.insert(key, ()).is_none() {
            out.push(path);
        }
    }
    out
}

fn normalize_key(path: &Path) -> String {
    let key = path.to_string_lossy().to_string();
    if cfg!(windows) {
        key.to_ascii_lowercase()
    } else {
        key
    }
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn executable_path(path: &Path) -> Option<PathBuf> {
    if !is_executable_file(path) {
        return None;
    }
    fs::canonicalize(path).ok().or_else(|| {
        path.is_absolute()
            .then(|| path.to_path_buf())
            .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(path)))
    })
}

fn candidate_names(bin: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        let path = Path::new(bin);
        if path.extension().is_some() {
            return vec![bin.to_string()];
        }

        let mut names = vec![bin.to_string()];
        let mut extensions = std::env::var_os("PATHEXT")
            .map(|value| {
                value
                    .to_string_lossy()
                    .split(';')
                    .filter_map(normalize_extension)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for ext in [".exe", ".cmd", ".bat", ".ps1"] {
            if !extensions
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(ext))
            {
                extensions.push(ext.to_string());
            }
        }
        for ext in extensions {
            names.push(format!("{bin}{ext}"));
        }
        return names;
    }

    #[cfg(not(windows))]
    {
        vec![bin.to_string()]
    }
}

#[cfg(windows)]
fn normalize_extension(ext: &str) -> Option<String> {
    let trimmed = ext.trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.starts_with('.') {
        Some(trimmed.to_ascii_lowercase())
    } else {
        Some(format!(".{}", trimmed.to_ascii_lowercase()))
    }
}

fn find_in_dir(dir: &Path, bin: &str) -> Option<PathBuf> {
    for name in candidate_names(bin) {
        let candidate = dir.join(&name);
        if let Some(path) = executable_path(&candidate) {
            return Some(path);
        }
    }

    #[cfg(windows)]
    {
        let names = candidate_names(bin);
        let Ok(entries) = fs::read_dir(dir) else {
            return None;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if names
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(name))
            {
                if let Some(path) = executable_path(&path) {
                    return Some(path);
                }
            }
        }
    }

    None
}

/// Resolve a binary path via the user's login shell or platform lookup tool.
/// Returns an absolute path only.
#[cfg(not(windows))]
fn via_system_lookup(bin: &str) -> Option<PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let output = Command::new(shell)
        .arg("-l")
        .arg("-c")
        .arg(format!("command -v {bin}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    first_executable_line(&output.stdout)
}

#[cfg(windows)]
fn via_system_lookup(bin: &str) -> Option<PathBuf> {
    let output = Command::new("where.exe").arg(bin).output().ok()?;
    if !output.status.success() {
        return None;
    }
    first_executable_line(&output.stdout)
}

fn first_executable_line(stdout: &[u8]) -> Option<PathBuf> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .find_map(|path| executable_path(&path))
}

fn extension_binaries(bin: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    for root in extension_roots() {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let extension = entry.path();
            if !extension.is_dir() {
                continue;
            }

            match bin {
                "claude" => collect_named_binaries(
                    &extension.join("resources/native-binary"),
                    bin,
                    2,
                    &mut candidates,
                ),
                "codex" => collect_named_binaries(&extension.join("bin"), bin, 4, &mut candidates),
                _ => {}
            }
        }
    }

    candidates
}

fn collect_named_binaries(root: &Path, bin: &str, max_depth: usize, out: &mut Vec<PathBuf>) {
    if max_depth == 0 || !root.is_dir() {
        return;
    }

    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_named_binaries(&path, bin, max_depth - 1, out);
        } else if matches_binary_name(&path, bin) {
            if let Some(path) = executable_path(&path) {
                out.push(path);
            }
        }
    }
}

fn matches_binary_name(path: &Path, bin: &str) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let names = candidate_names(bin);
    if cfg!(windows) {
        names
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
    } else {
        names.iter().any(|candidate| candidate == name)
    }
}

fn newest_path(mut paths: Vec<PathBuf>) -> Option<PathBuf> {
    paths.sort_by(|a, b| {
        modified_at(a)
            .cmp(&modified_at(b))
            .then_with(|| a.as_os_str().cmp(b.as_os_str()))
    });
    paths.pop()
}

fn modified_at(path: &Path) -> SystemTime {
    path.metadata()
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Find the absolute path to an agent CLI binary, or `None` if not installed.
///
/// `bin` must be a fixed, trusted name (e.g. `"claude"`, `"codex"`).
pub fn find_binary(bin: &str) -> Option<PathBuf> {
    if let Some(cached) = cached_binary(bin) {
        return cached;
    }

    let resolved = resolve_binary(bin);
    remember_binary(bin, resolved.clone());
    resolved
}

fn cached_binary(bin: &str) -> Option<Option<PathBuf>> {
    let cache = binary_cache().lock().ok()?;
    let cached = cache.get(bin)?;
    (cached.checked_at.elapsed() < BINARY_CACHE_TTL).then(|| cached.path.clone())
}

fn remember_binary(bin: &str, path: Option<PathBuf>) {
    if let Ok(mut cache) = binary_cache().lock() {
        cache.insert(
            bin.to_string(),
            CachedBinary {
                path,
                checked_at: Instant::now(),
            },
        );
    }
}

fn resolve_binary(bin: &str) -> Option<PathBuf> {
    for dir in candidate_dirs() {
        if let Some(path) = find_in_dir(&dir, bin) {
            return Some(path);
        }
    }
    via_system_lookup(bin).or_else(|| newest_path(extension_binaries(bin)))
}

/// Run `<binary> --version` and return the trimmed first line, if it succeeds.
pub fn binary_version(binary: &Path) -> Option<String> {
    let output = Command::new(binary).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&output.stdout);
    v.lines().next().map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, "#!/bin/sh\n").unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(not(unix))]
    fn make_executable(path: &Path) {
        fs::write(path, "").unwrap();
    }

    #[test]
    fn collects_nested_codex_extension_binary() {
        let root = std::env::temp_dir().join(format!("agentmanager-detect-{}", std::process::id()));
        let bin_dir = root.join("openai.chatgpt-test/bin/macos-aarch64");
        fs::create_dir_all(&bin_dir).unwrap();
        let codex = bin_dir.join("codex");
        make_executable(&codex);

        let mut found = Vec::new();
        collect_named_binaries(
            &root.join("openai.chatgpt-test/bin"),
            "codex",
            4,
            &mut found,
        );

        assert_eq!(found, vec![fs::canonicalize(&codex).unwrap_or(codex)]);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_candidate_names_cover_common_launchers() {
        let names = candidate_names("codex");
        assert!(names
            .iter()
            .any(|name| name.eq_ignore_ascii_case("codex.exe")));
        assert!(names
            .iter()
            .any(|name| name.eq_ignore_ascii_case("codex.cmd")));
        assert!(names
            .iter()
            .any(|name| name.eq_ignore_ascii_case("codex.bat")));
        assert!(names
            .iter()
            .any(|name| name.eq_ignore_ascii_case("codex.ps1")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_binary_matching_is_case_insensitive() {
        let root =
            std::env::temp_dir().join(format!("agentmanager-detect-case-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let codex = root.join("CoDeX.EXE");
        make_executable(&codex);

        let mut found = Vec::new();
        collect_named_binaries(&root, "codex", 1, &mut found);

        assert_eq!(found, vec![fs::canonicalize(&codex).unwrap_or(codex)]);
        let _ = fs::remove_dir_all(root);
    }
}
