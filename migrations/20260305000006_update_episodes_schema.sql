-- Update episodes table with air_date and better status tracking
ALTER TABLE episodes ADD COLUMN air_date TEXT;
