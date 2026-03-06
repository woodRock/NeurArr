use anyhow::{Context, Result};
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};
use std::env;
use crate::parser::MediaMetadata;

pub async fn init_db() -> Result<SqlitePool> {
    let database_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:neurarr.db".to_string());
    
    // Create the database file if it doesn't exist
    if !database_url.starts_with("sqlite::memory:") {
        let db_path = database_url.trim_start_matches("sqlite:");
        if !std::path::Path::new(db_path).exists() {
            std::fs::File::create(db_path).context("Failed to create database file")?;
        }
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .context("Failed to connect to SQLite")?;

    // Run migrations
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("Failed to run database migrations")?;

    Ok(pool)
}

pub async fn insert_media_item(
    pool: &SqlitePool,
    filename: &str,
    metadata: &MediaMetadata,
) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO media_items (original_filename, title, season, episode, resolution, source, status)
         VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(filename)
    .bind(&metadata.title)
    .bind(metadata.season)
    .bind(metadata.episode)
    .bind(&metadata.resolution)
    .bind(&metadata.source)
    .bind("parsed")
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

#[allow(dead_code)]
pub async fn update_item_poster(
    pool: &SqlitePool,
    id: i64,
    poster_path: &str,
) -> Result<()> {
    sqlx::query("UPDATE media_items SET poster_path = ? WHERE id = ?")
        .bind(poster_path)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_summaries(
    pool: &SqlitePool,
    id: i64,
    tmdb_id: u32,
    original_summary: &str,
    spoiler_free_summary: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE media_items 
         SET tmdb_id = ?, original_summary = ?, spoiler_free_summary = ?, status = ?
         WHERE id = ?"
    )
    .bind(tmdb_id)
    .bind(original_summary)
    .bind(spoiler_free_summary)
    .bind("summarized")
    .bind(id)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn manual_match_item(
    pool: &SqlitePool,
    id: i64,
    tmdb_id: u32,
    title: &str,
    poster_path: Option<String>,
) -> Result<()> {
    sqlx::query(
        "UPDATE media_items 
         SET tmdb_id = ?, title = ?, poster_path = ?, status = 'matched'
         WHERE id = ?"
    )
    .bind(tmdb_id)
    .bind(title)
    .bind(poster_path)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_item_by_id(pool: &SqlitePool, id: i64) -> Result<Option<crate::web::MediaItem>> {
    let item = sqlx::query_as::<_, crate::web::MediaItem>("SELECT * FROM media_items WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(item)
}

pub async fn get_items_by_title(pool: &SqlitePool, title: &str) -> Result<Vec<crate::web::MediaItem>> {
    let items = sqlx::query_as::<_, crate::web::MediaItem>(
        "SELECT * FROM media_items 
         WHERE LOWER(title) = LOWER(?) 
         AND status != 'completed'"
    )
    .bind(title)
    .fetch_all(pool)
    .await?;
    Ok(items)
}

pub async fn insert_tracked_show(
    pool: &SqlitePool,
    title: &str,
    tmdb_id: u32,
    media_type: &str,
    poster_path: Option<String>,
    release_date: Option<String>,
) -> Result<()> {
    let year = release_date.as_deref()
        .and_then(|d| d.split('-').next())
        .and_then(|y| y.parse::<i32>().ok());

    sqlx::query(
        "INSERT INTO tracked_shows (title, tmdb_id, media_type, status, poster_path, release_date, year)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(tmdb_id) DO UPDATE SET status = 'wanted'"
    )
    .bind(title)
    .bind(tmdb_id)
    .bind(media_type)
    .bind("wanted")
    .bind(poster_path)
    .bind(release_date)
    .bind(year)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_tracked_shows(pool: &SqlitePool) -> Result<Vec<TrackedShow>> {
    let items = sqlx::query_as::<_, TrackedShow>("SELECT * FROM tracked_shows ORDER BY release_date ASC")
        .fetch_all(pool)
        .await?;
    Ok(items)
}

#[derive(sqlx::FromRow, serde::Serialize)]
pub struct TrackedShow {
    pub id: i64,
    pub title: String,
    pub tmdb_id: i64,
    pub media_type: String,
    pub status: String,
    pub poster_path: Option<String>,
    pub release_date: Option<String>,
    pub added_at: String,
    pub year: Option<i64>,
}

pub async fn delete_tracked_show(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM tracked_shows WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[allow(dead_code)]
pub async fn get_wanted_shows(pool: &SqlitePool) -> Result<Vec<TrackedShow>> {
    let items = sqlx::query_as::<_, TrackedShow>("SELECT * FROM tracked_shows WHERE status = 'wanted' ORDER BY added_at ASC")
        .fetch_all(pool)
        .await?;
    Ok(items)
}

pub async fn get_episodes_for_show(pool: &SqlitePool, show_id: i64) -> Result<Vec<Episode>> {
    let items = sqlx::query_as::<_, Episode>("SELECT * FROM episodes WHERE show_id = ? ORDER BY season ASC, episode ASC")
        .bind(show_id)
        .fetch_all(pool)
        .await?;
    Ok(items)
}

pub async fn insert_episode(
    pool: &SqlitePool,
    show_id: i64,
    season: u32,
    episode: u32,
    title: &str,
    air_date: Option<String>,
    status: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO episodes (show_id, season, episode, title, air_date, status)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(show_id, season, episode) DO UPDATE SET title = EXCLUDED.title, air_date = EXCLUDED.air_date"
    )
    .bind(show_id)
    .bind(season)
    .bind(episode)
    .bind(title)
    .bind(air_date)
    .bind(status)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_episode_status(pool: &SqlitePool, id: i64, status: &str) -> Result<()> {
    sqlx::query("UPDATE episodes SET status = ? WHERE id = ?")
        .bind(status)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_wanted_episodes(pool: &SqlitePool) -> Result<Vec<(Episode, TrackedShow)>> {
    // Join with tracked_shows to get the title for searching
    // Implement back-off: only search if attempts < 3 OR last_searched_at < 30 mins ago
    let rows = sqlx::query(
        "SELECT e.*, s.title as show_title, s.media_type, s.tmdb_id as show_tmdb_id, s.year as show_year 
         FROM episodes e 
         JOIN tracked_shows s ON e.show_id = s.id 
         WHERE e.status = 'wanted' 
         AND (
            e.search_attempts < 3 
            OR e.last_searched_at IS NULL 
            OR e.last_searched_at < datetime('now', '-30 minutes')
         )"
    )
    .fetch_all(pool)
    .await?;

    let mut results = Vec::new();
    for r in rows {
        use sqlx::Row;
        let ep = Episode {
            id: r.get("id"),
            show_id: r.get("show_id"),
            season: r.get("season"),
            episode: r.get("episode"),
            title: r.get("title"),
            status: r.get("status"),
            air_date: r.get("air_date"),
            search_attempts: r.get("search_attempts"),
            last_searched_at: r.get("last_searched_at"),
        };
        let show = TrackedShow {
            id: r.get("show_id"),
            title: r.get("show_title"),
            tmdb_id: r.get("show_tmdb_id"),
            media_type: r.get("media_type"),
            status: "".to_string(), // not needed for search
            poster_path: None,
            release_date: None,
            added_at: "".to_string(),
            year: r.get("show_year"),
        };
        results.push((ep, show));
    }
    Ok(results)
}

#[derive(sqlx::FromRow, serde::Serialize, Clone)]
pub struct Episode {
    pub id: i64,
    pub show_id: i64,
    pub season: i64,
    pub episode: i64,
    pub title: Option<String>,
    pub status: String,
    pub air_date: Option<String>,
    pub search_attempts: i64,
    pub last_searched_at: Option<String>,
}

pub async fn increment_episode_attempts(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query("UPDATE episodes SET search_attempts = search_attempts + 1, last_searched_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn reset_episode_attempts(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query("UPDATE episodes SET search_attempts = 0 WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[allow(dead_code)]
pub async fn update_tracked_show_status(pool: &SqlitePool, id: i64, status: &str) -> Result<()> {
    sqlx::query("UPDATE tracked_shows SET status = ? WHERE id = ?")
        .bind(status)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn clear_media_queue(pool: &SqlitePool) -> Result<()> {
    sqlx::query("DELETE FROM media_items").execute(pool).await?;
    Ok(())
}

pub async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>> {
    let row = sqlx::query("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    use sqlx::Row;
    Ok(row.map(|r| r.get(0)))
}

#[allow(dead_code)]
pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = EXCLUDED.value")
        .bind(key)
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_user_hash(pool: &SqlitePool, username: &str) -> Result<Option<String>> {
    let row = sqlx::query("SELECT password_hash FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(pool)
        .await?;
    use sqlx::Row;
    Ok(row.map(|r| r.get(0)))
}

pub async fn create_user(pool: &SqlitePool, username: &str, password_hash: &str) -> Result<()> {
    sqlx::query("INSERT INTO users (username, password_hash) VALUES (?, ?)")
        .bind(username)
        .bind(password_hash)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(sqlx::FromRow, serde::Serialize, serde::Deserialize, Clone)]
pub struct QualityProfile {
    pub id: i64,
    pub name: String,
    pub min_resolution: String,
    pub max_resolution: String,
    pub must_contain: Option<String>,
    pub must_not_contain: Option<String>,
}

pub async fn get_default_quality_profile(pool: &SqlitePool) -> Result<QualityProfile> {
    let profile = sqlx::query_as::<_, QualityProfile>("SELECT id, name, min_resolution, max_resolution, must_contain, must_not_contain FROM quality_profiles WHERE is_default = 1 LIMIT 1")
        .fetch_one(pool)
        .await?;
    Ok(profile)
}

pub async fn get_all_quality_profiles(pool: &SqlitePool) -> Result<Vec<QualityProfile>> {
    let profiles = sqlx::query_as::<_, QualityProfile>("SELECT id, name, min_resolution, max_resolution, must_contain, must_not_contain FROM quality_profiles")
        .fetch_all(pool)
        .await?;
    Ok(profiles)
}

pub async fn insert_manual_match(
    pool: &SqlitePool,
    parsed_title: &str,
    tmdb_id: u32,
    tmdb_title: &str,
    poster_path: Option<String>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO manual_matches (parsed_title, tmdb_id, tmdb_title, poster_path)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(parsed_title) DO UPDATE SET tmdb_id = EXCLUDED.tmdb_id, tmdb_title = EXCLUDED.tmdb_title, poster_path = EXCLUDED.poster_path"
    )
    .bind(parsed_title.to_lowercase())
    .bind(tmdb_id)
    .bind(tmdb_title)
    .bind(poster_path)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_manual_match(pool: &SqlitePool, parsed_title: &str) -> Result<Option<(u32, String, Option<String>)>> {
    let row = sqlx::query(
        "SELECT tmdb_id, tmdb_title, poster_path FROM manual_matches WHERE parsed_title = ?"
    )
    .bind(parsed_title.to_lowercase())
    .fetch_optional(pool)
    .await?;

    if let Some(r) = row {
        use sqlx::Row;
        let id: i64 = r.get(0);
        let title: String = r.get(1);
        let poster: Option<String> = r.get(2);
        Ok(Some((id as u32, title, poster)))
    } else {
        Ok(None)
    }
}
