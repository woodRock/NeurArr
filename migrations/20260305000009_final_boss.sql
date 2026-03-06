-- Users for Authentication
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT UNIQUE NOT NULL,
    password_hash TEXT NOT NULL
);

-- Store subtitle status
CREATE TABLE IF NOT EXISTS subtitles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    media_item_id INTEGER NOT NULL,
    language TEXT NOT NULL,
    path TEXT,
    FOREIGN KEY(media_item_id) REFERENCES media_items(id) ON DELETE CASCADE
);

-- Add quality to media_items for upgrading logic
ALTER TABLE media_items ADD COLUMN quality TEXT;

-- Default settings for renaming
INSERT OR IGNORE INTO settings (key, value) VALUES ('rename_format_movie', '{title} ({year})/{title} ({year}) [{quality}]');
INSERT OR IGNORE INTO settings (key, value) VALUES ('rename_format_tv', '{title}/Season {season}/{title} - S{season}E{episode} - {quality}');
