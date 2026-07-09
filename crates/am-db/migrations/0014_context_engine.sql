-- Context engine: incremental indexing state + lossless handoff archive.

-- File mtime (ms since epoch) lets the indexer skip unchanged files by
-- size+mtime without reading or hashing them.
ALTER TABLE repo_context_index ADD COLUMN mtime_ms INTEGER NOT NULL DEFAULT 0;

-- Per-repo walk state: when HEAD and the dirty-file digest are unchanged and
-- the last walk is recent, the whole filesystem walk is skipped.
CREATE TABLE IF NOT EXISTS repo_index_state (
    repo_id TEXT PRIMARY KEY REFERENCES repos(id) ON DELETE CASCADE,
    head_commit TEXT,
    dirty_digest TEXT,
    last_walk_at TEXT NOT NULL,
    file_count INTEGER NOT NULL DEFAULT 0
);

-- Append-only archive of session handoffs. The rendered TASK_CONTEXT.md keeps
-- a bounded rolling window; this table preserves the full history and feeds
-- blocker/sibling context packets.
CREATE TABLE IF NOT EXISTS task_handoffs (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    status TEXT NOT NULL,
    summary TEXT NOT NULL,
    changed_files_json TEXT NOT NULL DEFAULT '[]',
    next_actions TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_task_handoffs_task
    ON task_handoffs(task_id, created_at DESC);

-- Covering index for ledger-ordered context retrieval per repo set.
CREATE INDEX IF NOT EXISTS idx_repo_context_index_repo_path
    ON repo_context_index(repo_id, path);

-- Token-aware budgeting: rough token estimate per persisted inclusion.
ALTER TABLE context_inclusions ADD COLUMN estimated_tokens INTEGER NOT NULL DEFAULT 0;
