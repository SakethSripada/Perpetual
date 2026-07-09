-- Per-session model and reasoning-effort overrides. NULL means "let the agent
-- CLI use its own default".
ALTER TABLE agent_threads ADD COLUMN model TEXT;
ALTER TABLE agent_threads ADD COLUMN reasoning TEXT;
