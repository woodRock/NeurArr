-- Settings and Quality Profiles
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS quality_profiles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    min_resolution TEXT DEFAULT '720p',
    max_resolution TEXT DEFAULT '2160p',
    must_contain TEXT,
    must_not_contain TEXT,
    is_default BOOLEAN DEFAULT 0,
    upgrade_until TEXT DEFAULT '1080p'
);

-- Insert default profile
INSERT OR IGNORE INTO quality_profiles (name, min_resolution, max_resolution, must_contain, must_not_contain, is_default, upgrade_until)
VALUES ('Standard HD', '720p', '1080p', '', 'cam,ts,telesync', 1, '1080p');
