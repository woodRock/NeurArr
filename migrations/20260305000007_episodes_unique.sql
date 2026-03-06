-- Fix episodes table to have a unique constraint for show+season+episode
-- Since we can't easily add unique to existing table in SQLite without recreate, let's create a new migration
CREATE TABLE IF NOT EXISTS episodes_v2 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    show_id INTEGER NOT NULL,
    season INTEGER NOT NULL,
    episode INTEGER NOT NULL,
    title TEXT,
    status TEXT NOT NULL,
    air_date TEXT,
    UNIQUE(show_id, season, episode),
    FOREIGN KEY(show_id) REFERENCES tracked_shows(id) ON DELETE CASCADE
);

-- Copy data if any exists
INSERT OR IGNORE INTO episodes_v2 (id, show_id, season, episode, title, status, air_date)
SELECT id, show_id, season, episode, title, status, air_date FROM episodes;

DROP TABLE episodes;
ALTER TABLE episodes_v2 RENAME TO episodes;
