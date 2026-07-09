-- Cloud continuation runs: provider-hosted legs (Codex Cloud / Claude Code
-- web) that keep a thread's work moving while the machine is unavailable.

CREATE TABLE cloud_runs (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    agent_kind TEXT NOT NULL,
    provider_task_id TEXT,
    url TEXT,
    env_id TEXT,
    branch TEXT,
    base_commit TEXT,
    launch_commit TEXT,
    status TEXT NOT NULL,
    trigger TEXT NOT NULL DEFAULT 'manual',
    launched_at TEXT NOT NULL,
    last_activity_at TEXT,
    last_seen_commit TEXT,
    reclaimed_at TEXT,
    failure_reason TEXT
);

CREATE INDEX idx_cloud_runs_thread ON cloud_runs(thread_id, launched_at);
CREATE INDEX idx_cloud_runs_status ON cloud_runs(status);
