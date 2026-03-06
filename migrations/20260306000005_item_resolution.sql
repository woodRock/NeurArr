-- Add current resolution tracking to episodes and tracked_shows for quality upgrades
ALTER TABLE episodes ADD COLUMN resolution TEXT;
ALTER TABLE tracked_shows ADD COLUMN resolution TEXT;
