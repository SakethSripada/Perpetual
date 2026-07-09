-- Plan-run failure handling and coordination hardening.

-- halt: any failed node fails the plan (previous behavior).
-- continue: failed subtrees are skipped; independent work keeps running.
-- retry: failed nodes are re-queued up to max_node_retries times.
ALTER TABLE work_plan_runs ADD COLUMN failure_mode TEXT NOT NULL DEFAULT 'halt';
ALTER TABLE work_plan_runs ADD COLUMN max_node_retries INTEGER NOT NULL DEFAULT 0;

-- When a prerequisite completes, optionally steer already-running dependent
-- sessions with its handoff summary (consumes an agent turn; opt-in).
ALTER TABLE work_plan_runs ADD COLUMN steer_dependents_on_unblock INTEGER NOT NULL DEFAULT 0;

-- Consecutive unknown-reset limit strikes per agent, for exponential probe
-- backoff instead of a fixed retry window.
ALTER TABLE agents ADD COLUMN limit_strikes INTEGER NOT NULL DEFAULT 0;
