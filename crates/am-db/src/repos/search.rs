use am_proto::SearchHit;
use sqlx::SqlitePool;

use crate::DbError;

#[derive(sqlx::FromRow)]
struct HitRow {
    kind: String,
    entity_id: String,
    project_id: Option<String>,
    task_id: Option<String>,
    title: String,
    snippet: String,
}

impl From<HitRow> for SearchHit {
    fn from(r: HitRow) -> Self {
        SearchHit {
            kind: r.kind,
            entity_id: r.entity_id,
            project_id: r.project_id,
            task_id: r.task_id,
            title: r.title,
            snippet: r.snippet,
        }
    }
}

/// Turn free-text user input into a safe FTS5 prefix query. Each whitespace
/// token is reduced to its alphanumeric characters and turned into a prefix
/// term (`foo*`); terms are implicitly AND-ed. Returning `None` means there is
/// nothing searchable, so callers should yield no results. Building the query
/// from alphanumerics only means no FTS5 operator can be injected.
fn fts_query(raw: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in raw.split_whitespace() {
        let cleaned: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        if !cleaned.is_empty() {
            terms.push(format!("{cleaned}*"));
        }
    }
    (!terms.is_empty()).then(|| terms.join(" "))
}

const SELECT: &str = "SELECT kind, entity_id, project_id, task_id, title, \
    snippet(search_index, 1, '', '', '…', 12) AS snippet \
    FROM search_index WHERE search_index MATCH ?";

/// Full-text search across tasks, docs, and memory. `project_id = None` searches
/// every project. Results are ranked by BM25 relevance.
pub async fn search(
    pool: &SqlitePool,
    query: &str,
    project_id: Option<&str>,
    limit: i64,
) -> Result<Vec<SearchHit>, DbError> {
    let Some(match_query) = fts_query(query) else {
        return Ok(Vec::new());
    };

    let rows = match project_id {
        Some(pid) => {
            sqlx::query_as::<_, HitRow>(&format!(
                "{SELECT} AND project_id = ? ORDER BY bm25(search_index) LIMIT ?"
            ))
            .bind(&match_query)
            .bind(pid)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, HitRow>(&format!("{SELECT} ORDER BY bm25(search_index) LIMIT ?"))
                .bind(&match_query)
                .bind(limit)
                .fetch_all(pool)
                .await?
        }
    };

    Ok(rows.into_iter().map(Into::into).collect())
}
