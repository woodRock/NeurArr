-- Store manual title-to-tmdb mappings for future automatic matching
CREATE TABLE IF NOT EXISTS manual_matches (
    parsed_title TEXT PRIMARY KEY,
    tmdb_id INTEGER NOT NULL,
    tmdb_title TEXT NOT NULL,
    poster_path TEXT
);
