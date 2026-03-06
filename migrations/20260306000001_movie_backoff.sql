-- Track search attempts and back-off for movies (tracked_shows)
ALTER TABLE tracked_shows ADD COLUMN search_attempts INTEGER DEFAULT 0;
ALTER TABLE tracked_shows ADD COLUMN last_searched_at DATETIME;
