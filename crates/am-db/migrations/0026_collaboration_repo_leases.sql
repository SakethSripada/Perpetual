-- A shared repository has one active writer at a time. Read-only assignments
-- do not acquire a lease. Review keeps the lease until every returned change
-- set is applied or rejected, preventing a later worker from starting against
-- state that the coordinator has not reconciled yet.
CREATE TABLE collaboration_repo_leases (
    repo_id       TEXT PRIMARY KEY REFERENCES repos(id) ON DELETE CASCADE,
    assignment_id TEXT NOT NULL REFERENCES collaboration_assignments(id) ON DELETE CASCADE,
    acquired_at   TEXT NOT NULL
);
CREATE INDEX idx_collaboration_repo_leases_assignment
    ON collaboration_repo_leases(assignment_id);
