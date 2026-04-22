-- src/db/schema.sql
-- Chạy khi khởi động server (idempotent - dùng IF NOT EXISTS)

CREATE TABLE IF NOT EXISTS sessions (
    id            TEXT PRIMARY KEY,
    thread_id     TEXT,
    mode          TEXT NOT NULL,
    surface       TEXT,
    account_id    TEXT,
    created_at    INTEGER NOT NULL,  -- unix timestamp
    updated_at    INTEGER NOT NULL,
    metadata_json TEXT
);

CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_sessions_account ON sessions(account_id);

CREATE TABLE IF NOT EXISTS accounts (
    id          TEXT PRIMARY KEY,
    label       TEXT,
    auth_path   TEXT NOT NULL UNIQUE,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_accounts_enabled ON accounts(enabled);
CREATE INDEX IF NOT EXISTS idx_accounts_auth_path ON accounts(auth_path);

CREATE TABLE IF NOT EXISTS account_pool_settings (
    singleton        INTEGER PRIMARY KEY CHECK (singleton = 1),
    error_threshold  INTEGER NOT NULL,
    cooldown_secs    INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS jobs (
    id          TEXT PRIMARY KEY,
    session_id  TEXT,
    status      TEXT NOT NULL DEFAULT 'pending',  -- pending|running|done|failed
    surface     TEXT,
    account_id  TEXT,
    created_at  INTEGER NOT NULL,
    finished_at INTEGER,
    error       TEXT,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_jobs_status   ON jobs(status);
CREATE INDEX IF NOT EXISTS idx_jobs_created  ON jobs(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_jobs_account  ON jobs(account_id);
