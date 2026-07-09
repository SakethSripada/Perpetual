-- Stable client-side message ids let the webview reconcile optimistic user
-- bubbles with persisted user events and queued turns without text matching.
ALTER TABLE queued_turns ADD COLUMN client_message_id TEXT;
