-- Add smarter discovery support
ALTER TABLE tracked_shows ADD COLUMN genres TEXT;
ALTER TABLE tracked_shows ADD COLUMN rating INTEGER DEFAULT 0; -- 0 to 5
ALTER TABLE tracked_shows ADD COLUMN last_updated DATETIME DEFAULT '2026-03-06 00:00:00';

-- Add a seen list table for quick lookup or just use status in tracked_shows
-- Using status in tracked_shows is simpler: 'wanted', 'downloading', 'watched'
