-- M1: track the base commit a task's worktree was branched from, so diffs can be
-- computed against the original starting point regardless of agent commits.
ALTER TABLE task_repos ADD COLUMN base_ref TEXT;
