-- M6: durable memory notes. Project-scoped when task_id IS NULL, task-scoped
-- otherwise. Short pinned facts the team and agents should remember (decisions,
-- gotchas, conventions) that survive across tasks and agent switches.
CREATE TABLE memory_notes (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    task_id     TEXT REFERENCES tasks(id) ON DELETE CASCADE,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX idx_memory_project ON memory_notes(project_id);
CREATE INDEX idx_memory_task ON memory_notes(task_id);
