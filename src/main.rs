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
use tracing::{info, error};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use walkdir::WalkDir;

use tray_icon::{TrayIconBuilder, TrayIconEvent};
use muda::{MenuEvent, Menu, MenuItem, PredefinedMenuItem};
use tao::event_loop::{EventLoopBuilder, ControlFlow};
use image::Rgba;
use open;

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

fn load_icon() -> Option<tray_icon::Icon> {
    if let Ok(img) = image::open("assets/logo.png") {
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        let raw = rgba.into_raw();
        if let Ok(icon) = tray_icon::Icon::from_rgba(raw, width, height) {
            return Some(icon);
        }
    }

    let mut img = image::RgbaImage::new(32, 32);
    for x in 0..32 {
        for y in 0..32 {
            let dx = x as f32 - 16.0;
            let dy = y as f32 - 16.0;
            if dx*dx + dy*dy < 200.0 {
                img.put_pixel(x, y, Rgba([56, 189, 248, 255]));
            }
        }
    }
    let (width, height) = img.dimensions();
    let rgba = img.into_raw();
    tray_icon::Icon::from_rgba(rgba, width, height).ok()
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
            Ok(())
        }
        Some(Commands::Update) => update_app().await,
        Some(Commands::Scan) => {
            let pool = init_db().await?;
            scan_library(pool).await
        }
        _ => run_tray_daemon(log_tx).await,
    }
}

async fn run_tray_daemon(log_tx: broadcast::Sender<String>) -> Result<()> {
    let event_loop = EventLoopBuilder::new().build();

    let menu = Menu::new();
    let open_i = MenuItem::new("Open NeurArr", true, None);
    let settings_i = MenuItem::new("Settings", true, None);
    let logs_i = MenuItem::new("View Logs", true, None);
    let update_i = MenuItem::new("Restart to Update", true, None);
    let quit_i = MenuItem::new("Quit NeurArr", true, None);

    menu.append_items(&[
        &open_i,
        &settings_i,
        &logs_i,
        &PredefinedMenuItem::separator(),
        &update_i,
        &PredefinedMenuItem::separator(),
        &quit_i,
    ]).unwrap();

    let icon = load_icon().unwrap();

    let mut tray_icon = Some(
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("NeurArr Pro")
            .with_icon(icon)
            .build()
            .unwrap(),
    );

    let menu_channel = MenuEvent::receiver();
    let tray_channel = TrayIconEvent::receiver();

    let log_tx_inner = log_tx.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            if let Err(e) = run_daemon(log_tx_inner).await {
                error!("Daemon error: {:?}", e);
            }
        });
    });

    event_loop.run(move |_event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if let Ok(event) = menu_channel.try_recv() {
            if event.id == open_i.id() {
                let _ = open::that("http://localhost:3000/");
            } else if event.id == settings_i.id() {
                let _ = open::that("http://localhost:3000/");
            } else if event.id == logs_i.id() {
                let _ = open::that("http://localhost:3000/");
            } else if event.id == update_i.id() {
                std::thread::spawn(|| {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    let _ = rt.block_on(async { update_app().await });
                });
            } else if event.id == quit_i.id() {
                tray_icon.take();
                *control_flow = ControlFlow::Exit;
            }
        }

        if let Ok(event) = tray_channel.try_recv() {
            match event {
                TrayIconEvent::Click { button: tray_icon::MouseButton::Left, .. } => {
                    let _ = open::that("http://localhost:3000/");
                }
                _ => {}
            }
        }
    });
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
                            if alts.iter().any(|(_, a)| a.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "") == normalized_filename_title) {
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

pub async fn run_automation_cycle(pool: sqlx::SqlitePool, tmdb: TmdbClient, ollama: Arc<OllamaClient>, qbit: Arc<QBittorrentClient>, target_show_id: Option<i64>) -> Result<()> {
    let indexer = crate::integrations::indexer::IndexerClient::new().unwrap();
    let profile = db::get_default_quality_profile(&pool).await.ok();

    info!("Automation: Starting cycle (Target: {:?})...", target_show_id);
    
    if let Err(e) = qbit.login().await {
        error!("Automation: Failed to log into qBittorrent: {:?}", e);
        return Ok(());
    }

    if let Ok(tracked) = db::get_tracked_shows(&pool).await {
        for show in tracked { 
            if let Some(tid) = target_show_id { if show.id != tid { continue; } }
            if show.media_type == "tv" && show.status != "watchlist" { let _ = sync_show_episodes(&pool, &tmdb, show.id).await; }
        }
    }

    if let Ok(needed_seasons) = db::get_needed_seasons(&pool).await {
        for (season_num, show) in needed_seasons {
            if let Some(tid) = target_show_id { if show.id != tid { continue; } }
            let s_code = format!("S{:02}", season_num);
            let mut queries = Vec::new();
            let year_str = show.year.map(|y| y.to_string()).unwrap_or_default();
            queries.push(format!("{} {}", show.title, s_code));
            if !year_str.is_empty() { queries.push(format!("{} {} {}", show.title, year_str, s_code)); }
            if let Ok(alts) = tmdb.get_alternative_titles(show.tmdb_id as u32, true).await {
                for (iso, alt) in alts { 
                    if ["US", "GB", "AU", "CA"].contains(&iso.as_str()) || alt.chars().all(|c| c.is_ascii()) {
                        queries.push(format!("{} {}", alt, s_code)); 
                    }
                }
            }
            let mut found = false;
            for q in queries {
                if found { break; }
                info!("Automation: Searching for pack: {}", q);
                if let Ok(res) = indexer.search(&q).await {
                    let filtered: Vec<_> = res.into_iter().filter(|r| {
                        let t = r.title.to_lowercase();
                        if let Some(y) = show.year {
                            let y_s = y.to_string();
                            let other_year = regex::Regex::new(r"\b(19|20)\d{2}\b").unwrap().find_iter(&r.title).any(|m| m.as_str() != y_s);
                            if other_year && !t.contains(&y_s) { return false; }
                        }
                        t.contains(&s_code.to_lowercase()) && (t.contains("complete") || t.contains("season") || !t.contains("e0"))
                    }).collect();
                    for best in filtered {
                        let target_norm = show.title.to_lowercase().replace(|c: char| !c.is_alphanumeric() && !c.is_whitespace(), "");
                        let torrent_norm = best.title.to_lowercase().replace(|c: char| !c.is_alphanumeric() && !c.is_whitespace(), "");
                        let target_words: Vec<_> = target_norm.split_whitespace().collect();
                        let is_string_match = target_words.iter().all(|w| torrent_norm.contains(w));
                        
                        let verified = if is_string_match {
                            info!("Automation [LOCAL]: High-confidence match for '{}' - Skipping AI", best.title);
                            true
                        } else {
                            info!("Automation [AI]: Fuzzy match suspected for '{}', verifying...", best.title);
                            ollama.verify_torrent_match(&show.title, &best.title).await.unwrap_or(false)
                        };

                        if verified {
                            info!("Automation: Confirmed and adding pack: {}", best.title);
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
            if let Some(tid) = target_show_id { if show.id != tid { continue; } }
            let ep_code = format!("S{:02}E{:02}", ep.season, ep.episode);
            let mut queries = Vec::new();
            let year_str = show.year.map(|y| y.to_string()).unwrap_or_default();
            queries.push(format!("{} {}", show.title, ep_code));
            if !year_str.is_empty() { queries.push(format!("{} {} {}", show.title, year_str, ep_code)); }
            if let Ok(alts) = tmdb.get_alternative_titles(show.tmdb_id as u32, true).await {
                for (iso, alt) in alts { 
                    if ["US", "GB", "AU", "CA"].contains(&iso.as_str()) || alt.chars().all(|c| c.is_ascii()) {
                        queries.push(format!("{} {}", alt, ep_code)); 
                    }
                }
            }
            let mut found = false;
            for q in queries {
                if found { break; }
                info!("Automation: Searching for episode: {}", q);
                if let Ok(res) = indexer.search(&q).await {
                    let filtered: Vec<_> = res.into_iter().filter(|r| {
                        let t = r.title.to_lowercase();
                        if let Some(y) = show.year {
                            let y_s = y.to_string();
                            let other_year = regex::Regex::new(r"\b(19|20)\d{2}\b").unwrap().find_iter(&r.title).any(|m| m.as_str() != y_s);
                            if other_year && !t.contains(&y_s) { return false; }
                        }
                        if let Some(p) = &profile {
                            if let Some(must) = &p.must_contain { if !must.is_empty() && !t.contains(must) { return false; } }
                            if let Some(not) = &p.must_not_contain { for w in not.split(',') { if !w.trim().is_empty() && t.contains(w.trim().to_lowercase().as_str()) { return false; } } }
                            if p.max_resolution == "1080p" && t.contains("2160p") { return false; }
                        }
                        true
                    }).collect();
                    for best in filtered {
                        let target_norm = show.title.to_lowercase().replace(|c: char| !c.is_alphanumeric() && !c.is_whitespace(), "");
                        let torrent_norm = best.title.to_lowercase().replace(|c: char| !c.is_alphanumeric() && !c.is_whitespace(), "");
                        let target_words: Vec<_> = target_norm.split_whitespace().collect();
                        let is_string_match = target_words.iter().all(|w| torrent_norm.contains(w));

                        let verified = if is_string_match {
                            info!("Automation [LOCAL]: High-confidence match for '{}' - Skipping AI", best.title);
                            true
                        } else {
                            info!("Automation [AI]: Fuzzy match suspected for '{}', verifying...", best.title);
                            ollama.verify_torrent_match(&show.title, &best.title).await.unwrap_or(false)
                        };

                        if verified {
                            info!("Automation: Confirmed and adding episode: {}", best.title);
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

    if let Ok(wanted_movies) = db::get_wanted_movies(&pool).await {
        for movie in wanted_movies {
            if let Some(tid) = target_show_id { if movie.id != tid { continue; } }
            let mut queries = Vec::new();
            let year_str = movie.year.map(|y| y.to_string()).unwrap_or_default();
            queries.push(movie.title.clone());
            if !year_str.is_empty() { queries.push(format!("{} {}", movie.title, year_str)); }
            if let Ok(alts) = tmdb.get_alternative_titles(movie.tmdb_id as u32, false).await {
                for (iso, alt) in alts { 
                    if ["US", "GB", "AU", "CA"].contains(&iso.as_str()) || alt.chars().all(|c| c.is_ascii()) {
                        queries.push(alt); 
                    }
                }
            }
            let mut found = false;
            for q in queries {
                if found { break; }
                info!("Automation: Searching for movie: {}", q);
                if let Ok(res) = indexer.search(&q).await {
                    let filtered: Vec<_> = res.into_iter().filter(|r| {
                        let t = r.title.to_lowercase();
                        if let Some(y) = movie.year {
                            let y_s = y.to_string();
                            let other_year = regex::Regex::new(r"\b(19|20)\d{2}\b").unwrap().find_iter(&r.title).any(|m| m.as_str() != y_s);
                            if other_year && !t.contains(&y_s) { return false; }
                        }
                        if let Some(p) = &profile {
                            if let Some(must) = &p.must_contain { if !must.is_empty() && !t.contains(must) { return false; } }
                            if let Some(not) = &p.must_not_contain { for w in not.split(',') { if !w.trim().is_empty() && t.contains(w.trim().to_lowercase().as_str()) { return false; } } }
                            if p.max_resolution == "1080p" && t.contains("2160p") { return false; }
                        }
                        !t.contains("soundtrack") && !t.contains("ost")
                    }).collect();
                    for best in filtered {
                        let target_norm = movie.title.to_lowercase().replace(|c: char| !c.is_alphanumeric() && !c.is_whitespace(), "");
                        let torrent_norm = best.title.to_lowercase().replace(|c: char| !c.is_alphanumeric() && !c.is_whitespace(), "");
                        let target_words: Vec<_> = target_norm.split_whitespace().collect();
                        let is_string_match = target_words.iter().all(|w| torrent_norm.contains(w));

                        let verified = if is_string_match {
                            info!("Automation [LOCAL]: High-confidence match for '{}' - Skipping AI", best.title);
                            true
                        } else {
                            info!("Automation [AI]: Fuzzy match suspected for '{}', verifying...", best.title);
                            ollama.verify_torrent_match(&movie.title, &best.title).await.unwrap_or(false)
                        };

                        if verified {
                            info!("Automation: Confirmed and adding movie: {}", best.title);
                            let ingest = std::fs::canonicalize("./ingest").unwrap_or_else(|_| PathBuf::from("./ingest"));
                            if qbit.add_torrent_url(&best.link, Some(&ingest.to_string_lossy())).await.is_ok() {
                                let _ = db::update_tracked_show_status(&pool, movie.id, "downloading").await;
                                let _ = db::insert_pending_download(&pool, &best.title, Some(movie.id), None, movie.tmdb_id as u32, "movie", None).await;
                                found = true; break;
                            }
                        }
                    }
                }
            }
        }
    }

    info!("Automation: Cycle complete.");
    Ok(())
}

pub async fn scan_ingest_folder(pool: sqlx::SqlitePool, tmdb: TmdbClient, ollama: Arc<OllamaClient>, _qbit: Arc<QBittorrentClient>) -> Result<()> {
    let ingest_dir = env::var("NEURARR_INGEST_DIR").unwrap_or_else(|_| "./ingest".to_string());
    let mut scanner = Scanner::new()?;
    scanner.scan(pool, tmdb, ollama, _qbit, PathBuf::from(ingest_dir)).await
}

async fn run_daemon(log_tx: broadcast::Sender<String>) -> Result<()> {
    let pool = init_db().await?;
    let tmdb_client = TmdbClient::new().unwrap_or_else(|_| {
        info!("TMDB client initialized in degraded mode");
        TmdbClient { client: reqwest::Client::new(), api_key: "MISSING".to_string() }
    });
    let ollama = Arc::new(OllamaClient::new().unwrap_or_else(|_| {
        info!("Ollama client initialized in degraded mode");
        OllamaClient::new().unwrap()
    }));
    let qbit = Arc::new(QBittorrentClient::new().unwrap_or_else(|_| {
        info!("qBittorrent client initialized in degraded mode");
        QBittorrentClient::new().unwrap()
    }));
    let _ = qbit.login().await;

    let scanner_pool = pool.clone();
    let scanner_tmdb = tmdb_client.clone();
    let scanner_ollama = ollama.clone();
    let scanner_qbit = qbit.clone();
    let ingest_dir = env::var("NEURARR_INGEST_DIR").unwrap_or_else(|_| "./ingest".to_string());
    
    let ai_semaphore = Arc::new(Semaphore::new(1));
    let processing_registry = Arc::new(Mutex::new(HashSet::new()));

    let mut scanner = Scanner::new()?;
    let _ = scanner.watch(PathBuf::from(&ingest_dir))?;

    tokio::spawn(async move {
        while let Some(res) = scanner.next_event().await {
            if let Ok(event) = res {
                for path in event.paths {
                    let path_clone = path.clone();
                    if path_clone.exists() {
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

    let initial_pool = pool.clone();
    let initial_tmdb = tmdb_client.clone();
    let initial_ollama = ollama.clone();
    let initial_qbit = qbit.clone();
    tokio::spawn(async move {
        let _ = scan_ingest_folder(initial_pool, initial_tmdb, initial_ollama, initial_qbit).await;
    });

    let scheduler_pool = pool.clone();
    let scheduler_tmdb = tmdb_client.clone();
    let scheduler_ollama = ollama.clone();
    let scheduler_qbit = qbit.clone();
    
    tokio::spawn(async move {
        loop {
            let _ = run_automation_cycle(scheduler_pool.clone(), scheduler_tmdb.clone(), scheduler_ollama.clone(), scheduler_qbit.clone(), None).await;
            let _ = scan_ingest_folder(scheduler_pool.clone(), scheduler_tmdb.clone(), scheduler_ollama.clone(), scheduler_qbit.clone()).await;
            tokio::time::sleep(std::time::Duration::from_secs(1800)).await;
        }
    });

    start_web_server(pool, log_tx).await?;
    Ok(())
}

use std::fs::OpenOptions;

fn is_file_locked(path: &PathBuf) -> bool {
    // Try to open the file in append mode. If it fails, it's likely being written to.
    // Note: This is a common heuristic on Windows/Unix for finding exclusive locks.
    match OpenOptions::new().append(true).open(path) {
        Ok(_) => false,
        Err(_) => true,
    }
}

async fn process_file(path: PathBuf, pool: sqlx::SqlitePool, tmdb: TmdbClient, ollama: Arc<OllamaClient>, _qbit: Arc<QBittorrentClient>) -> Result<()> {
    if is_file_locked(&path) {
        info!("Ingest: Skipping {} (File is currently locked/writing)", path.display());
        return Ok(());
    }

    let filename = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
    if filename.is_empty() { return Ok(()); }

    // Fast path: Check pending downloads
    if let Ok(Some(pending)) = db::get_pending_download(&pool, filename).await {
        info!("Ingest: Identified {} from pending (Stage: Fast)", filename);
        let details = if pending.media_type == "tv" { tmdb.get_tv_details(pending.tmdb_id as u32).await? } else { tmdb.get_movie_details(pending.tmdb_id as u32).await? };
        let final_title = details.name.or(details.title).unwrap_or_else(|| "Unknown".to_string());

        if path.is_dir() {
            for entry in WalkDir::new(&path).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    let f = entry.path().file_name().and_then(|s| s.to_str()).unwrap_or("");
                    if ["mkv", "mp4", "avi", "mov"].contains(&entry.path().extension().and_then(|e| e.to_str()).unwrap_or("")) {
                        let meta = Parser::parse_regex(f);
                        let summary = ollama.rewrite_summary(&details.overview.as_deref().unwrap_or_default()).await.unwrap_or_default();
                        let item_id = db::insert_media_item(&pool, f, &meta).await?;
                        db::update_media_item_full(&pool, item_id, pending.tmdb_id as u32, &final_title, summary, meta.season.map(|s| s as i32), meta.episode.map(|e| e as i32)).await?;
                        if let (Some(s), Some(e)) = (meta.season, meta.episode) {
                            if let Some(sid) = pending.show_id { let _ = db::update_episode_status_completed(&pool, sid, s as i32, e as i32).await; }
                        }
                        let _ = Renamer::new(env::var("NEURARR_LIBRARY_DIR")?).move_file(entry.path(), &meta, &final_title).await;
                    }
                }
            }
            let _ = tokio::fs::remove_dir_all(&path).await;
        } else {
            let meta = Parser::parse_regex(filename);
            let item_id = db::insert_media_item(&pool, filename, &meta).await?;
            run_pipeline(item_id, path, pool, tmdb, ollama, Some(pending.tmdb_id as u32)).await?;
        }
        return Ok(());
    }

    // Slow path: Match and process
    if path.is_dir() {
        // Try to match the directory name itself
        let metadata = Parser::parse_regex(filename);
        let local_show = db::get_tracked_show_by_title(&pool, &metadata.title).await.unwrap_or(None);
        
        if let Some(show) = local_show {
            let tmdb_id = show.tmdb_id as u32;
            let show_id = show.id;
            info!("Ingest: Identified directory pack '{}' (Stage: Local)", filename);
            let details = if metadata.season.is_some() { tmdb.get_tv_details(tmdb_id).await? } else { tmdb.get_movie_details(tmdb_id).await? };
            let final_title = details.name.or(details.title).unwrap_or_else(|| "Unknown".to_string());

            for entry in WalkDir::new(&path).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    let f = entry.path().file_name().and_then(|s| s.to_str()).unwrap_or("");
                    if ["mkv", "mp4", "avi", "mov"].contains(&entry.path().extension().and_then(|e| e.to_str()).unwrap_or("")) {
                        let meta = Parser::parse_regex(f);
                        let summary = ollama.rewrite_summary(&details.overview.as_deref().unwrap_or_default()).await.unwrap_or_default();
                        let item_id = db::insert_media_item(&pool, f, &meta).await?;
                        db::update_media_item_full(&pool, item_id, tmdb_id, &final_title, summary, meta.season.map(|s| s as i32), meta.episode.map(|e| e as i32)).await?;
                        if let (Some(s), Some(e)) = (meta.season, meta.episode) {
                            let _ = db::update_episode_status_completed(&pool, show_id, s as i32, e as i32).await;
                        }
                        let _ = Renamer::new(env::var("NEURARR_LIBRARY_DIR")?).move_file(entry.path(), &meta, &final_title).await;
                    }
                }
            }
            let _ = tokio::fs::remove_dir_all(&path).await;
        }
    } else if path.is_file() {
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
        // --- STAGE 1: LOCAL (Database & Manual) ---
        if let Ok(Some(id)) = db::get_manual_match(&pool, &metadata.title).await { 
            info!("Ingest [LOCAL]: Found manual match for '{}'", metadata.title);
            id as u32 
        } 
        else if let Ok(Some(show)) = db::get_tracked_show_by_title(&pool, &metadata.title).await {
            info!("Ingest [LOCAL]: Found collection match for '{}'", metadata.title);
            show.tmdb_id as u32
        }
        else {
            // --- STAGE 2: TMDB (Exact/Heuristic Search) ---
            info!("Ingest [TMDB]: Searching for '{}'...", metadata.title);
            let results = if metadata.season.is_some() { tmdb.search_tv(&metadata.title, None).await? } else { tmdb.search_movie(&metadata.title, None).await? };
            
            let best_match = results.iter().find(|res| {
                let tmdb_title = res.title.clone().or(res.name.clone()).unwrap_or_default().to_lowercase().replace(|c: char| !c.is_alphanumeric(), "");
                let search_title = metadata.title.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "");
                tmdb_title == search_title
            });

            if let Some(m) = best_match {
                info!("Ingest [TMDB]: Found exact title match: {}", m.title.clone().or(m.name.clone()).unwrap_or_default());
                m.id
            } else {
                // --- STAGE 3: AI (Local LLM Verification) ---
                if let Some(best) = results.first() {
                    let target_name = best.title.clone().or(best.name.clone()).unwrap_or_default();
                    info!("Ingest [AI]: Verifying fuzzy match '{}' vs '{}'...", metadata.title, target_name);
                    if let Ok(true) = ollama.verify_torrent_match(&metadata.title, &target_name).await {
                        info!("Ingest [AI]: Match confirmed by neural logic.");
                        best.id
                    } else {
                        info!("Ingest: No confident match found at any stage for '{}'", metadata.title);
                        return Ok(()); 
                    }
                } else {
                    return Ok(());
                }
            }
        }
    };
    let details = if metadata.season.is_some() { tmdb.get_tv_details(tmdb_id).await? } else { tmdb.get_movie_details(tmdb_id).await? };
    let final_title = details.name.or(details.title).unwrap_or_else(|| "Unknown".to_string());
    let summary = ollama.rewrite_summary(&details.overview.unwrap_or_default()).await?;
    db::update_media_item_full(&pool, item_id, tmdb_id, &final_title, summary, metadata.season.map(|s| s as i32), metadata.episode.map(|e| e as i32)).await?;
    
    // Update episode/show status to completed
    if let (Some(s), Some(e)) = (metadata.season, metadata.episode) {
        if let Ok(tracked) = db::get_tracked_shows(&pool).await {
            if let Some(show) = tracked.iter().find(|t| t.tmdb_id == tmdb_id as i64) {
                let _ = db::update_episode_status_completed(&pool, show.id, s as i32, e as i32).await;
            }
        }
    } else if metadata.season.is_none() {
        if let Ok(tracked) = db::get_tracked_shows(&pool).await {
            if let Some(show) = tracked.iter().find(|t| t.tmdb_id == tmdb_id as i64) {
                let _ = db::update_tracked_show_status(&pool, show.id, "completed").await;
            }
        }
    }

    let renamer = Renamer::new(env::var("NEURARR_LIBRARY_DIR")?);
    renamer.move_file(&path, &metadata, &final_title).await?;
    Ok(())
}

async fn update_app() -> Result<()> {
    info!("Starting NeurArr update process...");
    let status = std::process::Command::new("git").arg("pull").status()?;
    if status.success() {
        info!("Successfully pulled latest changes. Rebuilding...");
        
        #[cfg(target_os = "windows")]
        {
            if let Ok(current_exe) = std::env::current_exe() {
                let old_exe = current_exe.with_extension("old.exe");
                let _ = std::fs::remove_file(&old_exe);
                let _ = std::fs::rename(&current_exe, &old_exe);
            }
        }

        let build_status = std::process::Command::new("cargo").arg("build").arg("--release").status()?;
        if build_status.success() {
            info!("Build successful. Restarting...");
            let current_exe = std::env::current_exe()?;
            std::process::Command::new(current_exe).spawn()?;
            std::process::exit(0);
        } else {
            anyhow::bail!("Build failed. Update aborted.");
        }
    }
    Ok(())
}

async fn start_web_server(pool: sqlx::SqlitePool, log_tx: broadcast::Sender<String>) -> Result<()> {
    crate::web::start_web_server(pool, log_tx).await
}

use std::collections::HashSet;
