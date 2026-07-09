ALTER TABLE work_plan_runs ADD COLUMN default_agent TEXT;
ALTER TABLE work_plan_runs ADD COLUMN default_permission TEXT;
ALTER TABLE work_plan_runs ADD COLUMN default_execution_backend TEXT;
ALTER TABLE work_plan_runs ADD COLUMN evaluator_policy_json TEXT;
ALTER TABLE work_plan_runs ADD COLUMN resume_after_node_id TEXT REFERENCES work_nodes(id) ON DELETE SET NULL;

CREATE TABLE work_gate_evaluations (
    id                     TEXT PRIMARY KEY,
    plan_run_id            TEXT REFERENCES work_plan_runs(id) ON DELETE SET NULL,
    node_id                TEXT NOT NULL REFERENCES work_nodes(id) ON DELETE CASCADE,
    evaluator_agent        TEXT,
    verdict                TEXT NOT NULL,
    confidence             REAL NOT NULL DEFAULT 0,
    findings_json          TEXT NOT NULL DEFAULT '[]',
    required_followups_json TEXT NOT NULL DEFAULT '[]',
    validation_commands_json TEXT NOT NULL DEFAULT '[]',
    rationale              TEXT NOT NULL DEFAULT '',
    raw_output             TEXT NOT NULL DEFAULT '',
    created_at             TEXT NOT NULL
);

CREATE INDEX idx_work_gate_evaluations_node ON work_gate_evaluations(node_id, created_at DESC);
CREATE INDEX idx_work_gate_evaluations_plan ON work_gate_evaluations(plan_run_id, created_at DESC);
