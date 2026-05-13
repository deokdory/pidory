-- projects: Discord channel → project path mapping
CREATE TABLE IF NOT EXISTS projects (
    channel_id    TEXT PRIMARY KEY,
    path          TEXT NOT NULL,
    name          TEXT,
    disallowed_tools TEXT,  -- JSON array; NULL means use global default
    created_at    TEXT NOT NULL DEFAULT TO_CHAR(NOW(), 'YYYY-MM-DD HH24:MI:SS')
);

-- sessions: Discord thread → Claude CLI session mapping
CREATE TABLE IF NOT EXISTS sessions (
    thread_id      TEXT PRIMARY KEY,
    channel_id     TEXT NOT NULL REFERENCES projects(channel_id),
    session_id     TEXT,    -- Claude CLI session UUID (set after first run)
    status         TEXT NOT NULL DEFAULT 'idle',  -- idle | running | error | completed
    created_at     TEXT NOT NULL DEFAULT TO_CHAR(NOW(), 'YYYY-MM-DD HH24:MI:SS'),
    last_active_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_sessions_channel ON sessions(channel_id);
CREATE INDEX IF NOT EXISTS idx_sessions_status  ON sessions(status);
