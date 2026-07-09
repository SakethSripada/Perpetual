-- Internal carryover turns should not duplicate the user's visible prompt in
-- the transcript. Publicly queued turns keep the old behavior by default.
ALTER TABLE queued_turns ADD COLUMN echo_user_message INTEGER NOT NULL DEFAULT 1;
