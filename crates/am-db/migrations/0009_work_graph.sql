-- Canonical project work graph. Existing tasks and Workbench threads are
-- projected into work_nodes and kept synchronized by triggers so old APIs keep
-- working while new project views use one shared graph.

ALTER TABLE tasks ADD COLUMN work_node_id TEXT;
ALTER TABLE agent_threads ADD COLUMN work_node_id TEXT;

CREATE TABLE work_nodes (
    id            TEXT PRIMARY KEY,
    project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    parent_id     TEXT REFERENCES work_nodes(id) ON DELETE SET NULL,
    task_id       TEXT UNIQUE REFERENCES tasks(id) ON DELETE SET NULL,
    thread_id     TEXT UNIQUE REFERENCES agent_threads(id) ON DELETE SET NULL,
    kind          TEXT NOT NULL,
    title         TEXT NOT NULL,
    description   TEXT,
    status        TEXT NOT NULL,
    priority      TEXT NOT NULL DEFAULT 'medium',
    primary_agent TEXT,
    position_x    REAL NOT NULL DEFAULT 0,
    position_y    REAL NOT NULL DEFAULT 0,
    sort_order    INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX idx_work_nodes_project ON work_nodes(project_id, sort_order, updated_at);
CREATE INDEX idx_work_nodes_parent ON work_nodes(parent_id);
CREATE INDEX idx_work_nodes_status ON work_nodes(status);
CREATE INDEX idx_work_nodes_task ON work_nodes(task_id);
CREATE INDEX idx_work_nodes_thread ON work_nodes(thread_id);

CREATE TABLE work_edges (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source_id   TEXT NOT NULL REFERENCES work_nodes(id) ON DELETE CASCADE,
    target_id   TEXT NOT NULL REFERENCES work_nodes(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    label       TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE(source_id, target_id, kind)
);
CREATE INDEX idx_work_edges_project ON work_edges(project_id);
CREATE INDEX idx_work_edges_source ON work_edges(source_id);
CREATE INDEX idx_work_edges_target ON work_edges(target_id);

CREATE TABLE work_node_repos (
    node_id           TEXT NOT NULL REFERENCES work_nodes(id) ON DELETE CASCADE,
    repo_id           TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    worktree_path     TEXT,
    branch            TEXT,
    base_ref          TEXT,
    workspace_backend TEXT NOT NULL DEFAULT 'host',
    path_globs        TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (node_id, repo_id)
);
CREATE INDEX idx_work_node_repos_repo ON work_node_repos(repo_id);

CREATE TABLE work_runs (
    id          TEXT PRIMARY KEY,
    node_id     TEXT NOT NULL REFERENCES work_nodes(id) ON DELETE CASCADE,
    task_id     TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    thread_id   TEXT REFERENCES agent_threads(id) ON DELETE SET NULL,
    agent_kind  TEXT NOT NULL,
    run_ref     TEXT NOT NULL,
    state       TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    ended_at    TEXT
);
CREATE INDEX idx_work_runs_node ON work_runs(node_id, started_at);

CREATE TABLE context_packets (
    id           TEXT PRIMARY KEY,
    node_id      TEXT NOT NULL REFERENCES work_nodes(id) ON DELETE CASCADE,
    budget_bytes INTEGER NOT NULL,
    used_bytes   INTEGER NOT NULL,
    summary      TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL
);
CREATE INDEX idx_context_packets_node ON context_packets(node_id, created_at);

CREATE TABLE context_inclusions (
    packet_id   TEXT NOT NULL REFERENCES context_packets(id) ON DELETE CASCADE,
    source_kind TEXT NOT NULL,
    entity_id   TEXT,
    title       TEXT NOT NULL DEFAULT '',
    snippet     TEXT NOT NULL DEFAULT '',
    reason      TEXT NOT NULL DEFAULT '',
    score       REAL NOT NULL DEFAULT 0,
    bytes       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_context_inclusions_packet ON context_inclusions(packet_id);

CREATE TABLE work_locks (
    node_id     TEXT NOT NULL REFERENCES work_nodes(id) ON DELETE CASCADE,
    repo_id     TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    path_glob   TEXT NOT NULL DEFAULT '*',
    mode        TEXT NOT NULL DEFAULT 'write',
    acquired_at TEXT NOT NULL,
    PRIMARY KEY (repo_id, path_glob, mode)
);
CREATE INDEX idx_work_locks_node ON work_locks(node_id);

INSERT INTO work_nodes (
    id, project_id, parent_id, task_id, thread_id, kind, title, description,
    status, priority, primary_agent, position_x, position_y, sort_order,
    created_at, updated_at
)
SELECT
    'wn-task-' || id, project_id, NULL, id, NULL, 'task', title, description,
    status, priority, primary_agent, 0, 0,
    ROW_NUMBER() OVER (PARTITION BY project_id ORDER BY created_at ASC),
    created_at, updated_at
FROM tasks;

UPDATE tasks SET work_node_id = 'wn-task-' || id WHERE work_node_id IS NULL;

INSERT INTO work_nodes (
    id, project_id, parent_id, task_id, thread_id, kind, title, description,
    status, priority, primary_agent, position_x, position_y, sort_order,
    created_at, updated_at
)
SELECT
    'wn-thread-' || id, project_id, NULL, NULL, id, 'session', title, objective,
    status, 'medium', COALESCE(active_agent, preferred_agent), 360, 0,
    ROW_NUMBER() OVER (PARTITION BY project_id ORDER BY created_at ASC),
    created_at, updated_at
FROM agent_threads
WHERE project_id IS NOT NULL;

UPDATE agent_threads SET work_node_id = 'wn-thread-' || id
WHERE work_node_id IS NULL AND project_id IS NOT NULL;

INSERT OR REPLACE INTO work_node_repos (
    node_id, repo_id, worktree_path, branch, base_ref, workspace_backend
)
SELECT t.work_node_id, tr.repo_id, tr.worktree_path, tr.branch, tr.base_ref, tr.workspace_backend
FROM task_repos tr
JOIN tasks t ON t.id = tr.task_id
WHERE t.work_node_id IS NOT NULL;

INSERT OR REPLACE INTO work_node_repos (
    node_id, repo_id, worktree_path, branch, base_ref, workspace_backend
)
SELECT at.work_node_id, atr.repo_id, atr.worktree_path, atr.branch, atr.base_ref, atr.workspace_backend
FROM agent_thread_repos atr
JOIN agent_threads at ON at.id = atr.thread_id
WHERE at.work_node_id IS NOT NULL;

CREATE TRIGGER trg_tasks_work_node_ai AFTER INSERT ON tasks BEGIN
    INSERT OR IGNORE INTO work_nodes (
        id, project_id, parent_id, task_id, thread_id, kind, title, description,
        status, priority, primary_agent, position_x, position_y, sort_order,
        created_at, updated_at
    )
    VALUES (
        'wn-task-' || new.id, new.project_id, NULL, new.id, NULL, 'task',
        new.title, new.description, new.status, new.priority, new.primary_agent,
        0, 0,
        COALESCE((SELECT MAX(sort_order) + 1 FROM work_nodes WHERE project_id = new.project_id), 0),
        new.created_at, new.updated_at
    );
    UPDATE tasks SET work_node_id = 'wn-task-' || new.id WHERE id = new.id;
END;

CREATE TRIGGER trg_tasks_work_node_au AFTER UPDATE ON tasks BEGIN
    UPDATE work_nodes SET
        project_id = new.project_id,
        task_id = new.id,
        kind = 'task',
        title = new.title,
        description = new.description,
        status = new.status,
        priority = new.priority,
        primary_agent = new.primary_agent,
        updated_at = new.updated_at
    WHERE id = COALESCE(new.work_node_id, 'wn-task-' || new.id);
END;

CREATE TRIGGER trg_tasks_work_node_ad AFTER DELETE ON tasks BEGIN
    DELETE FROM work_nodes WHERE id = old.work_node_id OR task_id = old.id;
END;

CREATE TRIGGER trg_agent_threads_work_node_ai AFTER INSERT ON agent_threads
WHEN new.project_id IS NOT NULL
BEGIN
    INSERT OR IGNORE INTO work_nodes (
        id, project_id, parent_id, task_id, thread_id, kind, title, description,
        status, priority, primary_agent, position_x, position_y, sort_order,
        created_at, updated_at
    )
    VALUES (
        'wn-thread-' || new.id, new.project_id, NULL, NULL, new.id, 'session',
        new.title, new.objective, new.status, 'medium', COALESCE(new.active_agent, new.preferred_agent),
        360, 0,
        COALESCE((SELECT MAX(sort_order) + 1 FROM work_nodes WHERE project_id = new.project_id), 0),
        new.created_at, new.updated_at
    );
    UPDATE agent_threads SET work_node_id = 'wn-thread-' || new.id WHERE id = new.id;
END;

CREATE TRIGGER trg_agent_threads_work_node_au AFTER UPDATE ON agent_threads
WHEN new.project_id IS NOT NULL
BEGIN
    UPDATE work_nodes SET
        project_id = new.project_id,
        thread_id = new.id,
        kind = 'session',
        title = new.title,
        description = new.objective,
        status = new.status,
        primary_agent = COALESCE(new.active_agent, new.preferred_agent),
        updated_at = new.updated_at
    WHERE id = COALESCE(new.work_node_id, 'wn-thread-' || new.id);
END;

CREATE TRIGGER trg_agent_threads_work_node_project_null_au AFTER UPDATE ON agent_threads
WHEN new.project_id IS NULL
BEGIN
    DELETE FROM work_nodes WHERE id = old.work_node_id OR thread_id = old.id;
END;

CREATE TRIGGER trg_agent_threads_work_node_ad AFTER DELETE ON agent_threads BEGIN
    DELETE FROM work_nodes WHERE id = old.work_node_id OR thread_id = old.id;
END;

CREATE TRIGGER trg_task_repos_work_node_ai AFTER INSERT ON task_repos BEGIN
    INSERT INTO work_node_repos (
        node_id, repo_id, worktree_path, branch, base_ref, workspace_backend
    )
    SELECT work_node_id, new.repo_id, new.worktree_path, new.branch, new.base_ref, new.workspace_backend
    FROM tasks WHERE id = new.task_id AND work_node_id IS NOT NULL
    ON CONFLICT(node_id, repo_id) DO UPDATE SET
        worktree_path = excluded.worktree_path,
        branch = excluded.branch,
        base_ref = excluded.base_ref,
        workspace_backend = excluded.workspace_backend;
END;

CREATE TRIGGER trg_task_repos_work_node_au AFTER UPDATE ON task_repos BEGIN
    INSERT INTO work_node_repos (
        node_id, repo_id, worktree_path, branch, base_ref, workspace_backend
    )
    SELECT work_node_id, new.repo_id, new.worktree_path, new.branch, new.base_ref, new.workspace_backend
    FROM tasks WHERE id = new.task_id AND work_node_id IS NOT NULL
    ON CONFLICT(node_id, repo_id) DO UPDATE SET
        worktree_path = excluded.worktree_path,
        branch = excluded.branch,
        base_ref = excluded.base_ref,
        workspace_backend = excluded.workspace_backend;
END;

CREATE TRIGGER trg_task_repos_work_node_ad AFTER DELETE ON task_repos BEGIN
    DELETE FROM work_node_repos
    WHERE repo_id = old.repo_id
      AND node_id = (SELECT work_node_id FROM tasks WHERE id = old.task_id);
END;

CREATE TRIGGER trg_thread_repos_work_node_ai AFTER INSERT ON agent_thread_repos BEGIN
    INSERT INTO work_node_repos (
        node_id, repo_id, worktree_path, branch, base_ref, workspace_backend
    )
    SELECT work_node_id, new.repo_id, new.worktree_path, new.branch, new.base_ref, new.workspace_backend
    FROM agent_threads WHERE id = new.thread_id AND work_node_id IS NOT NULL
    ON CONFLICT(node_id, repo_id) DO UPDATE SET
        worktree_path = excluded.worktree_path,
        branch = excluded.branch,
        base_ref = excluded.base_ref,
        workspace_backend = excluded.workspace_backend;
END;

CREATE TRIGGER trg_thread_repos_work_node_au AFTER UPDATE ON agent_thread_repos BEGIN
    INSERT INTO work_node_repos (
        node_id, repo_id, worktree_path, branch, base_ref, workspace_backend
    )
    SELECT work_node_id, new.repo_id, new.worktree_path, new.branch, new.base_ref, new.workspace_backend
    FROM agent_threads WHERE id = new.thread_id AND work_node_id IS NOT NULL
    ON CONFLICT(node_id, repo_id) DO UPDATE SET
        worktree_path = excluded.worktree_path,
        branch = excluded.branch,
        base_ref = excluded.base_ref,
        workspace_backend = excluded.workspace_backend;
END;

CREATE TRIGGER trg_thread_repos_work_node_ad AFTER DELETE ON agent_thread_repos BEGIN
    DELETE FROM work_node_repos
    WHERE repo_id = old.repo_id
      AND node_id = (SELECT work_node_id FROM agent_threads WHERE id = old.thread_id);
END;
