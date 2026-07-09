-- Rented open-model compute metadata. Agent execution still runs locally;
-- these fields describe where inference is served.

ALTER TABLE tasks ADD COLUMN model TEXT;
ALTER TABLE tasks ADD COLUMN model_target TEXT NOT NULL DEFAULT 'frontier_default';
ALTER TABLE tasks ADD COLUMN compute_lease_id TEXT;
ALTER TABLE tasks ADD COLUMN compute_provider TEXT;
ALTER TABLE tasks ADD COLUMN estimated_compute_cost_usd REAL;
ALTER TABLE tasks ADD COLUMN fallback_model_target TEXT;

ALTER TABLE sessions ADD COLUMN model_target TEXT NOT NULL DEFAULT 'frontier_default';
ALTER TABLE sessions ADD COLUMN compute_lease_id TEXT;
ALTER TABLE sessions ADD COLUMN compute_provider TEXT;
ALTER TABLE sessions ADD COLUMN estimated_compute_cost_usd REAL;
ALTER TABLE sessions ADD COLUMN fallback_model_target TEXT;

ALTER TABLE agent_threads ADD COLUMN model_target TEXT NOT NULL DEFAULT 'frontier_default';
ALTER TABLE agent_threads ADD COLUMN compute_lease_id TEXT;
ALTER TABLE agent_threads ADD COLUMN compute_provider TEXT;
ALTER TABLE agent_threads ADD COLUMN estimated_compute_cost_usd REAL;
ALTER TABLE agent_threads ADD COLUMN fallback_model_target TEXT;

ALTER TABLE agent_turns ADD COLUMN model_target TEXT NOT NULL DEFAULT 'frontier_default';
ALTER TABLE agent_turns ADD COLUMN compute_lease_id TEXT;
ALTER TABLE agent_turns ADD COLUMN compute_provider TEXT;
ALTER TABLE agent_turns ADD COLUMN estimated_compute_cost_usd REAL;
ALTER TABLE agent_turns ADD COLUMN fallback_model_target TEXT;

CREATE TABLE compute_leases (
    id                          TEXT PRIMARY KEY,
    quote_id                    TEXT,
    provider                    TEXT NOT NULL,
    provider_instance_id        TEXT,
    model_id                    TEXT NOT NULL,
    model_label                 TEXT NOT NULL,
    status                      TEXT NOT NULL,
    region                      TEXT,
    gpu_summary                 TEXT,
    price_per_hour_usd          REAL NOT NULL,
    max_compute_usd             REAL NOT NULL,
    estimated_cost_usd          REAL,
    endpoint_base_url           TEXT,
    endpoint_token_configured   INTEGER NOT NULL DEFAULT 0,
    fallback_target_json        TEXT,
    status_message              TEXT,
    started_at                  TEXT,
    ready_at                    TEXT,
    expires_at                  TEXT,
    terminated_at               TEXT,
    lease_json                  TEXT NOT NULL,
    created_at                  TEXT NOT NULL,
    updated_at                  TEXT NOT NULL
);
CREATE INDEX idx_compute_leases_status ON compute_leases(status, updated_at);
CREATE INDEX idx_compute_leases_provider ON compute_leases(provider, provider_instance_id);
CREATE INDEX idx_compute_leases_model ON compute_leases(model_id, status);

CREATE TABLE compute_lease_events (
    id            TEXT PRIMARY KEY,
    lease_id      TEXT NOT NULL REFERENCES compute_leases(id) ON DELETE CASCADE,
    status        TEXT NOT NULL,
    message       TEXT,
    payload_json  TEXT NOT NULL DEFAULT '{}',
    ts            TEXT NOT NULL
);
CREATE INDEX idx_compute_lease_events_lease ON compute_lease_events(lease_id, ts);
