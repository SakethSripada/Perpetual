-- Approval requests raised by an isolated device worker are persisted on the
-- coordinator until a user decision is delivered back to that leased worker.
CREATE TABLE collaboration_approvals (
    id                TEXT PRIMARY KEY,
    assignment_id     TEXT NOT NULL REFERENCES collaboration_assignments(id) ON DELETE CASCADE,
    local_approval_id TEXT NOT NULL,
    request_json      TEXT NOT NULL,
    decision          TEXT,
    created_at        TEXT NOT NULL,
    resolved_at       TEXT,
    delivered_at      TEXT,
    UNIQUE(assignment_id, local_approval_id)
);
CREATE INDEX idx_collaboration_approvals_pending
    ON collaboration_approvals(assignment_id, decision, delivered_at);
