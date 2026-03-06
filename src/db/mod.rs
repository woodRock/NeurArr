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

pub async fn get_wanted_shows(pool: &SqlitePool) -> Result<Vec<TrackedShow>> {
    let items = sqlx::query_as::<_, TrackedShow>("SELECT * FROM tracked_shows WHERE status = 'wanted' ORDER BY added_at ASC")
        .fetch_all(pool)
        .await?;
    Ok(items)
}

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
