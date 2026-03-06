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
use crate::llm::OllamaClient;
use crate::parser::Parser;
use crate::utils::{send_notification, Renamer};

use anyhow::{Context, Result};
use clap::{Parser as ClapParser, Subcommand};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore, broadcast};
use tracing::{info, error};
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
                let hash = utils::auth::hash_password("admin");
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
                        info!("Scanner: Matched {} to tracked show: {}", filename, show.title);
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

async fn update_app() -> Result<()> {
    info!("Starting NeurArr update process...");

    info!("Pulling latest changes from GitHub...");
    let status = std::process::Command::new("git")
        .arg("pull")
        .status()
        .context("Failed to execute git pull.")?;

    if !status.success() { anyhow::bail!("Git pull failed."); }

    #[cfg(windows)]
    {
        let exe_path = std::env::current_exe()?;
        let old_exe = exe_path.with_extension("old");
        if old_exe.exists() { let _ = std::fs::remove_file(&old_exe); }
        std::fs::rename(&exe_path, &old_exe).context("Failed to rename running executable.")?;
    }

    info!("Building the latest version...");
    let status = std::process::Command::new("cargo")
        .args(["build", "--release"])
        .status()
        .context("Failed to execute cargo build.")?;

    if !status.success() {
        #[cfg(windows)]
        {
            let exe_path = std::env::current_exe()?;
            let old_exe = exe_path.with_extension("old");
            let _ = std::fs::rename(&old_exe, &exe_path);
        }
        anyhow::bail!("Build failed.");
    }

    info!("Update successful!");

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut args = std::env::args();
        let cmd = args.next().unwrap();
        let err = std::process::Command::new(cmd).args(args).exec();
        return Err(anyhow::anyhow!("Failed to restart: {}", err));
    }

    #[cfg(windows)]
    {
        info!("Restart NeurArr manually to apply changes.");
        std::process::exit(0);
    }

    #[cfg(not(any(unix, windows)))]
    Ok(())
}

async fn run_daemon(log_tx: broadcast::Sender<String>) -> Result<()> {
    info!("NeurArr Pro starting up...");
    let pool = init_db().await?;
    let tmdb_client = TmdbClient::new()?;
    let ollama = Arc::new(OllamaClient::new()?);
    let qbit = Arc::new(crate::integrations::torrent::QBittorrentClient::new()?);
    let _ = qbit.login().await;

    let watch_path = env::var("NEURARR_INGEST_DIR").unwrap_or_else(|_| "ingest".to_string());
    if !std::path::Path::new(&watch_path).exists() { std::fs::create_dir_all(&watch_path)?; }

    let mut scanner = Scanner::new()?;
    scanner.watch(PathBuf::from(&watch_path))?;

    let processing_registry = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let ai_semaphore = Arc::new(Semaphore::new(1));

    let scanner_pool = pool.clone();
    let scanner_tmdb = tmdb_client.clone();
    let scanner_ollama = ollama.clone();
    let scanner_qbit = qbit.clone();
    let scanner_handle = async move {
        let mut scanner = scanner;
        while let Some(event_res) = scanner.next_event().await {
            if let Ok(event) = event_res {
                use notify::event::EventKind;
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) => {
                        for path in event.paths {
                            if path.is_file() {
                                if path.file_name().unwrap().to_string_lossy().starts_with('.') { continue; }
                                let mut registry: tokio::sync::MutexGuard<std::collections::HashSet<PathBuf>> = processing_registry.lock().await;
                                if registry.contains(&path) { continue; }
                                registry.insert(path.clone());
                                drop(registry);

                                let pool = scanner_pool.clone();
                                let tmdb = scanner_tmdb.clone();
                                let ollama = scanner_ollama.clone();
                                let qbit_clone = scanner_qbit.clone();
                                let registry = processing_registry.clone();
                                let sem = ai_semaphore.clone();
                                tokio::spawn(async move {
                                    let _permit = sem.acquire().await.ok();
                                    let _ = process_file(path.clone(), pool, tmdb, ollama, qbit_clone).await;
                                    registry.lock().await.remove(&path);
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    // Initial ingest scan on startup
    let _ = scan_ingest_folder(pool.clone(), tmdb_client.clone(), ollama.clone(), qbit.clone()).await;

    let web_handle = crate::web::start_web_server(pool.clone(), log_tx);

    let scheduler_pool = pool.clone();
    let scheduler_tmdb = tmdb_client.clone();
    let scheduler_ollama = ollama.clone();
    let scheduler_qbit = qbit.clone();
    let scheduler_handle = tokio::spawn(async move {
        let indexer = crate::integrations::indexer::IndexerClient::new().unwrap();
        let mut last_ingest_scan = tokio::time::Instant::now();
        loop {
            // Periodic ingest scan every 30 minutes
            if last_ingest_scan.elapsed().as_secs() > 1800 {
                let _ = scan_ingest_folder(scheduler_pool.clone(), scheduler_tmdb.clone(), scheduler_ollama.clone(), scheduler_qbit.clone()).await;
                last_ingest_scan = tokio::time::Instant::now();
            }

            let profile = db::get_default_quality_profile(&scheduler_pool).await.ok();

            
            if let Ok(tracked) = db::get_tracked_shows(&scheduler_pool).await {
                for show in tracked {
                    if show.media_type == "tv" {
                        if let Ok(full) = scheduler_tmdb.get_tv_details(show.tmdb_id as u32).await {
                            let seasons = full.number_of_seasons.unwrap_or(1);
                            for s in 1..=seasons {
                                if let Ok(eps) = scheduler_tmdb.get_tv_season(show.tmdb_id as u32, s).await {
                                    for ep in eps {
                                        let aired = if let Some(d) = &ep.air_date {
                                            if d.is_empty() { false }
                                            else { d <= &chrono::Utc::now().date_naive().to_string() }
                                        } else { false };
                                        
                                        if aired {
                                            let _ = db::insert_episode(&scheduler_pool, show.id, ep.season_number, ep.episode_number, &ep.name, ep.air_date, "wanted").await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Ok(wanted_eps) = db::get_wanted_episodes(&scheduler_pool).await {
                for (ep, show) in wanted_eps {
                    let ep_code = format!("S{:02}E{:02}", ep.season, ep.episode);
                    let mut queries = vec![format!("{} {}", show.title, ep_code)];
                    if let Ok(alts) = scheduler_tmdb.get_alternative_titles(show.tmdb_id as u32, true).await {
                        for alt in alts { queries.push(format!("{} {}", alt, ep_code)); }
                    }
                    let mut found = false;
                    let mut seen_torrents = std::collections::HashSet::new();
                    for q in queries {
                        if found { break; }
                        match indexer.search(&q).await {
                            Ok(res) => {
                                info!("Found {} results for query: {}", res.len(), q);
                                let filtered: Vec<_> = res.into_iter().filter(|r| {
                                    if let Some(p) = &profile {
                                        let t = r.title.to_lowercase();
                                        if let Some(must) = &p.must_contain { if !must.is_empty() && !t.contains(must) { 
                                            info!("Filtered out '{}' (missing must_contain: {})", r.title, must);
                                            return false; 
                                        } }
                                        if let Some(not) = &p.must_not_contain { 
                                            for w in not.split(',') { 
                                                let tag = w.trim().to_lowercase();
                                                if tag.is_empty() { continue; }
                                                
                                                // Create a list of "words" by splitting on common separators
                                                let title_parts: Vec<_> = t.split(|c: char| !c.is_alphanumeric())
                                                    .filter(|s| !s.is_empty())
                                                    .collect();
                                                
                                                if title_parts.contains(&tag.as_str()) {
                                                    info!("Filtered out '{}' (contains must_not_contain tag: {})", r.title, tag);
                                                    return false; 
                                                }
                                            } 
                                        }
                                        if p.max_resolution == "1080p" && t.contains("2160p") { 
                                            info!("Filtered out '{}' (resolution too high)", r.title);
                                            return false; 
                                        }
                                    }
                                    true
                                }).collect();
                                for best in filtered {
                                    if seen_torrents.contains(&best.link) { continue; }
                                    seen_torrents.insert(best.link.clone());

                                    info!("Verifying match for torrent: '{}' with target title: '{}'", best.title, show.title);
                                    
                                    // Simple string pre-verification to save LLM time
                                    let normalize = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>();
                                    let target_norm = normalize(&show.title);
                                    let torrent_norm = normalize(&best.title);
                                    let is_string_match = torrent_norm.contains(&target_norm);
                                    
                                    let verified = if is_string_match {
                                        info!("String match confirmed for: {}", best.title);
                                        true
                                    } else {
                                        // Before asking LLM, check if there's at least some word overlap
                                        // to avoid asking about completely unrelated shows.
                                        let target_words: std::collections::HashSet<_> = target_norm.split_whitespace()
                                            .filter(|w| w.len() > 2) // Ignore tiny words like 'a', 'of', 'the'
                                            .collect();
                                        let torrent_words: std::collections::HashSet<_> = torrent_norm.split_whitespace().collect();
                                        let has_overlap = target_words.iter().any(|w| torrent_words.contains(w));

                                        if !has_overlap {
                                            info!("No word overlap for: {}. Skipping LLM.", best.title);
                                            false
                                        } else {
                                            match scheduler_ollama.verify_torrent_match(&show.title, &best.title).await {
                                                Ok(v) => v,
                                                Err(e) => {
                                                    error!("LLM verification error: {}", e);
                                                    false
                                                }
                                            }
                                        }
                                    };

                                    if verified {
                                        info!("Verified match for: {}", best.title);
                                        let ingest = std::fs::canonicalize("./ingest").unwrap_or_else(|_| PathBuf::from("./ingest"));
                                        if scheduler_qbit.add_torrent_url(&best.link, Some(&ingest.to_string_lossy())).await.is_ok() {
                                            send_notification("NeurArr", &format!("Downloading: {}", best.title));
                                            let _ = db::update_episode_status(&scheduler_pool, ep.id, "downloading").await;
                                            let _ = db::reset_episode_attempts(&scheduler_pool, ep.id).await;
                                            let _ = db::insert_pending_download(&scheduler_pool, &best.title, Some(show.id), Some(ep.id), show.tmdb_id as u32, "tv").await;
                                            found = true; break;
                                        }
 else {
                                            error!("Failed to add torrent to qbit: {}", best.title);
                                        }
                                    } else {
                                        info!("Rejected match for: {}", best.title);
                                    }
                                }
                            },
                            Err(e) => {
                                error!("Indexer search error for query {}: {}", q, e);
                            }
                        }
                    }
                    if !found {
                        let _ = db::increment_episode_attempts(&scheduler_pool, ep.id).await;
                    }
                    // Small delay between episodes to avoid overwhelming services
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }

            if let Ok(wanted_movies) = db::get_wanted_movies(&scheduler_pool).await {
                for movie in wanted_movies {
                    let mut queries = vec![movie.title.clone()];
                    if let Ok(alts) = scheduler_tmdb.get_alternative_titles(movie.tmdb_id as u32, false).await {
                        for alt in alts { queries.push(alt); }
                    }
                    let mut found = false;
                    let mut seen_torrents = std::collections::HashSet::new();
                    for q in queries {
                        if found { break; }
                        match indexer.search(&q).await {
                            Ok(res) => {
                                info!("Found {} results for movie query: {}", res.len(), q);
                                let filtered: Vec<_> = res.into_iter().filter(|r| {
                                    if let Some(p) = &profile {
                                        let t = r.title.to_lowercase();
                                        if let Some(must) = &p.must_contain { if !must.is_empty() && !t.contains(must) { return false; } }
                                        if let Some(not) = &p.must_not_contain { 
                                            for w in not.split(',') { 
                                                let tag = w.trim().to_lowercase();
                                                if tag.is_empty() { continue; }
                                                let title_parts: Vec<_> = t.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).collect();
                                                if title_parts.contains(&tag.as_str()) { return false; }
                                            } 
                                        }
                                        if p.max_resolution == "1080p" && t.contains("2160p") { return false; }
                                    }
                                    true
                                }).collect();
                                for best in filtered {
                                    if seen_torrents.contains(&best.link) { continue; }
                                    seen_torrents.insert(best.link.clone());

                                    let normalize = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>();
                                    let target_norm = normalize(&movie.title);
                                    let torrent_norm = normalize(&best.title);
                                    let is_string_match = torrent_norm.contains(&target_norm);
                                    
                                    let verified = if is_string_match {
                                        info!("String match confirmed for movie: {}", best.title);
                                        true
                                    } else {
                                        let target_words: std::collections::HashSet<_> = target_norm.split_whitespace().filter(|w| w.len() > 2).collect();
                                        let torrent_words: std::collections::HashSet<_> = torrent_norm.split_whitespace().collect();
                                        let mut has_overlap = target_words.iter().any(|w| torrent_words.contains(w));

                                        // For movies, if year is known, it MUST overlap with torrent name if possible
                                        if let Some(y) = movie.year {
                                            let year_str = y.to_string();
                                            if !best.title.contains(&year_str) {
                                                // If torrent has NO year, we might still check, but if it has a WRONG year, we fail
                                                let re = regex::Regex::new(r"(19|20)\d{2}").unwrap();
                                                if let Some(caps) = re.find(&best.title) {
                                                    if caps.as_str() != year_str {
                                                        info!("Year mismatch for movie: {} (expected {})", best.title, year_str);
                                                        has_overlap = false;
                                                    }
                                                }
                                            }
                                        }

                                        if !has_overlap {
                                            false
                                        } else {
                                            match scheduler_ollama.verify_torrent_match(&movie.title, &best.title).await {
                                                Ok(v) => v,
                                                Err(_) => false
                                            }
                                        }
                                    };

                                    if verified {
                                        info!("Verified movie match: {}", best.title);
                                        let ingest = std::fs::canonicalize("./ingest").unwrap_or_else(|_| PathBuf::from("./ingest"));
                                        if scheduler_qbit.add_torrent_url(&best.link, Some(&ingest.to_string_lossy())).await.is_ok() {
                                            send_notification("NeurArr", &format!("Downloading Movie: {}", best.title));
                                            let _ = db::update_tracked_show_status(&scheduler_pool, movie.id, "downloading").await;
                                            let _ = db::reset_movie_attempts(&scheduler_pool, movie.id).await;
                                            let _ = db::insert_pending_download(&scheduler_pool, &best.title, Some(movie.id), None, movie.tmdb_id as u32, "movie").await;
                                            found = true; break;
                                        }
                                    }
                                }
                            },
                            Err(e) => error!("Indexer search error for movie query {}: {}", q, e),
                        }
                    }
                    if !found {
                        let _ = db::increment_movie_attempts(&scheduler_pool, movie.id).await;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        }
    });

    tokio::select! {
        res = scanner_handle => res?,
        res = web_handle => res?,
        _ = scheduler_handle => {},
    }
    Ok(())
}

async fn is_file_locked(path: &PathBuf) -> bool {
    // Try to open the file in write mode to see if it's locked by another process (like qbit)
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(_) => false, // We can open it for writing, so it's likely not locked
        Err(_) => true,  // Could not open, likely locked
    }
}

async fn wait_for_torrent_completion(path: &PathBuf, qbit: &Arc<crate::integrations::torrent::QBittorrentClient>) -> bool {
    let filename = path.file_name().unwrap().to_string_lossy().to_string();
    
    for _ in 0..120 { // Timeout after 10 minutes
        let mut qbit_says_done = false;
        if let Ok(torrents) = qbit.get_torrents().await {
            if let Some(tor) = torrents.iter().find(|t| {
                filename.contains(&t.name) || t.name.contains(&filename)
            }) {
                let is_finished = ["uploading", "stalledUP", "queuedUP", "checkingUP", "forcedUP"]
                    .iter().any(|&s| tor.state == s);
                
                if is_finished || tor.progress >= 1.0 {
                    qbit_says_done = true;
                    // Don't delete yet, wait for lock check
                }
            }
        }

        // Even if qbit is done, check if we can get an exclusive lock on the file
        if qbit_says_done || !path.exists() {
            if path.exists() {
                if !is_file_locked(path).await {
                    info!("Torrent completion verified and file is unlocked: {:?}", path);
                    // Now safe to tell qbit to stop
                    if let Ok(torrents) = qbit.get_torrents().await {
                        if let Some(tor) = torrents.iter().find(|t| filename.contains(&t.name) || t.name.contains(&filename)) {
                            let _ = qbit.delete_torrent(&tor.hash, false).await;
                        }
                    }
                    return true;
                } else {
                    info!("qBit says done, but file is still locked by a process: {:?}", path);
                }
            } else {
                // If it's a directory (complex torrent), we might need more logic, 
                // but for single files this is usually enough.
                if qbit_says_done { return true; }
            }
        }
        
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }

    false
}

pub async fn scan_ingest_folder(pool: sqlx::SqlitePool, tmdb: TmdbClient, ollama: Arc<OllamaClient>, qbit: Arc<crate::integrations::torrent::QBittorrentClient>) -> Result<()> {
    let watch_path = std::env::var("NEURARR_INGEST_DIR").unwrap_or_else(|_| "ingest".to_string());
    info!("Manual ingest scan started for: {}", watch_path);
    
    for entry in WalkDir::new(&watch_path).max_depth(2).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let path = entry.path().to_path_buf();
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if !["mkv", "mp4", "avi", "mov"].contains(&ext) { continue; }
            if path.file_name().unwrap().to_string_lossy().starts_with('.') { continue; }

            let pool = pool.clone();
            let tmdb = tmdb.clone();
            let ollama = ollama.clone();
            let qbit = qbit.clone();
            tokio::spawn(async move {
                let _ = process_file(path, pool, tmdb, ollama, qbit).await;
            });
        }
    }
    Ok(())
}

pub async fn process_file(path: PathBuf, pool: sqlx::SqlitePool, tmdb: TmdbClient, ollama: Arc<OllamaClient>, qbit: Arc<crate::integrations::torrent::QBittorrentClient>) -> Result<()> {
    info!("Waiting for torrent completion: {:?}", path);
    if !wait_for_torrent_completion(&path, &qbit).await {
        anyhow::bail!("Torrent never finished or settled: {:?}", path);
    }
    
    let filename = path.file_name().unwrap().to_string_lossy().to_string();
    
    // Check if we already know what this file is (pre-match)
    if let Ok(Some(pending)) = db::get_pending_download(&pool, &filename).await {
        info!("Matched pending download for: {} (registered as: {}, show_id: {:?}). Skipping AI parsing.", filename, pending.torrent_name, pending.show_id);
        // Create a minimal metadata object based on what we know
        let mut metadata = crate::parser::MediaMetadata {
            title: filename.clone(), // Pipeline will use TMDB ID anyway
            season: None,
            episode: None,
            resolution: Some("Unknown".to_string()),
            source: Some("Indexer".to_string()),
        };

        if pending.media_type == "tv" {
            // Need to get season/episode from database if we have episode_id
            if let Some(eid) = pending.episode_id {
                if let Ok(rows) = sqlx::query("SELECT season, episode FROM episodes WHERE id = ?").bind(eid).fetch_all(&pool).await {
                    if let Some(r) = rows.first() {
                        use sqlx::Row;
                        metadata.season = Some(r.get::<i64, _>("season") as u32);
                        metadata.episode = Some(r.get::<i64, _>("episode") as u32);
                    }
                }
            }
        }

        let id = db::insert_media_item(&pool, &filename, &metadata).await?;
        let res = run_pipeline(id, path, pool.clone(), tmdb, ollama, Some(pending.tmdb_id as u32)).await;
        let _ = db::delete_pending_download(&pool, pending.id).await;
        return res;
    }

    info!("No pending match for {}. Running AI ingestion.", filename);
    let metadata = match ollama.parse_scene_release(&filename).await {
        Ok(json) => Parser::parse_llm_json(&json).unwrap_or_else(|_| Parser::parse_regex(&filename)),
        Err(_) => Parser::parse_regex(&filename),
    };
    let id = db::insert_media_item(&pool, &filename, &metadata).await?;
    run_pipeline(id, path, pool, tmdb, ollama, None).await
}

pub async fn run_pipeline(item_id: i64, path: PathBuf, pool: sqlx::SqlitePool, tmdb: TmdbClient, ollama: Arc<OllamaClient>, manual_id: Option<u32>) -> Result<()> {
    let item = db::get_item_by_id(&pool, item_id).await?.context("not found")?;
    let mut tid = manual_id;
    if tid.is_none() {
        if let Ok(Some((h_id, _, _))) = db::get_manual_match(&pool, &item.title).await { tid = Some(h_id); }
        else if let Ok(tracked) = db::get_tracked_shows(&pool).await {
            for s in tracked {
                if s.title.to_lowercase() == item.title.to_lowercase() { tid = Some(s.tmdb_id as u32); break; }
            }
        }
    }

    let media = if let Some(id) = tid {
        if item.season.is_some() { tmdb.get_tv_details(id).await.ok() }
        else { tmdb.get_movie_details(id).await.ok() }
    } else {
        let res = if item.season.is_some() { tmdb.search_tv(&item.title).await? } else { tmdb.search_movie(&item.title).await? };
        if let Some(m) = res.first() {
            if item.season.is_some() { tmdb.get_tv_details(m.id).await.ok() }
            else { tmdb.get_movie_details(m.id).await.ok() }
        } else { None }
    };

    if let Some(m) = media {
        if let Some(ov) = &m.overview {
            let sf = ollama.rewrite_summary(ov).await.unwrap_or_else(|_| ov.to_string());
            let _ = db::update_summaries(&pool, item_id, m.id, ov, &sf).await;
            
            let format_movie = db::get_setting(&pool, "rename_format_movie").await?.unwrap_or("{title} ({year})".to_string());
            let format_tv = db::get_setting(&pool, "rename_format_tv").await?.unwrap_or("{title} - S{season}E{episode}".to_string());
            let library_dir = env::var("NEURARR_LIBRARY_DIR").unwrap_or_else(|_| "./library".to_string());
            
            let quality = if path.to_string_lossy().contains("2160p") { "2160p" } else { "1080p" };
            let year = m.release_date.as_deref().unwrap_or("Unknown").split('-').next().unwrap_or("Unknown");
            
            let mut final_path = PathBuf::from(library_dir);
            let new_name = if item.season.is_some() {
                let name = Renamer::format_tv(&format_tv, &item.title, item.season.unwrap(), item.episode.unwrap_or(0), quality);
                final_path.push("TV");
                name
            } else {
                let name = Renamer::format_movie(&format_movie, &item.title, year, quality);
                final_path.push("Movies");
                name
            };

            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("mkv");
            let dest_file = final_path.join(format!("{}.{}", new_name, ext));
            let nfo_file = final_path.join(format!("{}.nfo", new_name));

            info!("Attempting to move file from {:?} to {:?}", path, dest_file);
            let _ = tokio::fs::create_dir_all(dest_file.parent().unwrap()).await;
            if path.exists() {
                if let Err(e) = tokio::fs::rename(&path, &dest_file).await {
                    info!("Rename failed (likely cross-device), trying copy: {}", e);
                    if let Err(e) = tokio::fs::copy(&path, &dest_file).await {
                        error!("Failed to copy file: {}", e);
                    } else {
                        let _ = tokio::fs::remove_file(&path).await;
                        info!("Copy successful, original removed.");
                    }
                } else {
                    info!("Rename successful.");
                }
                send_notification("NeurArr", &format!("Imported: {}", item.title));
            } else {
                error!("Source path does not exist for move: {:?}", path);
            }

            let nfo_content = format!("<movie><title>{}</title><plot>{}</plot></movie>", m.title.as_deref().unwrap_or(&item.title), sf);
            let _ = tokio::fs::write(&nfo_file, nfo_content).await;

            // Fetch Subtitles
            if let Ok(sub_client) = crate::integrations::subtitles::SubtitleClient::new() {
                let _ = sub_client.download_subtitles(&item.original_filename, &dest_file).await;
            }

            sqlx::query("UPDATE media_items SET status = 'completed', resolution = ? WHERE id = ?").bind(quality).bind(item_id).execute(&pool).await?;
            
            // Update the tracked show or episode status and resolution
            if item.season.is_some() {
                sqlx::query("UPDATE episodes SET status = 'completed', resolution = ? WHERE show_id = (SELECT id FROM tracked_shows WHERE tmdb_id = ?) AND season = ? AND episode = ?")
                    .bind(quality)
                    .bind(m.id as i64)
                    .bind(item.season)
                    .bind(item.episode)
                    .execute(&pool).await?;
            } else {
                sqlx::query("UPDATE tracked_shows SET status = 'completed', resolution = ? WHERE tmdb_id = ?")
                    .bind(quality)
                    .bind(m.id as i64)
                    .execute(&pool).await?;
            }

            if let Ok(plex) = integrations::plex::PlexClient::new() { let _ = plex.refresh_library().await; }
        }
    }
    Ok(())
}
