CREATE TABLE provider_account_states (
    account_id    TEXT PRIMARY KEY,
    availability  TEXT NOT NULL DEFAULT 'unknown',
    reset_at      TEXT,
    limit_strikes INTEGER NOT NULL DEFAULT 0,
    last_checked  TEXT
);
