//! Relevance ranking for context-packet file selection.
//!
//! BM25 computed over the candidate set (per-repo summaries are small, so the
//! corpus statistics are cheap to derive on the fly), plus structural boosts
//! the pure text model can't see: files touched by prerequisite work, fresh
//! index entries, and path segments matching the work title.

use std::collections::{HashMap, HashSet};

use am_db::repos::work_graph::RepoContextFile;
use chrono::{DateTime, Utc};

const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;
/// Scores are mapped into this band so downstream budget logic (which treats
/// >=0.8 as must-keep) composes with the fixed scores of other source kinds.
const SCORE_FLOOR: f64 = 0.45;
const SCORE_SPAN: f64 = 0.45;
const DEPENDENCY_BOOST: f64 = 0.20;
const RECENCY_BOOST: f64 = 0.05;
const PATH_TITLE_BOOST: f64 = 0.10;
const SCORE_CAP: f64 = 0.98;
const RECENCY_WINDOW_SECS: i64 = 3600;

/// Signals beyond text similarity.
#[derive(Default)]
pub(crate) struct ScoreBoosts {
    /// Paths changed by completed prerequisite (blocker/parent) work — the
    /// files a dependent task most likely needs to build on.
    pub dependency_paths: HashSet<String>,
}

/// Rank candidate files for a work item, best first.
pub(crate) fn rank_context_files(
    title: &str,
    description: Option<&str>,
    files: Vec<RepoContextFile>,
    boosts: &ScoreBoosts,
    now: DateTime<Utc>,
) -> Vec<(f64, RepoContextFile)> {
    let mut query_terms: HashMap<String, f64> = HashMap::new();
    for token in tokenize(title) {
        *query_terms.entry(token).or_insert(0.0) += 1.0;
    }
    if let Some(description) = description {
        for token in tokenize(description).into_iter().take(24) {
            *query_terms.entry(token).or_insert(0.0) += 0.5;
        }
    }

    // Corpus statistics over the candidate set.
    let docs: Vec<Vec<String>> = files
        .iter()
        .map(|file| {
            let mut tokens = tokenize(&file.path);
            tokens.extend(tokenize(&file.summary));
            tokens.extend(tokenize(&file.symbols_json));
            tokens
        })
        .collect();
    let doc_count = docs.len().max(1) as f64;
    let avg_len = (docs.iter().map(Vec::len).sum::<usize>() as f64 / doc_count).max(1.0);
    let mut doc_freq: HashMap<&str, f64> = HashMap::new();
    for doc in &docs {
        let unique: HashSet<&str> = doc.iter().map(String::as_str).collect();
        for term in unique {
            if query_terms.contains_key(term) {
                *doc_freq.entry(term).or_insert(0.0) += 1.0;
            }
        }
    }

    let bm25_scores: Vec<f64> = docs
        .iter()
        .map(|doc| {
            let len = doc.len() as f64;
            let mut tf: HashMap<&str, f64> = HashMap::new();
            for term in doc {
                if query_terms.contains_key(term.as_str()) {
                    *tf.entry(term.as_str()).or_insert(0.0) += 1.0;
                }
            }
            query_terms
                .iter()
                .map(|(term, weight)| {
                    let Some(tf) = tf.get(term.as_str()) else {
                        return 0.0;
                    };
                    let df = doc_freq.get(term.as_str()).copied().unwrap_or(0.0);
                    let idf = ((doc_count - df + 0.5) / (df + 0.5) + 1.0).ln();
                    let norm = tf * (BM25_K1 + 1.0)
                        / (tf + BM25_K1 * (1.0 - BM25_B + BM25_B * len / avg_len));
                    weight * idf * norm
                })
                .sum()
        })
        .collect();
    let max_bm25 = bm25_scores.iter().copied().fold(0.0_f64, f64::max);

    let title_tokens: HashSet<String> = tokenize(title).into_iter().collect();
    let mut scored: Vec<(f64, RepoContextFile)> = files
        .into_iter()
        .zip(bm25_scores)
        .map(|(file, bm25)| {
            let text_score = if max_bm25 > 0.0 {
                SCORE_FLOOR + SCORE_SPAN * (bm25 / max_bm25)
            } else {
                SCORE_FLOOR + 0.07
            };
            let mut score = text_score;
            if path_matches_dependency(&file.path, &boosts.dependency_paths) {
                score += DEPENDENCY_BOOST;
            }
            if (now - file.indexed_at).num_seconds() < RECENCY_WINDOW_SECS {
                score += RECENCY_BOOST;
            }
            if path_segment_matches(&file.path, &title_tokens) {
                score += PATH_TITLE_BOOST;
            }
            (score.min(SCORE_CAP), file)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() >= 3)
        .map(str::to_lowercase)
        .collect()
}

fn path_matches_dependency(path: &str, dependency_paths: &HashSet<String>) -> bool {
    dependency_paths
        .iter()
        .any(|dep| dep == path || dep.ends_with(path) || path.ends_with(dep.as_str()))
}

fn path_segment_matches(path: &str, title_tokens: &HashSet<String>) -> bool {
    path.split(['/', '.'])
        .any(|segment| title_tokens.contains(&segment.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use am_proto::now;

    fn file(path: &str, summary: &str) -> RepoContextFile {
        RepoContextFile {
            id: path.to_string(),
            repo_id: "repo".into(),
            path: path.to_string(),
            language: None,
            symbols_json: "[]".into(),
            summary: summary.to_string(),
            size_bytes: 100,
            mtime_ms: 0,
            content_hash: "h".into(),
            // Outside the recency window so text relevance dominates.
            indexed_at: now() - chrono::Duration::hours(2),
        }
    }

    #[test]
    fn relevant_files_rank_above_unrelated_ones() {
        let files = vec![
            file(
                "src/billing/invoice.rs",
                "Path: src/billing/invoice.rs\ninvoice generation and totals",
            ),
            file(
                "src/util/strings.rs",
                "Path: src/util/strings.rs\nstring helpers",
            ),
            file(
                "src/billing/tax.rs",
                "Path: src/billing/tax.rs\ntax rates for invoice totals",
            ),
        ];
        let ranked = rank_context_files(
            "Fix invoice totals",
            Some("The invoice tax calculation rounds incorrectly."),
            files,
            &ScoreBoosts::default(),
            now(),
        );
        let order: Vec<&str> = ranked.iter().map(|(_, f)| f.path.as_str()).collect();
        assert_eq!(order.last().copied(), Some("src/util/strings.rs"));
        assert!(ranked[0].0 > ranked[2].0);
    }

    #[test]
    fn rare_terms_outweigh_common_ones() {
        // "handler" appears everywhere; "webhook" is discriminative.
        let files = vec![
            file("src/a.rs", "handler handler handler"),
            file("src/webhook.rs", "webhook handler"),
            file("src/b.rs", "handler handler"),
        ];
        let ranked = rank_context_files(
            "webhook handler retries",
            None,
            files,
            &ScoreBoosts::default(),
            now(),
        );
        assert_eq!(ranked[0].1.path, "src/webhook.rs");
    }

    #[test]
    fn dependency_and_recency_boosts_apply() {
        let mut fresh = file("src/other.rs", "unrelated content");
        fresh.indexed_at = now();
        let files = vec![file("src/dep.rs", "unrelated content"), fresh];

        let mut boosts = ScoreBoosts::default();
        boosts.dependency_paths.insert("src/dep.rs".into());
        let ranked = rank_context_files("completely different title", None, files, &boosts, now());

        let dep = ranked.iter().find(|(_, f)| f.path == "src/dep.rs").unwrap();
        let other = ranked
            .iter()
            .find(|(_, f)| f.path == "src/other.rs")
            .unwrap();
        // Dependency boost (0.2) beats recency boost (0.05).
        assert!(dep.0 > other.0);
        assert!(other.0 > SCORE_FLOOR, "recency boost applied");
    }

    #[test]
    fn path_segment_matching_title_gets_boosted() {
        let files = vec![
            file("src/gateway/proxy.rs", "some content"),
            file("src/misc.rs", "some content"),
        ];
        let ranked = rank_context_files(
            "gateway streaming",
            None,
            files,
            &ScoreBoosts::default(),
            now(),
        );
        assert_eq!(ranked[0].1.path, "src/gateway/proxy.rs");
    }
}
