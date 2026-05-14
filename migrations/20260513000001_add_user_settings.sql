CREATE TABLE IF NOT EXISTS user_settings (
    user_id BIGINT PRIMARY KEY,
    default_perm_scope TEXT NOT NULL DEFAULT 'project'
        CHECK (default_perm_scope IN ('project', 'global'))
);
