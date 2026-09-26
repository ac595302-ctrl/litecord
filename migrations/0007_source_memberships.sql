-- A single guild/channel row can be visible through multiple Discord
-- integrations. Keep presence per provenance source so one source's partial
-- snapshot cannot retire rows retained by another source.
CREATE TABLE guild_source_memberships (
    source   TEXT    NOT NULL,
    guild_id INTEGER NOT NULL REFERENCES guilds (id) ON DELETE CASCADE,
    PRIMARY KEY (source, guild_id)
);
CREATE INDEX guild_source_memberships_guild
    ON guild_source_memberships (guild_id);

CREATE TABLE channel_source_memberships (
    source     TEXT    NOT NULL,
    guild_id   INTEGER NOT NULL,
    channel_id INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    PRIMARY KEY (source, channel_id)
);
CREATE INDEX channel_source_memberships_guild
    ON channel_source_memberships (source, guild_id);
CREATE INDEX channel_source_memberships_channel
    ON channel_source_memberships (channel_id);

-- Existing active canonical rows came from their stored origin. Channels can
-- prove that a source retained the containing guild even if another source
-- most recently updated that guild's display fields.
INSERT OR IGNORE INTO guild_source_memberships (source, guild_id)
SELECT origin, id FROM guilds WHERE departed = 0;
INSERT OR IGNORE INTO guild_source_memberships (source, guild_id)
SELECT c.origin, c.guild_id
FROM channels c
JOIN guilds g ON g.id = c.guild_id
WHERE c.removed = 0;
INSERT OR IGNORE INTO channel_source_memberships (source, guild_id, channel_id)
SELECT origin, guild_id, id FROM channels WHERE removed = 0;
