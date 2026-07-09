CREATE TABLE policy_documents (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    description  TEXT,
    enabled      INTEGER NOT NULL DEFAULT 1,
    priority     INTEGER NOT NULL DEFAULT 0,
    rules_json   TEXT NOT NULL DEFAULT '[]',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX idx_policy_documents_enabled ON policy_documents(enabled, priority);

CREATE TABLE policy_bindings (
    id           TEXT PRIMARY KEY,
    document_id  TEXT NOT NULL REFERENCES policy_documents(id) ON DELETE CASCADE,
    scope_kind   TEXT NOT NULL,
    scope_id     TEXT,
    created_at   TEXT NOT NULL
);
CREATE INDEX idx_policy_bindings_document ON policy_bindings(document_id);
CREATE INDEX idx_policy_bindings_scope ON policy_bindings(scope_kind, scope_id);

CREATE TABLE policy_envelopes (
    id                TEXT PRIMARY KEY,
    request_id        TEXT NOT NULL,
    decision_id       TEXT NOT NULL,
    envelope_json     TEXT NOT NULL,
    project_id        TEXT,
    session_id        TEXT,
    run_id            TEXT,
    agent_kind        TEXT NOT NULL,
    runtime           TEXT NOT NULL,
    action            TEXT NOT NULL,
    created_at        TEXT NOT NULL
);
CREATE INDEX idx_policy_envelopes_project ON policy_envelopes(project_id, created_at DESC);
CREATE INDEX idx_policy_envelopes_session ON policy_envelopes(session_id);
CREATE INDEX idx_policy_envelopes_run ON policy_envelopes(run_id);

CREATE TABLE policy_evaluations (
    id                TEXT PRIMARY KEY,
    request_id        TEXT NOT NULL,
    envelope_id       TEXT,
    request_json      TEXT NOT NULL,
    decision_json     TEXT NOT NULL,
    action            TEXT NOT NULL,
    project_id        TEXT,
    session_id        TEXT,
    run_id            TEXT,
    created_at        TEXT NOT NULL
);
CREATE INDEX idx_policy_evaluations_project ON policy_evaluations(project_id, created_at DESC);
CREATE INDEX idx_policy_evaluations_envelope ON policy_evaluations(envelope_id);

CREATE TABLE usage_ledger (
    id                    TEXT PRIMARY KEY,
    ts                    TEXT NOT NULL,
    org_id                TEXT,
    team_id               TEXT,
    user_id               TEXT,
    project_id            TEXT,
    repo_id               TEXT,
    session_id            TEXT,
    run_id                TEXT,
    agent_kind            TEXT,
    provider              TEXT,
    model                 TEXT,
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    estimated_cost_usd    REAL,
    policy_envelope_id    TEXT REFERENCES policy_envelopes(id) ON DELETE SET NULL
);
CREATE INDEX idx_usage_ledger_project ON usage_ledger(project_id, ts DESC);
CREATE INDEX idx_usage_ledger_session ON usage_ledger(session_id, ts DESC);
CREATE INDEX idx_usage_ledger_model ON usage_ledger(provider, model, ts DESC);

CREATE TABLE budget_windows (
    id                    TEXT PRIMARY KEY,
    scope                 TEXT NOT NULL,
    subject_id            TEXT,
    window                TEXT NOT NULL,
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    estimated_cost_usd    REAL,
    updated_at            TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_budget_windows_scope ON budget_windows(scope, subject_id, window);

CREATE TABLE policy_approval_grants (
    id              TEXT PRIMARY KEY,
    request_hash    TEXT NOT NULL,
    status          TEXT NOT NULL,
    reason          TEXT,
    created_at      TEXT NOT NULL,
    resolved_at     TEXT
);
CREATE INDEX idx_policy_approval_grants_hash ON policy_approval_grants(request_hash, status);
CREATE INDEX idx_policy_approval_grants_status ON policy_approval_grants(status, created_at);

CREATE TABLE policy_audit_exports (
    id          TEXT PRIMARY KEY,
    created_at  TEXT NOT NULL,
    format      TEXT NOT NULL,
    body        TEXT NOT NULL
);

ALTER TABLE sessions ADD COLUMN policy_envelope_id TEXT REFERENCES policy_envelopes(id) ON DELETE SET NULL;
ALTER TABLE agent_turns ADD COLUMN policy_envelope_id TEXT REFERENCES policy_envelopes(id) ON DELETE SET NULL;
ALTER TABLE queued_turns ADD COLUMN policy_envelope_id TEXT REFERENCES policy_envelopes(id) ON DELETE SET NULL;
ALTER TABLE work_plan_runs ADD COLUMN policy_envelope_id TEXT REFERENCES policy_envelopes(id) ON DELETE SET NULL;
ALTER TABLE work_runs ADD COLUMN policy_envelope_id TEXT REFERENCES policy_envelopes(id) ON DELETE SET NULL;
