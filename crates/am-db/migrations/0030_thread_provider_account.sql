-- The provider account a Workbench session last ran on (NULL: unmanaged CLI).
ALTER TABLE agent_threads ADD COLUMN provider_account_id TEXT;
