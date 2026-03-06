use anyhow::{Context, Result};
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};
use std::env;
use crate::parser::MediaMetadata;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, sqlx::FromRow, Clone, Debug)]
pub struct TrackedShow {
    pub id: i64,
    pub title: String,
    pub tmdb_id: i64,
    pub media_type: String,
    pub status: String,
    pub poster_path: Option<String>,
    pub release_date: Option<String>,
    pub year: Option<i32>,
    pub genres: Option<String>,
    pub rating: i64,
    pub last_updated: String,
    pub total_seasons: i64,
}

#[derive(Serialize, Deserialize, sqlx::FromRow, Clone, Debug)]
pub struct Episode {
    pub id: i64,
    pub show_id: i64,
    pub season: i64,
    pub episode: i64,
    pub title: Option<String>,
    pub air_date: Option<String>,
    pub status: String,
    pub resolution: Option<String>,
    pub last_searched_at: Option<String>,
    pub search_attempts: i64,
}

#[derive(Serialize, Deserialize, sqlx::FromRow, Clone, Debug)]
pub struct QualityProfile {
    pub id: i64,
    pub name: String,
    pub min_resolution: String,
    pub max_resolution: String,
    pub upgrade_until: String,
    pub must_contain: Option<String>,
    pub must_not_contain: Option<String>,
}

pub async fn init_db() -> Result<SqlitePool> {
    let database_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:neurarr.db".to_string());
    if !database_url.starts_with("sqlite::memory:") {
        let db_path = database_url.trim_start_matches("sqlite:");
        if !std::path::Path::new(db_path).exists() {
            std::fs::File::create(db_path).context("Failed to create database file")?;
        }
    }
    let pool = SqlitePoolOptions::new().max_connections(5).connect(&database_url).await.context("Failed to connect to SQLite")?;
    sqlx::migrate!("./migrations").run(&pool).await.context("Failed to run database migrations")?;
    Ok(pool)
}

pub async fn insert_media_item(pool: &SqlitePool, filename: &str, metadata: &MediaMetadata) -> Result<i64> {
    let result = sqlx::query("INSERT INTO media_items (original_filename, title, season, episode, resolution, source, status) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(filename).bind(&metadata.title).bind(metadata.season).bind(metadata.episode).bind(&metadata.resolution).bind(&metadata.source).bind("parsed").execute(pool).await?;
    Ok(result.last_insert_rowid())
}

pub async fn get_item_by_id(pool: &SqlitePool, id: i64) -> Result<Option<crate::web::MediaItem>> {
    Ok(sqlx::query_as::<_, crate::web::MediaItem>("SELECT * FROM media_items WHERE id = ?").bind(id).fetch_optional(pool).await?)
}

pub async fn get_items_by_title(pool: &SqlitePool, title: &str) -> Result<Vec<crate::web::MediaItem>> {
    Ok(sqlx::query_as::<_, crate::web::MediaItem>("SELECT * FROM media_items WHERE LOWER(title) = LOWER(?) AND status != 'completed'").bind(title).fetch_all(pool).await?)
}

pub async fn insert_tracked_show(pool: &SqlitePool, title: &str, tmdb_id: u32, media_type: &str, status: &str, poster_path: Option<String>, release_date: Option<String>, genres: Option<String>, total_seasons: u32) -> Result<i64> {
    let year = release_date.as_deref().and_then(|d| d.split('-').next()).and_then(|y| y.parse::<i32>().ok());
    sqlx::query("INSERT INTO tracked_shows (title, tmdb_id, media_type, status, poster_path, release_date, year, genres, total_seasons) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(tmdb_id) DO UPDATE SET status = EXCLUDED.status, total_seasons = EXCLUDED.total_seasons")
        .bind(title).bind(tmdb_id as i64).bind(media_type).bind(status).bind(poster_path).bind(release_date).bind(year).bind(genres).bind(total_seasons as i64).execute(pool).await?;
    let row: (i64,) = sqlx::query_as("SELECT id FROM tracked_shows WHERE tmdb_id = ?").bind(tmdb_id as i64).fetch_one(pool).await?;
    Ok(row.0)
}

pub async fn get_show_by_id(pool: &SqlitePool, id: i64) -> Result<Option<TrackedShow>> {
    Ok(sqlx::query_as::<_, TrackedShow>("SELECT * FROM tracked_shows WHERE id = ?").bind(id).fetch_optional(pool).await?)
}

pub async fn get_tracked_shows(pool: &SqlitePool) -> Result<Vec<TrackedShow>> {
    Ok(sqlx::query_as::<_, TrackedShow>("SELECT * FROM tracked_shows ORDER BY title ASC").fetch_all(pool).await?)
}

pub async fn update_tracked_show_status(pool: &SqlitePool, id: i64, status: &str) -> Result<()> {
    sqlx::query("UPDATE tracked_shows SET status = ?, last_updated = CURRENT_TIMESTAMP WHERE id = ?").bind(status).bind(id).execute(pool).await?;
    Ok(())
}

pub async fn update_tracked_show_info(pool: &SqlitePool, id: i64, status: Option<&str>, resolution: Option<&str>, rating: Option<i64>) -> Result<()> {
    if let Some(s) = status { sqlx::query("UPDATE tracked_shows SET status = ? WHERE id = ?").bind(s).bind(id).execute(pool).await?; }
    if let Some(r) = resolution { sqlx::query("UPDATE tracked_shows SET resolution = ? WHERE id = ?").bind(r).bind(id).execute(pool).await?; }
    if let Some(rt) = rating { sqlx::query("UPDATE tracked_shows SET rating = ? WHERE id = ?").bind(rt).bind(id).execute(pool).await?; }
    sqlx::query("UPDATE tracked_shows SET last_updated = CURRENT_TIMESTAMP WHERE id = ?").bind(id).execute(pool).await?;
    Ok(())
}

pub async fn delete_tracked_show(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM tracked_shows WHERE id = ?").bind(id).execute(pool).await?;
    Ok(())
}

pub async fn insert_recommendation_vote(pool: &SqlitePool, tmdb_id: u32, media_type: &str, vote: i32) -> Result<()> {
    sqlx::query("INSERT INTO recommendation_feedback (tmdb_id, media_type, vote) VALUES (?, ?, ?) ON CONFLICT(tmdb_id, media_type) DO UPDATE SET vote = EXCLUDED.vote").bind(tmdb_id as i64).bind(media_type).bind(vote).execute(pool).await?;
    Ok(())
}

pub async fn get_disapproved_ids(pool: &SqlitePool) -> Result<std::collections::HashSet<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as("SELECT tmdb_id FROM recommendation_feedback WHERE vote = -1").fetch_all(pool).await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

pub async fn get_approved_ids(pool: &SqlitePool) -> Result<Vec<(i64, String)>> {
    Ok(sqlx::query_as("SELECT tmdb_id, media_type FROM recommendation_feedback WHERE vote = 1").fetch_all(pool).await?)
}

pub async fn insert_episode(pool: &SqlitePool, show_id: i64, season: i32, episode: i32, title: Option<String>, air_date: Option<String>, status: &str) -> Result<()> {
    sqlx::query("INSERT INTO episodes (show_id, season, episode, title, air_date, status) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(show_id, season, episode) DO UPDATE SET title = COALESCE(EXCLUDED.title, title), air_date = COALESCE(EXCLUDED.air_date, air_date)")
        .bind(show_id).bind(season as i64).bind(episode as i64).bind(title).bind(air_date).bind(status).execute(pool).await?;
    Ok(())
}

pub async fn manual_match_item(pool: &SqlitePool, id: i64, tmdb_id: u32, title: &str, poster_path: Option<String>) -> Result<()> {
    sqlx::query("UPDATE media_items SET tmdb_id = ?, title = ?, poster_path = ?, status = 'matched' WHERE id = ?").bind(tmdb_id as i64).bind(title).bind(poster_path).bind(id).execute(pool).await?;
    Ok(())
}

pub async fn get_episodes_for_show(pool: &SqlitePool, show_id: i64) -> Result<Vec<Episode>> {
    Ok(sqlx::query_as::<_, Episode>("SELECT * FROM episodes WHERE show_id = ? ORDER BY season ASC, episode ASC").bind(show_id).fetch_all(pool).await?)
}

pub async fn update_episode_status(pool: &SqlitePool, id: i64, status: &str) -> Result<()> {
    sqlx::query("UPDATE episodes SET status = ? WHERE id = ?").bind(status).bind(id).execute(pool).await?;
    Ok(())
}

pub async fn bulk_update_episodes_status(pool: &SqlitePool, show_id: i64, season: i64, status: &str) -> Result<()> {
    sqlx::query("UPDATE episodes SET status = ? WHERE show_id = ? AND season = ?").bind(status).bind(show_id).bind(season).execute(pool).await?;
    Ok(())
}

pub async fn get_wanted_episodes(pool: &SqlitePool) -> Result<Vec<(Episode, TrackedShow)>> {
    let rows = sqlx::query("SELECT e.*, s.id as sid, s.title as stitle, s.tmdb_id, s.media_type, s.status as sstatus, s.poster_path, s.release_date, s.year, s.genres, s.rating, s.last_updated, s.total_seasons FROM episodes e JOIN tracked_shows s ON e.show_id = s.id WHERE e.status = 'wanted' AND s.status != 'watchlist' ORDER BY s.last_updated DESC").fetch_all(pool).await?;
    let mut results = Vec::new();
    for r in rows {
        use sqlx::Row;
        let ep = Episode { id: r.get("id"), show_id: r.get("show_id"), season: r.get("season"), episode: r.get("episode"), title: r.get("title"), air_date: r.get("air_date"), status: r.get("status"), resolution: r.get("resolution"), last_searched_at: r.get("last_searched_at"), search_attempts: r.get("search_attempts") };
        let show = TrackedShow { id: r.get("sid"), title: r.get("stitle"), tmdb_id: r.get("tmdb_id"), media_type: r.get("media_type"), status: r.get("sstatus"), poster_path: r.get("poster_path"), release_date: r.get("release_date"), year: r.get("year"), genres: r.get("genres"), rating: r.get("rating"), last_updated: r.get("last_updated"), total_seasons: r.get("total_seasons") };
        results.push((ep, show));
    }
    Ok(results)
}

pub async fn get_needed_seasons(pool: &SqlitePool) -> Result<Vec<(i64, TrackedShow)>> {
    let rows = sqlx::query("SELECT e.season, s.id as sid, s.title as stitle, s.tmdb_id, s.media_type, s.status as sstatus, s.poster_path, s.release_date, s.year, s.genres, s.rating, s.last_updated, s.total_seasons FROM episodes e JOIN tracked_shows s ON e.show_id = s.id WHERE e.status = 'wanted' AND s.media_type = 'tv' GROUP BY s.id, e.season HAVING COUNT(CASE WHEN e.status = 'wanted' THEN 1 END) > 5").fetch_all(pool).await?;
    let mut results = Vec::new();
    for r in rows {
        use sqlx::Row;
        let show = TrackedShow { id: r.get("sid"), title: r.get("stitle"), tmdb_id: r.get("tmdb_id"), media_type: r.get("media_type"), status: r.get("sstatus"), poster_path: r.get("poster_path"), release_date: r.get("release_date"), year: r.get("year"), genres: r.get("genres"), rating: r.get("rating"), last_updated: r.get("last_updated"), total_seasons: r.get("total_seasons") };
        results.push((r.get::<i64, _>(0), show));
    }
    Ok(results)
}

pub async fn insert_pending_download(pool: &SqlitePool, torrent_name: &str, show_id: Option<i64>, episode_id: Option<i64>, tmdb_id: u32, media_type: &str, season: Option<i64>) -> Result<()> {
    sqlx::query("INSERT INTO pending_downloads (torrent_name, show_id, episode_id, tmdb_id, media_type, season) VALUES (?, ?, ?, ?, ?, ?)").bind(torrent_name).bind(show_id).bind(episode_id).bind(tmdb_id as i64).bind(media_type).bind(season).execute(pool).await?;
    Ok(())
}

pub async fn get_user_hash(pool: &SqlitePool, username: &str) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT password_hash FROM users WHERE username = ?").bind(username).fetch_optional(pool).await?;
    Ok(row.map(|r| r.0))
}

pub async fn create_user(pool: &SqlitePool, username: &str, hash: &str) -> Result<()> {
    sqlx::query("INSERT INTO users (username, password_hash) VALUES (?, ?)").bind(username).bind(hash).execute(pool).await?;
    Ok(())
}

pub async fn get_default_quality_profile(pool: &SqlitePool) -> Result<QualityProfile> {
    Ok(sqlx::query_as::<_, QualityProfile>("SELECT * FROM quality_profiles LIMIT 1").fetch_one(pool).await?)
}

pub async fn get_all_quality_profiles(pool: &SqlitePool) -> Result<Vec<QualityProfile>> {
    Ok(sqlx::query_as::<_, QualityProfile>("SELECT * FROM quality_profiles").fetch_all(pool).await?)
}

pub async fn clear_media_queue(pool: &SqlitePool) -> Result<()> {
    sqlx::query("DELETE FROM media_items WHERE status = 'completed'").execute(pool).await?;
    Ok(())
}

pub async fn get_manual_match(pool: &SqlitePool, original_title: &str) -> Result<Option<i64>> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT tmdb_id FROM manual_matches WHERE LOWER(original_title) = LOWER(?)").bind(original_title).fetch_optional(pool).await?;
    Ok(row.map(|r| r.0))
}

pub async fn insert_manual_match(pool: &SqlitePool, original_title: &str, tmdb_id: u32, title: &str, poster_path: Option<String>) -> Result<()> {
    sqlx::query("INSERT INTO manual_matches (original_title, tmdb_id, title, poster_path) VALUES (?, ?, ?, ?) ON CONFLICT(original_title) DO UPDATE SET tmdb_id = EXCLUDED.tmdb_id, title = EXCLUDED.title, poster_path = EXCLUDED.poster_path").bind(original_title).bind(tmdb_id as i64).bind(title).bind(poster_path).execute(pool).await?;
    Ok(())
}

pub async fn update_episode_resolution(pool: &SqlitePool, id: i64, resolution: &str) -> Result<()> {
    sqlx::query("UPDATE episodes SET resolution = ? WHERE id = ?").bind(resolution).bind(id).execute(pool).await?;
    Ok(())
}

pub async fn update_season_status(pool: &SqlitePool, show_id: i64, season: i64, status: &str) -> Result<()> {
    sqlx::query("UPDATE episodes SET status = ? WHERE show_id = ? AND season = ?").bind(status).bind(show_id).bind(season).execute(pool).await?;
    Ok(())
}

pub async fn update_media_item_full(pool: &SqlitePool, id: i64, tmdb_id: u32, title: &str, summary: String, season: Option<i32>, episode: Option<i32>) -> Result<()> {
    sqlx::query("UPDATE media_items SET tmdb_id = ?, title = ?, spoiler_free_summary = ?, season = ?, episode = ?, status = 'processed' WHERE id = ?").bind(tmdb_id as i64).bind(title).bind(summary).bind(season.map(|s| s as i64)).bind(episode.map(|e| e as i64)).bind(id).execute(pool).await?;
    Ok(())
}
