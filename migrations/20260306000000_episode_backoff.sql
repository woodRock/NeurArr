-- Track search attempts and back-off for episodes
ALTER TABLE episodes ADD COLUMN search_attempts INTEGER DEFAULT 0;
ALTER TABLE episodes ADD COLUMN last_searched_at DATETIME;
