//! Discovers the Claude models the installed Claude Code CLI knows about.
//!
//! Claude Code ships its model lineup (display names and ids) inside the CLI,
//! so reading it from the installed binary keeps Perpetual's model list
//! current whenever the user updates the CLI, without a Perpetual release.
//! Nothing is sent anywhere and no credentials are read.

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// How many versions of each family to offer besides its "latest" alias.
const VERSIONS_PER_FAMILY: usize = 3;
const CHUNK: usize = 8 * 1024 * 1024;
const OVERLAP: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeModel {
    pub family: String,
    pub id: String,
    pub label: String,
    version: (u32, u32),
}

type CacheKey = (PathBuf, u64, Option<SystemTime>);

fn cache() -> &'static Mutex<HashMap<CacheKey, Vec<ClaudeModel>>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, Vec<ClaudeModel>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Versioned models for `families`, newest first within each family.
pub(crate) fn discover(binary: &Path, families: &[&str]) -> Vec<ClaudeModel> {
    let mut found = Scan::default();
    for target in scan_targets(binary) {
        let Ok(meta) = std::fs::metadata(&target) else {
            continue;
        };
        let key = (target.clone(), meta.len(), meta.modified().ok());
        if let Some(hit) = cache().lock().ok().and_then(|c| c.get(&key).cloned()) {
            return filter(hit, families);
        }
        if scan_file(&target, &mut found).is_ok() {
            let all = found.models();
            if let Ok(mut c) = cache().lock() {
                c.insert(key, all.clone());
            }
            if !all.is_empty() {
                return filter(all, families);
            }
        }
    }
    Vec::new()
}

/// The files that hold the lineup: the native binary itself, or for npm
/// installs (a `.cmd` shim or shell wrapper) the package's bundled CLI.
fn scan_targets(binary: &Path) -> Vec<PathBuf> {
    let resolved = std::fs::canonicalize(binary).unwrap_or_else(|_| binary.to_path_buf());
    let ext = resolved
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let small = std::fs::metadata(&resolved)
        .map(|m| m.len() < 64 * 1024)
        .unwrap_or(true);
    if !(matches!(ext.as_deref(), Some("cmd" | "bat" | "ps1")) || small) {
        return vec![resolved];
    }
    let mut out = Vec::new();
    if let Some(dir) = resolved.parent() {
        let package = dir
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code");
        out.push(package.join("cli.js"));
        if let Ok(entries) = std::fs::read_dir(package.join("bin")) {
            out.extend(entries.flatten().map(|e| e.path()));
        }
    }
    out
}

fn scan_file(path: &Path, found: &mut Scan) -> std::io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; CHUNK + OVERLAP];
    let mut carry = 0usize;
    loop {
        let read = file.read(&mut buf[carry..])?;
        if read == 0 {
            break;
        }
        let end = carry + read;
        found.feed(&buf[..end]);
        carry = OVERLAP.min(end);
        buf.copy_within(end - carry..end, 0);
    }
    Ok(())
}

#[derive(Default)]
struct Scan {
    ids: BTreeSet<(String, u32, u32)>,
    names: BTreeSet<(String, u32, u32, String)>,
}

impl Scan {
    fn feed(&mut self, bytes: &[u8]) {
        for at in memchr::memmem::find_iter(bytes, b"claude-") {
            if let Some(id) = parse_id(&bytes[at + 7..]) {
                self.ids.insert(id);
            }
        }
        for at in memchr::memmem::find_iter(bytes, b"Claude ") {
            if let Some(name) = parse_name(&bytes[at + 7..]) {
                self.names.insert(name);
            }
        }
    }

    /// Named models whose id the CLI also knows.
    fn models(&self) -> Vec<ClaudeModel> {
        let mut out: Vec<ClaudeModel> = self
            .names
            .iter()
            .filter(|(family, major, minor, _)| {
                self.ids.contains(&(family.clone(), *major, *minor))
            })
            .map(|(family, major, minor, display)| ClaudeModel {
                family: family.clone(),
                id: if *minor == 0 {
                    format!("claude-{family}-{major}")
                } else {
                    format!("claude-{family}-{major}-{minor}")
                },
                label: format!("Claude {display}"),
                version: (*major, *minor),
            })
            .collect();
        out.sort_by(|a, b| b.version.cmp(&a.version));
        out
    }
}

/// `opus-5-5…` → ("opus", 5, 5); `opus-5` → ("opus", 5, 0). Dated snapshot
/// suffixes (8 digits) are not minor versions.
fn parse_id(rest: &[u8]) -> Option<(String, u32, u32)> {
    let family_len = rest.iter().take_while(|b| b.is_ascii_lowercase()).count();
    if !(3..=12).contains(&family_len) || rest.get(family_len) != Some(&b'-') {
        return None;
    }
    let family = std::str::from_utf8(&rest[..family_len]).ok()?.to_string();
    let (major, used) = number(&rest[family_len + 1..], 2)?;
    let after = family_len + 1 + used;
    let minor = match (rest.get(after), rest.get(after + 1)) {
        (Some(b'-'), Some(d)) if d.is_ascii_digit() => {
            let digits = rest[after + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if digits > 2 {
                0
            } else {
                number(&rest[after + 1..], 2)?.0
            }
        }
        _ => 0,
    };
    Some((family, major, minor))
}

/// `Opus 5.5` → ("opus", 5, 5, "Opus 5.5").
fn parse_name(rest: &[u8]) -> Option<(String, u32, u32, String)> {
    let first = *rest.first()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    let word_len = 1 + rest[1..]
        .iter()
        .take_while(|b| b.is_ascii_lowercase())
        .count();
    if !(3..=12).contains(&word_len) || rest.get(word_len) != Some(&b' ') {
        return None;
    }
    let word = std::str::from_utf8(&rest[..word_len]).ok()?;
    let (major, used) = number(&rest[word_len + 1..], 2)?;
    let mut end = word_len + 1 + used;
    let minor = if rest.get(end) == Some(&b'.') {
        let (minor, used) = number(&rest[end + 1..], 2)?;
        end += 1 + used;
        minor
    } else {
        0
    };
    // "Opus 4.1.2" or "Opus 45" are not model names.
    if rest
        .get(end)
        .is_some_and(|b| b.is_ascii_digit() || *b == b'.')
    {
        return None;
    }
    let display = std::str::from_utf8(&rest[..end]).ok()?.to_string();
    Some((word.to_ascii_lowercase(), major, minor, display))
}

fn number(bytes: &[u8], max_digits: usize) -> Option<(u32, usize)> {
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > max_digits {
        return None;
    }
    Some((
        std::str::from_utf8(&bytes[..digits]).ok()?.parse().ok()?,
        digits,
    ))
}

fn filter(models: Vec<ClaudeModel>, families: &[&str]) -> Vec<ClaudeModel> {
    let mut out = Vec::new();
    for family in families {
        out.extend(
            models
                .iter()
                .filter(|m| m.family == *family)
                .take(VERSIONS_PER_FAMILY)
                .cloned(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lineup(bytes: &[u8], families: &[&str]) -> Vec<String> {
        let mut scan = Scan::default();
        scan.feed(bytes);
        filter(scan.models(), families)
            .into_iter()
            .map(|m| format!("{} {}", m.id, m.label))
            .collect()
    }

    #[test]
    fn reads_named_models_the_cli_also_has_ids_for() {
        let bytes = b"xx Claude Opus 5.5 yy Claude Opus 5 Claude Opus 4.8 Claude Opus 4 \
            Claude Mythos 5 Claude Sonnet 5 Claude Haiku 4.5 \
            claude-opus-5-5 claude-opus-5 claude-opus-4-8 claude-opus-4-20250514 claude-opus-4 \
            claude-mythos-5 claude-sonnet-5 claude-haiku-4-5-20251001";
        assert_eq!(
            lineup(bytes, &["opus", "sonnet", "haiku"]),
            [
                "claude-opus-5-5 Claude Opus 5.5",
                "claude-opus-5 Claude Opus 5",
                "claude-opus-4-8 Claude Opus 4.8",
                "claude-sonnet-5 Claude Sonnet 5",
                "claude-haiku-4-5 Claude Haiku 4.5",
            ]
        );
    }

    #[test]
    fn ignores_names_without_ids_and_dated_snapshots_as_versions() {
        let bytes = b"Claude Opus 6 claude-opus-4-20250514 Claude Opus 4";
        assert_eq!(lineup(bytes, &["opus"]), ["claude-opus-4 Claude Opus 4"]);
    }

    #[test]
    fn rejects_text_that_only_looks_like_a_model() {
        assert_eq!(parse_name(b"Code 2.1.283"), None);
        assert_eq!(parse_name(b"opus 5"), None);
        assert_eq!(parse_id(b"code-oauth-token"), None);
    }
}
