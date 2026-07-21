-- Session task budgets are public configuration. Consumption, quota baselines,
-- and provider telemetry remain private enforcement state.
ALTER TABLE agent_threads ADD COLUMN task_budget TEXT NOT NULL DEFAULT '{"mode":"unlimited"}';
