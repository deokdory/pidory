-- member_roster: Discord guild member → username/nickname/aliases mapping
-- Rollback DDL (단방향 migrate 참고용):
--   DROP INDEX IF EXISTS idx_member_roster_guild;
--   DROP TABLE IF EXISTS member_roster;

CREATE TABLE IF NOT EXISTS member_roster (
    guild_id       BIGINT      NOT NULL,
    user_id        BIGINT      NOT NULL,
    username       TEXT        NOT NULL,
    global_name    TEXT,
    guild_nickname TEXT,
    aliases        JSONB       NOT NULL DEFAULT '[]',
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (guild_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_member_roster_guild ON member_roster(guild_id);
