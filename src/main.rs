mod db;
mod integrations;
mod llm;
mod parser;
mod scanner;
mod web;
mod utils;

use crate::scanner::Scanner;
use crate::db::init_db;
use crate::integrations::tmdb::TmdbClient;
use crate::integrations::torrent::QBittorrentClient;
use crate::llm::OllamaClient;
use crate::parser::Parser;
use crate::utils::Renamer;

use anyhow::Result;
use clap::{Parser as ClapParser, Subcommand};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore, broadcast};
use tracing::info;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use walkdir::WalkDir;

#[derive(ClapParser)]
#[command(name = "neurarr")]
#[command(about = "Privacy-first AI media management daemon", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Setup,
    Run,
    Update,
    Scan,
}

use tracing_subscriber::Layer;

struct BroadcastLayer {
    tx: broadcast::Sender<String>,
}

impl<S> Layer<S> for BroadcastLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut visitor = LogVisitor(String::new());
        event.record(&mut visitor);
        let _ = self.tx.send(visitor.0);
    }
}

struct LogVisitor(String);
impl tracing::field::Visit for LogVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{:?}", value);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let (log_tx, _) = broadcast::channel(100);
    let log_tx_clone = log_tx.clone();

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .with(BroadcastLayer { tx: log_tx_clone })
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Setup) => {
            info!("NeurArr Setup Mode");
            let pool = init_db().await?;
            if db::get_user_hash(&pool, "admin").await?.is_none() {
                info!("Creating default admin user (password: admin)");
                let hash = crate::utils::auth::hash_password("admin");
                db::create_user(&pool, "admin", &hash).await?;
            }
            return Ok(());
        }
        Some(Commands::Update) => {
            update_app().await?;
            return Ok(());
        }
        Some(Commands::Scan) => {
            let pool = init_db().await?;
            scan_library(pool).await?;
            return Ok(());
        }
        _ => {
            run_daemon(log_tx).await?;
        }
    }

    Ok(())
}

pub async fn scan_library(pool: sqlx::SqlitePool) -> Result<()> {
    let library_dir = env::var("NEURARR_LIBRARY_DIR").unwrap_or_else(|_| "./library".to_string());
    info!("Starting full library scan in: {}", library_dir);
    
    let tmdb = TmdbClient::new()?;
    let tracked = db::get_tracked_shows(&pool).await?;

    for entry in WalkDir::new(&library_dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let filename = entry.file_name().to_string_lossy().to_string();
            let ext = entry.path().extension().and_then(|e| e.to_str()).unwrap_or("");
            if ["mkv", "mp4", "avi", "mov"].contains(&ext) {
                let metadata = Parser::parse_regex(&filename);
                let normalized_filename_title = metadata.title.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "");
                
                for show in &tracked {
                    let normalized_show_title = show.title.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "");
                    let mut is_match = normalized_show_title == normalized_filename_title;
                    
                    if !is_match {
                        if let Ok(alts) = tmdb.get_alternative_titles(show.tmdb_id as u32, show.media_type == "tv").await {
                            if alts.iter().any(|a| a.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "") == normalized_filename_title) {
                                is_match = true;
                            }
                        }
                    }

                    if is_match {
                        if let (Some(s), Some(e)) = (metadata.season, metadata.episode) {
                            let _ = sqlx::query("UPDATE episodes SET status = 'completed' WHERE show_id = ? AND season = ? AND episode = ?")
                                .bind(show.id).bind(s as i64).bind(e as i64).execute(&pool).await;
                        } else if show.media_type == "movie" {
                            let _ = db::update_tracked_show_status(&pool, show.id, "completed").await;
                        }
                    }
                }
                let _ = db::insert_media_item(&pool, &filename, &metadata).await;
            }
        }
    }
    Ok(())
}

pub async fn sync_show_episodes(pool: &sqlx::SqlitePool, tmdb: &TmdbClient, show_id: i64) -> Result<()> {
    if let Ok(Some(show)) = db::get_show_by_id(pool, show_id).await {
        if show.media_type == "tv" {
            if let Ok(full) = tmdb.get_tv_details(show.tmdb_id as u32).await {
                let seasons = full.number_of_seasons.unwrap_or(1);
                for s in 1..=seasons {
                    if let Ok(eps) = tmdb.get_tv_season(show.tmdb_id as u32, s).await {
                        for ep in eps {
                            let aired = if let Some(d) = &ep.air_date {
                                if d.is_empty() { false }
                                else { d <= &chrono::Utc::now().date_naive().to_string() }
                            } else { false };
                            if aired {
                                let _ = db::insert_episode(pool, show.id, ep.season_number as i32, ep.episode_number as i32, Some(ep.name.clone()), ep.air_date, "wanted").await;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub async fn run_automation_cycle(pool: sqlx::SqlitePool, tmdb: TmdbClient, ollama: Arc<OllamaClient>, qbit: Arc<QBittorrentClient>) -> Result<()> {
    let indexer = crate::integrations::indexer::IndexerClient::new().unwrap();
    let profile = db::get_default_quality_profile(&pool).await.ok();

    if let Ok(tracked) = db::get_tracked_shows(&pool).await {
        for show in tracked { let _ = sync_show_episodes(&pool, &tmdb, show.id).await; }
    }

    if let Ok(needed_seasons) = db::get_needed_seasons(&pool).await {
        for (season_num, show) in needed_seasons {
            let s_code = format!("S{:02}", season_num);
            let mut queries = Vec::new();
            let year_suffix = show.year.map(|y| format!(" {}", y)).unwrap_or_default();
            queries.push(format!("{} {}{}", show.title, s_code, year_suffix));
            if let Ok(alts) = tmdb.get_alternative_titles(show.tmdb_id as u32, true).await {
                for alt in alts { queries.push(format!("{} {}{}", alt, s_code, year_suffix)); }
            }
            let mut found = false;
            for q in queries {
                if found { break; }
                if let Ok(res) = indexer.search(&q).await {
                    let filtered: Vec<_> = res.into_iter().filter(|r| {
                        let t = r.title.to_lowercase();
                        if let Some(y) = show.year {
                            let year_str = y.to_string();
                            let re = regex::Regex::new(r"\b(19|20)\d{2}\b").unwrap();
                            for caps in re.find_iter(&r.title) {
                                if caps.as_str() != year_str {
                                    if t.contains(&format!("({})", caps.as_str())) || t.contains(&format!("[{}]", caps.as_str())) || t.ends_with(caps.as_str()) { return false; }
                                }
                            }
                        }
                        t.contains(&s_code.to_lowercase()) && (t.contains("complete") || t.contains("season") || !t.contains("e0"))
                    }).collect();
                    for best in filtered {
                        if let Ok(true) = ollama.verify_torrent_match(&show.title, &best.title).await {
                            let ingest = std::fs::canonicalize("./ingest").unwrap_or_else(|_| PathBuf::from("./ingest"));
                            if qbit.add_torrent_url(&best.link, Some(&ingest.to_string_lossy())).await.is_ok() {
                                let _ = db::update_season_status(&pool, show.id, season_num, "downloading").await;
                                let _ = db::insert_pending_download(&pool, &best.title, Some(show.id), None, show.tmdb_id as u32, "tv", Some(season_num)).await;
                                found = true; break;
                            }
                        }
                    }
                }
            }
        }
    }

    if let Ok(wanted_eps) = db::get_wanted_episodes(&pool).await {
        for (ep, show) in wanted_eps {
            let ep_code = format!("S{:02}E{:02}", ep.season, ep.episode);
            let mut queries = Vec::new();
            let year_suffix = show.year.map(|y| format!(" {}", y)).unwrap_or_default();
            queries.push(format!("{} {}{}", show.title, ep_code, year_suffix));
            if let Ok(alts) = tmdb.get_alternative_titles(show.tmdb_id as u32, true).await {
                for alt in alts { queries.push(format!("{} {}{}", alt, ep_code, year_suffix)); }
            }
            let mut found = false;
            for q in queries {
                if found { break; }
                if let Ok(res) = indexer.search(&q).await {
                    let filtered: Vec<_> = res.into_iter().filter(|r| {
                        if let Some(y) = show.year {
                            let year_str = y.to_string();
                            let re = regex::Regex::new(r"\b(19|20)\d{2}\b").unwrap();
                            for caps in re.find_iter(&r.title) {
                                if caps.as_str() != year_str {
                                    let t_l = r.title.to_lowercase();
                                    if t_l.contains(&format!("({})", caps.as_str())) || t_l.contains(&format!("[{}]", caps.as_str())) || t_l.ends_with(caps.as_str()) { return false; }
                                }
                            }
                        }
                        if let Some(p) = &profile {
                            let t = r.title.to_lowercase();
                            if let Some(must) = &p.must_contain { if !must.is_empty() && !t.contains(must) { return false; } }
                            if let Some(not) = &p.must_not_contain { for w in not.split(',') { if !w.trim().is_empty() && t.contains(w.trim().to_lowercase().as_str()) { return false; } } }
                            if p.max_resolution == "1080p" && t.contains("2160p") { return false; }
                        }
                        true
                    }).collect();
                    for best in filtered {
                        if let Ok(true) = ollama.verify_torrent_match(&show.title, &best.title).await {
                            let ingest = std::fs::canonicalize("./ingest").unwrap_or_else(|_| PathBuf::from("./ingest"));
                            if qbit.add_torrent_url(&best.link, Some(&ingest.to_string_lossy())).await.is_ok() {
                                let _ = db::update_episode_status(&pool, ep.id, "downloading").await;
                                let _ = db::insert_pending_download(&pool, &best.title, Some(show.id), Some(ep.id), show.tmdb_id as u32, "tv", None).await;
                                found = true; break;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub async fn scan_ingest_folder(pool: sqlx::SqlitePool, tmdb: TmdbClient, ollama: Arc<OllamaClient>, _qbit: Arc<QBittorrentClient>) -> Result<()> {
    let ingest_dir = env::var("NEURARR_INGEST_DIR").unwrap_or_else(|_| "./ingest".to_string());
    let mut scanner = Scanner::new()?;
    scanner.scan(pool, tmdb, ollama, _qbit, PathBuf::from(ingest_dir)).await
}

async fn run_daemon(log_tx: broadcast::Sender<String>) -> Result<()> {
    let pool = init_db().await?;
    let tmdb_client = TmdbClient::new()?;
    let ollama = Arc::new(OllamaClient::new()?);
    let qbit = Arc::new(QBittorrentClient::new()?);
    let _ = qbit.login().await;

    let scanner_pool = pool.clone();
    let scanner_tmdb = tmdb_client.clone();
    let scanner_ollama = ollama.clone();
    let scanner_qbit = qbit.clone();
    let ingest_dir = env::var("NEURARR_INGEST_DIR").unwrap_or_else(|_| "./ingest".to_string());
    
    let ai_semaphore = Arc::new(Semaphore::new(1));
    let processing_registry = Arc::new(Mutex::new(std::collections::HashSet::new()));

    let mut scanner = Scanner::new()?;
    let _ = scanner.watch(PathBuf::from(&ingest_dir))?;

    tokio::spawn(async move {
        while let Some(res) = scanner.next_event().await {
            if let Ok(event) = res {
                for path in event.paths {
                    let path_clone = path.clone();
                    if path_clone.is_file() {
                        let mut registry = processing_registry.lock().await;
                        if registry.insert(path_clone.clone()) {
                            let pool = scanner_pool.clone();
                            let tmdb = scanner_tmdb.clone();
                            let ollama = scanner_ollama.clone();
                            let qbit_clone = scanner_qbit.clone();
                            let registry_inner = processing_registry.clone();
                            let sem = ai_semaphore.clone();
                            tokio::spawn(async move {
                                let _permit = sem.acquire().await.ok();
                                let _ = crate::process_file(path_clone.clone(), pool, tmdb, ollama, qbit_clone).await;
                                registry_inner.lock().await.remove(&path_clone);
                            });
                        }
                    }
                }
            }
        }
    });

    let _ = scan_ingest_folder(pool.clone(), tmdb_client.clone(), ollama.clone(), qbit.clone()).await;

    let scheduler_pool = pool.clone();
    let scheduler_tmdb = tmdb_client.clone();
    let scheduler_ollama = ollama.clone();
    let scheduler_qbit = qbit.clone();
    
    tokio::spawn(async move {
        loop {
            let _ = run_automation_cycle(scheduler_pool.clone(), scheduler_tmdb.clone(), scheduler_ollama.clone(), scheduler_qbit.clone()).await;
            let _ = scan_ingest_folder(scheduler_pool.clone(), scheduler_tmdb.clone(), scheduler_ollama.clone(), scheduler_qbit.clone()).await;
            tokio::time::sleep(std::time::Duration::from_secs(1800)).await;
        }
    });

    start_web_server(pool, log_tx).await?;
    Ok(())
}

async fn process_file(path: PathBuf, pool: sqlx::SqlitePool, tmdb: TmdbClient, ollama: Arc<OllamaClient>, _qbit: Arc<QBittorrentClient>) -> Result<()> {
    if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
        if !["mkv", "mp4", "avi", "mov"].contains(&path.extension().and_then(|e| e.to_str()).unwrap_or("")) { return Ok(()); }
        let metadata = Parser::parse_regex(filename);
        let item_id = db::insert_media_item(&pool, filename, &metadata).await?;
        run_pipeline(item_id, path, pool, tmdb, ollama, None).await?;
    }
    Ok(())
}

pub async fn run_pipeline(item_id: i64, path: PathBuf, pool: sqlx::SqlitePool, tmdb: TmdbClient, ollama: Arc<OllamaClient>, force_tmdb_id: Option<u32>) -> Result<()> {
    let filename = path.file_name().unwrap().to_str().unwrap();
    let metadata = Parser::parse_regex(filename);
    let tmdb_id = if let Some(id) = force_tmdb_id { id } else {
        if let Ok(Some(id)) = db::get_manual_match(&pool, &metadata.title).await { id as u32 }
        else {
            let results = if metadata.season.is_some() { tmdb.search_tv(&metadata.title, None).await? } else { tmdb.search_movie(&metadata.title, None).await? };
            if let Some(best) = results.first() {
                if let Ok(true) = ollama.verify_torrent_match(&metadata.title, &best.title.clone().or(best.name.clone()).unwrap()).await { best.id }
                else { return Ok(()); }
            } else { return Ok(()); }
        }
    };
    let details = if metadata.season.is_some() { tmdb.get_tv_details(tmdb_id).await? } else { tmdb.get_movie_details(tmdb_id).await? };
    let final_title = details.name.or(details.title).unwrap_or_else(|| "Unknown".to_string());
    let summary = ollama.rewrite_summary(&details.overview.unwrap_or_default()).await?;
    db::update_media_item_full(&pool, item_id, tmdb_id, &final_title, summary, metadata.season.map(|s| s as i32), metadata.episode.map(|e| e as i32)).await?;
    let renamer = Renamer::new(env::var("NEURARR_LIBRARY_DIR")?);
    renamer.move_file(&path, &metadata, &final_title).await?;
    Ok(())
}

async fn update_app() -> Result<()> {
    info!("Starting NeurArr update process...");
    let status = std::process::Command::new("git").arg("pull").status()?;
    if status.success() {
        info!("Successfully pulled latest changes. Restarting...");
        std::process::Command::new("cargo").arg("build").arg("--release").status()?;
        std::process::exit(0);
    }
    Ok(())
}

async fn start_web_server(pool: sqlx::SqlitePool, log_tx: broadcast::Sender<String>) -> Result<()> {
    crate::web::start_web_server(pool, log_tx).await
}
