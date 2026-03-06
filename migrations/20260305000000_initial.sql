-- Create the media_items table
CREATE TABLE IF NOT EXISTS media_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    original_filename TEXT NOT NULL,
    title TEXT NOT NULL,
    season INTEGER,
    episode INTEGER,
    resolution TEXT,
    source TEXT,
    tmdb_id INTEGER,
    original_summary TEXT,
    spoiler_free_summary TEXT,
    processed_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    status TEXT NOT NULL -- 'pending', 'parsed', 'summarized', 'completed', 'failed'
);

-- Create index for quick lookup
CREATE INDEX IF NOT EXISTS idx_media_status ON media_items(status);
