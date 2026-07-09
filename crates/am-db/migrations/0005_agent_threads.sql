-- First-class Workbench agent sessions. These sit beside project tasks: a
-- thread may belong to a project, but it does not require the user to create a
-- task before delegating work.

CREATE TABLE agent_threads (
    id              TEXT PRIMARY KEY,
    project_id      TEXT REFERENCES projects(id) ON DELETE SET NULL,
    title           TEXT NOT NULL,
    status          TEXT NOT NULL,
    active_agent    TEXT,
    preferred_agent TEXT,
    permission      TEXT NOT NULL DEFAULT 'workspace_write',
    original_agent  TEXT,
    fallback_agent  TEXT,
    limit_reset_at  TEXT,
    switch_back     INTEGER NOT NULL DEFAULT 1,
    handoff_state   TEXT NOT NULL DEFAULT 'none',
    objective       TEXT NOT NULL DEFAULT '',
    decisions       TEXT NOT NULL DEFAULT '',
    progress        TEXT NOT NULL DEFAULT '',
    open_questions  TEXT NOT NULL DEFAULT '',
    next_actions    TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE INDEX idx_agent_threads_project ON agent_threads(project_id);
CREATE INDEX idx_agent_threads_status ON agent_threads(status);
CREATE INDEX idx_agent_threads_updated ON agent_threads(updated_at);

CREATE TABLE agent_thread_repos (
    thread_id     TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    repo_id       TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    worktree_path TEXT,
    branch        TEXT,
    base_ref      TEXT,
    PRIMARY KEY (thread_id, repo_id)
);
CREATE INDEX idx_agent_thread_repos_repo ON agent_thread_repos(repo_id);

CREATE TABLE agent_turns (
    id               TEXT PRIMARY KEY,
    thread_id        TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    agent_kind       TEXT NOT NULL,
    agent_session_id TEXT,
    status           TEXT NOT NULL,
    permission       TEXT NOT NULL DEFAULT 'workspace_write',
    started_at       TEXT NOT NULL,
    ended_at         TEXT
);
CREATE INDEX idx_agent_turns_thread ON agent_turns(thread_id);

CREATE TABLE agent_thread_messages (
    id           TEXT PRIMARY KEY,
    thread_id    TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    turn_id      TEXT NOT NULL REFERENCES agent_turns(id) ON DELETE CASCADE,
    role         TEXT NOT NULL,
    type         TEXT NOT NULL,
    content_json TEXT NOT NULL,
    ts           TEXT NOT NULL
);
CREATE INDEX idx_agent_thread_messages_thread ON agent_thread_messages(thread_id);
CREATE INDEX idx_agent_thread_messages_turn ON agent_thread_messages(turn_id);

CREATE TABLE queued_turns (
    id          TEXT PRIMARY KEY,
    thread_id   TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    agent_kind  TEXT NOT NULL,
    permission  TEXT NOT NULL DEFAULT 'workspace_write',
    message     TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_queued_turns_thread ON queued_turns(thread_id, created_at);
