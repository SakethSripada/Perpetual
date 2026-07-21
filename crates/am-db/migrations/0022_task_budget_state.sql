-- Private enforcement state for graceful task budgets. This table is never
-- included in agent-thread snapshots, transcript events, or webview payloads.
CREATE TABLE agent_thread_budget_state (
    thread_id  TEXT PRIMARY KEY REFERENCES agent_threads(id) ON DELETE CASCADE,
    state_json TEXT NOT NULL DEFAULT '{}',
    updated_at TEXT NOT NULL
);
