-- Device workers must never edit the user's visible checkout. This flag is
-- immutable session metadata set only for mirrored collaboration threads.
ALTER TABLE agent_threads ADD COLUMN force_managed_workspace INTEGER NOT NULL DEFAULT 0;
