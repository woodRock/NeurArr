mod db;
mod integrations;
mod llm;
mod parser;
mod scanner;
mod web;

use crate::scanner::Scanner;
use crate::db::init_db;
use crate::integrations::tmdb::TmdbClient;
use crate::llm::OllamaClient;
use crate::parser::Parser;
use crate::web::start_web_server;

use anyhow::{Context, Result};
use clap::{Parser as ClapParser, Subcommand};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
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

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry().with(fmt::layer()).with(filter).init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Setup) => {
            info!("Run: ollama run qwen3.5:0.8b");
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
            run_daemon().await?;
        }
    }

    Ok(())
}

pub async fn scan_library(pool: sqlx::SqlitePool) -> Result<()> {
    let library_dir = env::var("NEURARR_LIBRARY_DIR").unwrap_or_else(|_| "./library".to_string());
    info!("Starting full library scan in: {}", library_dir);
    for entry in WalkDir::new(&library_dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let filename = entry.file_name().to_string_lossy().to_string();
            if filename.ends_with(".mkv") || filename.ends_with(".mp4") || filename.ends_with(".avi") {
                let metadata = Parser::parse_regex(&filename);
                let _ = db::insert_media_item(&pool, &filename, &metadata).await;
            }
        }
    }
    Ok(())
}

async fn update_app() -> Result<()> {
    info!("Starting NeurArr update process...");

    // 1. Git Pull
    info!("Pulling latest changes from GitHub...");
    let status = std::process::Command::new("git")
        .arg("pull")
        .status()
        .context("Failed to execute git pull.")?;

    if !status.success() {
        anyhow::bail!("Git pull failed.");
    }

    // 2. Handle Windows Binary Lock
    #[cfg(windows)]
    {
        let exe_path = std::env::current_exe()?;
        let old_exe = exe_path.with_extension("old");
        if old_exe.exists() {
            let _ = std::fs::remove_file(&old_exe);
        }
        info!("Renaming current executable to bypass Windows file lock...");
        std::fs::rename(&exe_path, &old_exe).context("Failed to rename running executable.")?;
    }

    // 3. Cargo Build
    info!("Building the latest version...");
    let status = std::process::Command::new("cargo")
        .args(["build", "--release"])
        .status()
        .context("Failed to execute cargo build.")?;

    if !status.success() {
        // Rollback on Windows if build fails
        #[cfg(windows)]
        {
            let exe_path = std::env::current_exe()?;
            let old_exe = exe_path.with_extension("old");
            let _ = std::fs::rename(&old_exe, &exe_path);
        }
        anyhow::bail!("Build failed. Please check the logs.");
    }

    info!("Update successful!");

    // 4. Restart
    #[cfg(unix)]
    {
        info!("Restarting NeurArr...");
        use std::os::unix::process::CommandExt;
        let mut args = std::env::args();
        let cmd = args.next().unwrap();
        let _ = std::process::Command::new(cmd).args(args).exec();
    }

    #[cfg(windows)]
    {
        info!("New version built! Please close this window and restart NeurArr to apply changes.");
        std::process::exit(0);
    }
    Ok(())
}

async fn run_daemon() -> Result<()> {
    info!("NeurArr Pro starting up...");
    let pool = init_db().await?;
    let tmdb_client = TmdbClient::new()?;
    let ollama = Arc::new(OllamaClient::new()?);

    let watch_path = env::var("NEURARR_INGEST_DIR").unwrap_or_else(|_| "ingest".to_string());
    if !std::path::Path::new(&watch_path).exists() { std::fs::create_dir_all(&watch_path)?; }

    let mut scanner = Scanner::new()?;
    scanner.watch(PathBuf::from(&watch_path))?;

    let processing_registry = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let ai_semaphore = Arc::new(Semaphore::new(1));

    let scanner_pool = pool.clone();
    let scanner_tmdb = tmdb_client.clone();
    let scanner_ollama = ollama.clone();
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
                                let mut registry = processing_registry.lock().await;
                                if registry.contains(&path) { continue; }
                                registry.insert(path.clone());
                                drop(registry);

                                let pool = scanner_pool.clone();
                                let tmdb = scanner_tmdb.clone();
                                let ollama = scanner_ollama.clone();
                                let registry = processing_registry.clone();
                                let sem = ai_semaphore.clone();
                                tokio::spawn(async move {
                                    let _permit = sem.acquire().await.ok();
                                    let _ = process_file(path.clone(), pool, tmdb, ollama).await;
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

    let web_handle = start_web_server(pool.clone());

    let scheduler_pool = pool.clone();
    let scheduler_tmdb = tmdb_client.clone();
    let scheduler_ollama = ollama.clone();
    let scheduler_handle = tokio::spawn(async move {
        let indexer = crate::integrations::indexer::IndexerClient::new().unwrap();
        let qbit = crate::integrations::torrent::QBittorrentClient::new().unwrap();
        let _ = qbit.login().await;
        loop {
            let profile = db::get_default_quality_profile(&scheduler_pool).await.ok();
            if let Ok(tracked) = db::get_tracked_shows(&scheduler_pool).await {
                for show in tracked {
                    if show.media_type == "tv" {
                        if let Ok(full) = scheduler_tmdb.get_tv_details(show.tmdb_id as u32).await {
                            let seasons = full.number_of_seasons.unwrap_or(1);
                            for s in 1..=seasons {
                                if let Ok(eps) = scheduler_tmdb.get_tv_season(show.tmdb_id as u32, s).await {
                                    for ep in eps {
                                        let aired = ep.air_date.as_ref().map(|d| d <= &chrono::Utc::now().date_naive().to_string()).unwrap_or(false);
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
                    for q in queries {
                        if found { break; }
                        if let Ok(res) = indexer.search(&q).await {
                            let filtered: Vec<_> = res.into_iter().filter(|r| {
                                if let Some(p) = &profile {
                                    let t = r.title.to_lowercase();
                                    if let Some(must) = &p.must_contain { if !must.is_empty() && !t.contains(must) { return false; } }
                                    if let Some(not) = &p.must_not_contain { for w in not.split(',') { if !w.is_empty() && t.contains(w.trim()) { return false; } } }
                                    if p.max_resolution == "1080p" && t.contains("2160p") { return false; }
                                }
                                true
                            }).collect();
                            for best in filtered {
                                if let Ok(true) = scheduler_ollama.verify_torrent_match(&show.title, &best.title).await {
                                    let ingest = std::fs::canonicalize("./ingest").unwrap_or_else(|_| PathBuf::from("./ingest"));
                                    if qbit.add_torrent_url(&best.link, Some(&ingest.to_string_lossy())).await.is_ok() {
                                        let _ = db::update_episode_status(&scheduler_pool, ep.id, "downloading").await;
                                        found = true; break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });

    tokio::select! {
        res = scanner_handle => res?,
        res = web_handle => res?,
        _ = scheduler_handle => {},
    }
    Ok(())
}

async fn process_file(path: PathBuf, pool: sqlx::SqlitePool, tmdb: TmdbClient, ollama: Arc<OllamaClient>) -> Result<()> {
    let filename = path.file_name().unwrap().to_string_lossy().to_string();
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
        else {
            let res = tmdb.search_movie(&item.title).await?;
            res.first().map(|m| integrations::tmdb::TmdbMediaFull { 
                id: m.id, 
                name: m.name.clone(), 
                title: m.title.clone(), 
                overview: m.overview.clone(), 
                number_of_seasons: None,
                release_date: m.release_date.clone(),
                first_air_date: m.first_air_date.clone(),
                poster_path: m.poster_path.clone(),
            })
        }
    } else {
        let res = if item.season.is_some() { tmdb.search_tv(&item.title).await? } else { tmdb.search_movie(&item.title).await? };
        res.first().map(|m| integrations::tmdb::TmdbMediaFull { 
            id: m.id, 
            name: m.name.clone(), 
            title: m.title.clone(), 
            overview: m.overview.clone(), 
            number_of_seasons: None,
            release_date: m.release_date.clone(),
            first_air_date: m.first_air_date.clone(),
            poster_path: m.poster_path.clone(),
        })
    };

    if let Some(m) = media {
        if let Some(ov) = &m.overview {
            let sf = ollama.rewrite_summary(ov).await.unwrap_or_else(|_| ov.to_string());
            let _ = db::update_summaries(&pool, item_id, m.id, ov, &sf).await;
            let lib = env::var("NEURARR_LIBRARY_DIR").unwrap_or_else(|_| "./library".to_string());
            let mut dest = PathBuf::from(lib);
            if item.season.is_some() { dest.push("TV"); dest.push(&item.title); dest.push(format!("Season {}", item.season.unwrap())); }
            else { dest.push("Movies"); dest.push(&item.title); }
            let _ = tokio::fs::create_dir_all(&dest).await;
            let dest_file = dest.join(&item.original_filename);
            let _ = tokio::fs::rename(&path, &dest_file).await;
            sqlx::query("UPDATE media_items SET status = 'completed' WHERE id = ?").bind(item_id).execute(&pool).await?;
            if let Ok(plex) = integrations::plex::PlexClient::new() { let _ = plex.refresh_library().await; }
        }
    }
    Ok(())
}
