-- Add season to pending_downloads to support season packs
ALTER TABLE pending_downloads ADD COLUMN season INTEGER;

-- Add total_seasons to tracked_shows to know when a season is final
ALTER TABLE tracked_shows ADD COLUMN total_seasons INTEGER DEFAULT 1;
