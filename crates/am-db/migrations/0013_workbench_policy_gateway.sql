CREATE TABLE workbench_session_groups (
    id          TEXT PRIMARY KEY,
    project_id  TEXT REFERENCES projects(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    color       TEXT NOT NULL DEFAULT 'slate',
    collapsed   INTEGER NOT NULL DEFAULT 0,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX idx_workbench_session_groups_project
    ON workbench_session_groups(project_id, sort_order, name);

ALTER TABLE agent_threads ADD COLUMN group_id TEXT REFERENCES workbench_session_groups(id) ON DELETE SET NULL;
ALTER TABLE agent_threads ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;
CREATE INDEX idx_agent_threads_group ON agent_threads(group_id, sort_order, updated_at DESC);

CREATE TABLE budget_policies (
    id                         TEXT PRIMARY KEY,
    name                       TEXT NOT NULL,
    enabled                    INTEGER NOT NULL DEFAULT 1,
    scope_kind                 TEXT NOT NULL,
    scope_id                   TEXT,
    provider                   TEXT,
    agent_kind                 TEXT,
    model                      TEXT,
    traffic_kind               TEXT,
    enforce_managed_sessions   INTEGER NOT NULL DEFAULT 1,
    enforce_api_gateway        INTEGER NOT NULL DEFAULT 0,
    soft_token_cap             INTEGER,
    hard_token_cap             INTEGER,
    soft_cost_cap_usd          REAL,
    hard_cost_cap_usd          REAL,
    warning_threshold          REAL,
    window                     TEXT,
    created_at                 TEXT NOT NULL,
    updated_at                 TEXT NOT NULL
);
CREATE INDEX idx_budget_policies_scope
    ON budget_policies(enabled, scope_kind, scope_id);
CREATE INDEX idx_budget_policies_subject
    ON budget_policies(provider, agent_kind, model, traffic_kind);

ALTER TABLE usage_ledger ADD COLUMN group_id TEXT;
ALTER TABLE usage_ledger ADD COLUMN traffic_kind TEXT NOT NULL DEFAULT 'managed_session';
ALTER TABLE usage_ledger ADD COLUMN api_source TEXT;
ALTER TABLE usage_ledger ADD COLUMN source_label TEXT;
ALTER TABLE usage_ledger ADD COLUMN request_count INTEGER NOT NULL DEFAULT 1;
ALTER TABLE usage_ledger ADD COLUMN status_code INTEGER;
CREATE INDEX idx_usage_ledger_group ON usage_ledger(group_id, ts DESC);
CREATE INDEX idx_usage_ledger_traffic ON usage_ledger(traffic_kind, provider, ts DESC);

ALTER TABLE policy_envelopes ADD COLUMN group_id TEXT;
ALTER TABLE policy_envelopes ADD COLUMN provider TEXT;
ALTER TABLE policy_envelopes ADD COLUMN traffic_kind TEXT;
ALTER TABLE policy_envelopes ADD COLUMN api_source TEXT;

ALTER TABLE policy_evaluations ADD COLUMN group_id TEXT;
ALTER TABLE policy_evaluations ADD COLUMN provider TEXT;
ALTER TABLE policy_evaluations ADD COLUMN traffic_kind TEXT;
ALTER TABLE policy_evaluations ADD COLUMN api_source TEXT;

CREATE TABLE repo_context_index (
    id            TEXT PRIMARY KEY,
    repo_id       TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    path          TEXT NOT NULL,
    language      TEXT,
    symbols_json  TEXT NOT NULL DEFAULT '[]',
    summary       TEXT NOT NULL DEFAULT '',
    size_bytes    INTEGER NOT NULL DEFAULT 0,
    content_hash  TEXT NOT NULL,
    indexed_at    TEXT NOT NULL,
    UNIQUE(repo_id, path)
);
CREATE INDEX idx_repo_context_index_repo ON repo_context_index(repo_id, indexed_at DESC);
CREATE INDEX idx_repo_context_index_path ON repo_context_index(repo_id, path);

CREATE TABLE run_specs (
    id                  TEXT PRIMARY KEY,
    project_id          TEXT,
    group_id            TEXT,
    session_id          TEXT,
    run_id              TEXT,
    policy_envelope_id  TEXT REFERENCES policy_envelopes(id) ON DELETE SET NULL,
    context_packet_id   TEXT,
    agent_kind          TEXT,
    runtime             TEXT,
    model               TEXT,
    prompt_hash         TEXT NOT NULL,
    repo_ids_json       TEXT NOT NULL DEFAULT '[]',
    settings_json       TEXT NOT NULL DEFAULT '{}',
    created_at          TEXT NOT NULL
);
CREATE INDEX idx_run_specs_session ON run_specs(session_id, created_at DESC);
CREATE INDEX idx_run_specs_group ON run_specs(group_id, created_at DESC);

CREATE TABLE api_gateway_configs (
    id                TEXT PRIMARY KEY,
    provider          TEXT NOT NULL,
    name              TEXT NOT NULL,
    enabled           INTEGER NOT NULL DEFAULT 0,
    enforce_policies  INTEGER NOT NULL DEFAULT 1,
    listen_host       TEXT NOT NULL DEFAULT '127.0.0.1',
    listen_port       INTEGER,
    upstream_base_url TEXT NOT NULL,
    auth_env_var      TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);
CREATE INDEX idx_api_gateway_configs_provider ON api_gateway_configs(provider, enabled);
