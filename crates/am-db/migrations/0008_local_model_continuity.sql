-- Local-model fallback and target-safe continuation metadata.

ALTER TABLE sessions ADD COLUMN model TEXT;
ALTER TABLE sessions ADD COLUMN reasoning TEXT;
ALTER TABLE sessions ADD COLUMN local_provider TEXT;
ALTER TABLE sessions ADD COLUMN local_base_url TEXT;
ALTER TABLE sessions ADD COLUMN target_hash TEXT;

ALTER TABLE agent_turns ADD COLUMN model TEXT;
ALTER TABLE agent_turns ADD COLUMN reasoning TEXT;
ALTER TABLE agent_turns ADD COLUMN local_provider TEXT;
ALTER TABLE agent_turns ADD COLUMN local_base_url TEXT;
ALTER TABLE agent_turns ADD COLUMN target_hash TEXT;

ALTER TABLE agent_threads ADD COLUMN local_provider TEXT;
ALTER TABLE agent_threads ADD COLUMN local_base_url TEXT;
ALTER TABLE agent_threads ADD COLUMN original_model TEXT;
ALTER TABLE agent_threads ADD COLUMN fallback_model TEXT;
ALTER TABLE agent_threads ADD COLUMN original_local_provider TEXT;
ALTER TABLE agent_threads ADD COLUMN fallback_local_provider TEXT;
ALTER TABLE agent_threads ADD COLUMN original_local_base_url TEXT;
ALTER TABLE agent_threads ADD COLUMN fallback_local_base_url TEXT;
ALTER TABLE agent_threads ADD COLUMN switch_back_pending INTEGER NOT NULL DEFAULT 0;
