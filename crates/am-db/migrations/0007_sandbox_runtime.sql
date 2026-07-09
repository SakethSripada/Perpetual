-- Execution backend metadata. Existing sessions/workspaces continue to run on
-- the host; Docker sandboxing is opt-in for new runs.

ALTER TABLE sessions ADD COLUMN execution_backend TEXT NOT NULL DEFAULT 'host';
ALTER TABLE sessions ADD COLUMN sandbox_name TEXT;

ALTER TABLE agent_threads ADD COLUMN execution_backend TEXT NOT NULL DEFAULT 'host';

ALTER TABLE agent_turns ADD COLUMN execution_backend TEXT NOT NULL DEFAULT 'host';
ALTER TABLE agent_turns ADD COLUMN sandbox_name TEXT;

ALTER TABLE task_repos ADD COLUMN workspace_backend TEXT NOT NULL DEFAULT 'host';
ALTER TABLE agent_thread_repos ADD COLUMN workspace_backend TEXT NOT NULL DEFAULT 'host';
