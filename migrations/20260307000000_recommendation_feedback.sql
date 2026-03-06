-- Migration: User Preference Feedback
CREATE TABLE recommendation_feedback (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    tmdb_id INTEGER NOT NULL,
    media_type TEXT NOT NULL, -- 'movie' or 'tv'
    vote INTEGER NOT NULL, -- 1 for Thumbs Up, -1 for Thumbs Down
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(tmdb_id, media_type)
);
