-- Preserve provider-specific effort choices while a thread is temporarily
-- running on another agent, so switch-back restores the exact original run.
ALTER TABLE agent_threads ADD COLUMN original_reasoning TEXT;
ALTER TABLE agent_threads ADD COLUMN fallback_reasoning TEXT;
