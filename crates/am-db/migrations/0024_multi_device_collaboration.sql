-- Secure multi-device coordination. The coordinator owns these rows; paired
-- devices never share or mount the SQLite file.

CREATE TABLE collaboration_devices (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    hostname          TEXT NOT NULL,
    platform          TEXT NOT NULL,
    extension_version TEXT NOT NULL,
    capabilities_json TEXT NOT NULL DEFAULT '[]',
    last_seen_at      TEXT NOT NULL,
    paired_at         TEXT NOT NULL,
    revoked_at        TEXT
);
CREATE INDEX idx_collaboration_devices_seen
    ON collaboration_devices(revoked_at, last_seen_at DESC);

CREATE TABLE collaboration_assignments (
    id                 TEXT PRIMARY KEY,
    thread_id          TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    turn_id            TEXT NOT NULL REFERENCES agent_turns(id) ON DELETE CASCADE,
    device_id          TEXT NOT NULL REFERENCES collaboration_devices(id) ON DELETE RESTRICT,
    agent_kind         TEXT NOT NULL,
    permission         TEXT NOT NULL,
    execution_backend  TEXT NOT NULL,
    prompt             TEXT NOT NULL,
    status             TEXT NOT NULL,
    lease_token_hash   TEXT,
    lease_expires_at   TEXT,
    created_at         TEXT NOT NULL,
    started_at         TEXT,
    finished_at        TEXT,
    error              TEXT
);
CREATE INDEX idx_collaboration_assignments_device
    ON collaboration_assignments(device_id, status, created_at);
CREATE INDEX idx_collaboration_assignments_thread
    ON collaboration_assignments(thread_id, created_at);
CREATE UNIQUE INDEX idx_collaboration_assignments_active_thread
    ON collaboration_assignments(thread_id)
    WHERE status IN ('queued', 'running', 'review');

CREATE TABLE collaboration_change_sets (
    id                 TEXT PRIMARY KEY,
    assignment_id      TEXT NOT NULL REFERENCES collaboration_assignments(id) ON DELETE CASCADE,
    thread_id          TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    device_id          TEXT NOT NULL REFERENCES collaboration_devices(id) ON DELETE RESTRICT,
    repo_id            TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    base_ref           TEXT,
    files_json         TEXT NOT NULL DEFAULT '[]',
    patch              TEXT NOT NULL,
    patch_sha256       TEXT NOT NULL,
    status             TEXT NOT NULL DEFAULT 'pending',
    conflict_files_json TEXT NOT NULL DEFAULT '[]',
    created_at         TEXT NOT NULL,
    applied_at         TEXT
);
CREATE INDEX idx_collaboration_changes_thread
    ON collaboration_change_sets(thread_id, created_at DESC);
CREATE INDEX idx_collaboration_changes_assignment
    ON collaboration_change_sets(assignment_id, created_at);
