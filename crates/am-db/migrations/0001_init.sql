-- AgentManager foundational schema.
-- IDs are UUID v4 stored as TEXT. Timestamps are RFC3339 UTC stored as TEXT.

CREATE TABLE projects (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

-- Repositories connected to a project (single- or multi-repo projects).
CREATE TABLE repos (
    id             TEXT PRIMARY KEY,
    project_id     TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    kind           TEXT NOT NULL,          -- 'local' | 'github'
    local_path     TEXT,                   -- canonical clone / working path
    remote_url     TEXT,
    default_branch TEXT NOT NULL DEFAULT 'main',
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);
CREATE INDEX idx_repos_project ON repos(project_id);

CREATE TABLE tasks (
    id            TEXT PRIMARY KEY,
    project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title         TEXT NOT NULL,
    description   TEXT,
    status        TEXT NOT NULL,           -- TaskStatus
    priority      TEXT NOT NULL,           -- TaskPriority
    primary_agent TEXT,                    -- AgentKind | NULL
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX idx_tasks_project ON tasks(project_id);
CREATE INDEX idx_tasks_status ON tasks(status);

-- Per-task association to a repo + its isolated worktree (multi-repo capable).
CREATE TABLE task_repos (
    task_id      TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    repo_id      TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    worktree_path TEXT,
    branch       TEXT,
    PRIMARY KEY (task_id, repo_id)
);

-- Agent-independent task context (the "intent" that survives agent switches).
CREATE TABLE task_context (
    task_id       TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    objective     TEXT NOT NULL DEFAULT '',
    requirements  TEXT NOT NULL DEFAULT '',
    decisions     TEXT NOT NULL DEFAULT '',
    progress      TEXT NOT NULL DEFAULT '',
    open_questions TEXT NOT NULL DEFAULT '',
    next_actions  TEXT NOT NULL DEFAULT '',
    updated_at    TEXT NOT NULL
);

-- Per-agent install/availability snapshot.
CREATE TABLE agents (
    kind           TEXT PRIMARY KEY,       -- AgentKind
    install_status TEXT NOT NULL,          -- 'installed' | 'not_installed' | 'unauthenticated' | 'error'
    version        TEXT,
    availability   TEXT NOT NULL,          -- AvailabilityState
    reset_at       TEXT,
    last_checked   TEXT
);

-- A single agent run against a task.
CREATE TABLE sessions (
    id               TEXT PRIMARY KEY,
    task_id          TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    agent_kind       TEXT NOT NULL,
    agent_session_id TEXT,                 -- provider's own resumable session id
    status           TEXT NOT NULL,
    started_at       TEXT NOT NULL,
    ended_at         TEXT
);
CREATE INDEX idx_sessions_task ON sessions(task_id);

-- Normalized transcript: one row per normalized event.
CREATE TABLE messages (
    id           TEXT PRIMARY KEY,
    session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role         TEXT NOT NULL,            -- 'system' | 'assistant' | 'tool' | 'user'
    type         TEXT NOT NULL,            -- normalized event variant
    content_json TEXT NOT NULL,
    ts           TEXT NOT NULL
);
CREATE INDEX idx_messages_session ON messages(session_id);

-- Activity log / timeline.
CREATE TABLE events (
    id          TEXT PRIMARY KEY,
    project_id  TEXT REFERENCES projects(id) ON DELETE CASCADE,
    task_id     TEXT REFERENCES tasks(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    ts          TEXT NOT NULL
);
CREATE INDEX idx_events_project ON events(project_id);
CREATE INDEX idx_events_task ON events(task_id);
CREATE INDEX idx_events_ts ON events(ts);

-- Project knowledge / documentation.
CREATE TABLE knowledge_docs (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX idx_knowledge_project ON knowledge_docs(project_id);

-- Generic key/value settings.
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
