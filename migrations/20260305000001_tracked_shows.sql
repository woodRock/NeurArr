-- Create the tracked_shows table for upcoming and requested content
CREATE TABLE IF NOT EXISTS tracked_shows (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    tmdb_id INTEGER UNIQUE,
    media_type TEXT NOT NULL, -- 'movie' or 'tv'
    status TEXT NOT NULL, -- 'wanted', 'monitoring', 'downloading', 'completed'
    poster_path TEXT,
    release_date TEXT,
    added_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
