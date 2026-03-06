-- Create episodes table to track individual TV show episodes
CREATE TABLE IF NOT EXISTS episodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    show_id INTEGER NOT NULL,
    season INTEGER NOT NULL,
    episode INTEGER NOT NULL,
    title TEXT,
    status TEXT NOT NULL, -- 'wanted', 'downloading', 'completed'
    FOREIGN KEY(show_id) REFERENCES tracked_shows(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_episodes_show ON episodes(show_id);
