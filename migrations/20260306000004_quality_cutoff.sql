-- Add upgrade cutoff to quality profiles
ALTER TABLE quality_profiles ADD COLUMN upgrade_until TEXT DEFAULT '1080p';
