CREATE TABLE work_plan_runs (
    id               TEXT PRIMARY KEY,
    project_id       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    gate_mode        TEXT NOT NULL DEFAULT 'auto_evaluate',
    state            TEXT NOT NULL,
    max_active_runs  INTEGER NOT NULL DEFAULT 4,
    total_count      INTEGER NOT NULL DEFAULT 0,
    completed_count  INTEGER NOT NULL DEFAULT 0,
    active_count     INTEGER NOT NULL DEFAULT 0,
    blocked_count    INTEGER NOT NULL DEFAULT 0,
    error            TEXT,
    started_at       TEXT NOT NULL,
    ended_at         TEXT,
    updated_at       TEXT NOT NULL
);
CREATE INDEX idx_work_plan_runs_project ON work_plan_runs(project_id, started_at);

ALTER TABLE work_runs ADD COLUMN plan_run_id TEXT REFERENCES work_plan_runs(id) ON DELETE SET NULL;
CREATE INDEX idx_work_runs_plan ON work_runs(plan_run_id);
