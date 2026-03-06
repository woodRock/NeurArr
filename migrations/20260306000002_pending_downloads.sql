-- Store mapping of torrent filenames to known show/episode metadata
-- This allows skipping AI ingestion when the file is finished
CREATE TABLE IF NOT EXISTS pending_downloads (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    torrent_name TEXT UNIQUE NOT NULL,
    show_id INTEGER,
    episode_id INTEGER,
    tmdb_id INTEGER NOT NULL,
    media_type TEXT NOT NULL,
    added_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
