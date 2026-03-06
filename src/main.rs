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
use tracing::{error, info};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[derive(ClapParser)]
#[command(name = "neurarr")]
#[command(about = "Privacy-first AI media management daemon", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initial setup (Ollama setup instructions)
    Setup,
    /// Run the daemon (default)
    Run,
    /// Update NeurArr to the latest version from GitHub
    Update,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file
    dotenvy::dotenv().ok();

    // Initialize tracing
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Setup) => {
            info!("NeurArr now uses Ollama for high-speed AI inference!");
            info!("1. Download Ollama from https://ollama.com");
            info!("2. Run: ollama run qwen3.5:0.8b");
            info!("3. Start NeurArr: cargo run");
            return Ok(());
        }
        Some(Commands::Update) => {
            update_app().await?;
            return Ok(());
        }
        _ => {
            run_daemon().await?;
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
        .context("Failed to execute git pull. Is git installed?")?;

    if !status.success() {
        anyhow::bail!("Git pull failed. Please check your internet connection.");
    }

    info!("Building the latest version (this may take a minute)...");
    let status = std::process::Command::new("cargo")
        .args(["build", "--release"])
        .status()
        .context("Failed to execute cargo build.")?;

    if !status.success() {
        anyhow::bail!("Build failed. Please check the logs above.");
    }

    info!("Update successful! Restarting NeurArr...");

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut args = std::env::args();
        let cmd = args.next().unwrap();
        let err = std::process::Command::new(cmd)
            .args(args)
            .exec();
        return Err(anyhow::anyhow!("Failed to restart: {}", err));
    }

    #[cfg(windows)]
    {
        info!("On Windows, please manually restart the application to use the new version.");
        std::process::exit(0);
    }
}

async fn run_daemon() -> Result<()> {
    info!("NeurArr starting up with Ollama backend...");

    let pool = init_db().await?;
    info!("Database initialized");

    let tmdb_client = TmdbClient::new()?;
    info!("TMDB client initialized");

    let ollama = Arc::new(OllamaClient::new()?);
    info!("Ollama client ready");

    let watch_path = env::var("NEURARR_INGEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let mut path = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            path.push("ingest");
            path
        });

    if !watch_path.exists() {
        std::fs::create_dir_all(&watch_path)?;
    }

    let mut scanner = Scanner::new()?;
    scanner.watch(watch_path.clone())?;

    info!("Monitoring directory: {:?}", watch_path);

    let processing_registry = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let ai_semaphore = Arc::new(Semaphore::new(1));

    let scanner_pool = pool.clone();
    let scanner_tmdb = tmdb_client.clone();
    let scanner_ollama = ollama.clone();
    
    let scanner_handle = async move {
        let mut scanner = scanner;
        while let Some(event_res) = scanner.next_event().await {
            match event_res {
                Ok(event) => {
                    use notify::event::EventKind;
                    match event.kind {
                        EventKind::Create(_) | EventKind::Modify(_) => {
                            for path in event.paths {
                                if path.is_file() {
                                    if let Some(name) = path.file_name() {
                                        let name_str = name.to_string_lossy();
                                        if name_str.starts_with('.') || name_str.ends_with(".part") || name_str.ends_with(".tmp") {
                                            continue;
                                        }
                                    }
                                    
                                    let mut registry = processing_registry.lock().await;
                                    if registry.contains(&path) {
                                        continue;
                                    }
                                    registry.insert(path.clone());
                                    drop(registry);

                                    let pool = scanner_pool.clone();
                                    let tmdb = scanner_tmdb.clone();
                                    let ollama_clone = scanner_ollama.clone();
                                    let registry_clone = processing_registry.clone();
                                    let semaphore = ai_semaphore.clone();
                                    
                                    tokio::spawn(async move {
                                        let path_clone = path.clone();
                                        let _permit = semaphore.acquire().await.ok();
                                        
                                        info!("Processing task started for {:?}", path_clone.file_name().unwrap_or_default());
                                        if let Err(e) = process_file(path, pool, tmdb, ollama_clone).await {
                                            error!("Failed to process file: {}", e);
                                        }
                                        
                                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                        registry_clone.lock().await.remove(&path_clone);
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    error!("Watcher error: {}", e);
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    let web_handle = start_web_server(pool.clone());

    let scheduler_pool = pool.clone();
    let scheduler_handle = tokio::spawn(async move {
        let indexer = crate::integrations::indexer::IndexerClient::new().unwrap();
        let qbit = crate::integrations::torrent::QBittorrentClient::new().unwrap();
        let tmdb = crate::integrations::tmdb::TmdbClient::new().unwrap();
        let _ = qbit.login().await;
        
        loop {
            info!("Running scheduled background checks...");
            
            // 1. Sync episodes for tracked TV shows
            if let Ok(tracked) = crate::db::get_tracked_shows(&scheduler_pool).await {
                for show in tracked {
                    if show.media_type == "tv" {
                        info!("Syncing episodes for show: {}", show.title);
                        if let Ok(episodes) = tmdb.get_tv_season(show.tmdb_id as u32, 1).await {
                            for ep in episodes {
                                let already_aired = ep.air_date.as_ref().map(|d| {
                                    let now = chrono::Utc::now().date_naive().to_string();
                                    d <= &now
                                }).unwrap_or(false);

                                if already_aired {
                                    let _ = crate::db::insert_episode(
                                        &scheduler_pool, 
                                        show.id, 
                                        ep.season_number, 
                                        ep.episode_number, 
                                        &ep.name, 
                                        ep.air_date,
                                        "wanted"
                                    ).await;
                                }
                            }
                        }
                    }
                }
            }

            // 2. Search for wanted episodes (TV)
            if let Ok(wanted_eps) = crate::db::get_wanted_episodes(&scheduler_pool).await {
                for (ep, show) in wanted_eps {
                    let ep_code = format!("S{:02}E{:02}", ep.season, ep.episode);
                    let mut search_queries = Vec::new();
                    search_queries.push(format!("{} {}", show.title, ep_code));
                    
                    if let Ok(alts) = tmdb.get_alternative_titles(show.tmdb_id as u32, true).await {
                        for alt in alts {
                            search_queries.push(format!("{} {}", alt, ep_code));
                        }
                    }

                    for query in search_queries {
                        info!("Searching for episode: {}", query);
                        if let Ok(results) = indexer.search(&query).await {
                            if let Some(best) = results.first() {
                                info!("Found torrent for {}: {}", query, best.title);
                                let ingest_abs = std::fs::canonicalize("./ingest").unwrap_or_else(|_| PathBuf::from("./ingest"));
                                if let Ok(_) = qbit.add_torrent_url(&best.link, Some(&ingest_abs.to_string_lossy())).await {
                                    let _ = crate::db::update_episode_status(&scheduler_pool, ep.id, "downloading").await;
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            // 3. Search for wanted movies
            if let Ok(wanted_movies) = crate::db::get_wanted_shows(&scheduler_pool).await {
                for show in wanted_movies {
                    if show.media_type != "movie" { continue; }
                    let mut search_queries = Vec::new();
                    if let Some(y) = show.year {
                        search_queries.push(format!("{} ({})", show.title, y));
                    }
                    search_queries.push(show.title.clone());

                    if let Ok(alts) = tmdb.get_alternative_titles(show.tmdb_id as u32, false).await {
                        for alt in alts { search_queries.push(alt); }
                    }

                    for query in search_queries {
                        if let Ok(results) = indexer.search(&query).await {
                            if let Some(best) = results.first() {
                                let ingest_abs = std::fs::canonicalize("./ingest").unwrap_or_else(|_| PathBuf::from("./ingest"));
                                if let Ok(_) = qbit.add_torrent_url(&best.link, Some(&ingest_abs.to_string_lossy())).await {
                                    let _ = crate::db::update_tracked_show_status(&scheduler_pool, show.id, "downloading").await;
                                    break;
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

async fn process_file(
    path: PathBuf,
    pool: sqlx::SqlitePool,
    tmdb: TmdbClient,
    ollama: Arc<OllamaClient>,
) -> Result<()> {
    let filename = path.file_name().unwrap().to_string_lossy();
    info!("New file detected: {}", filename);

    info!("Parsing filename...");
    let metadata = match ollama.parse_scene_release(&filename).await {
        Ok(llm_json) => {
            match Parser::parse_llm_json(&llm_json) {
                Ok(m) => m,
                Err(e) => {
                    error!("LLM parsing failed, falling back to regex: {}", e);
                    Parser::parse_regex(&filename)
                }
            }
        }
        Err(e) => {
            error!("Ollama connection failed, falling back to regex: {}", e);
            Parser::parse_regex(&filename)
        }
    };

    info!("Parsed metadata: {:?}", metadata);

    let item_id = db::insert_media_item(&pool, &filename, &metadata).await?;

    run_pipeline(item_id, path, pool, tmdb, ollama, None).await
}

pub async fn run_pipeline(
    item_id: i64,
    path: PathBuf,
    pool: sqlx::SqlitePool,
    tmdb: TmdbClient,
    ollama: Arc<OllamaClient>,
    manual_tmdb_id: Option<u32>,
) -> Result<()> {
    let item = db::get_item_by_id(&pool, item_id).await?.context("Item not found")?;
    
    let mut final_tid = manual_tmdb_id;
    if final_tid.is_none() {
        if let Ok(Some((tid, _, poster))) = db::get_manual_match(&pool, &item.title).await {
            info!("Pipeline: Found manual match history for '{}', auto-matching to TMDB: {}", item.title, tid);
            final_tid = Some(tid);
            if let Some(p) = poster {
                let _ = db::update_item_poster(&pool, item_id, &p).await;
            }
        }
        
        if final_tid.is_none() {
            if let Ok(tracked) = db::get_tracked_shows(&pool).await {
                for show in tracked {
                    if show.title.to_lowercase() == item.title.to_lowercase() {
                        final_tid = Some(show.tmdb_id as u32);
                        break;
                    }
                    if let Ok(alts) = tmdb.get_alternative_titles(show.tmdb_id as u32, show.media_type == "tv").await {
                        if alts.iter().any(|a| a.to_lowercase() == item.title.to_lowercase()) {
                            final_tid = Some(show.tmdb_id as u32);
                            break;
                        }
                    }
                }
            }
        }
    }

    let media = if let Some(tid) = final_tid {
        if item.season.is_some() {
            tmdb.get_tv_details(tid).await.ok()
        } else {
            tmdb.get_movie_details(tid).await.ok()
        }
    } else {
        let results = if item.season.is_some() {
            tmdb.search_tv(&item.title).await?
        } else {
            tmdb.search_movie(&item.title).await?
        };
        results.first().cloned()
    };

    if let Some(media) = media {
        info!("Pipeline: Found TMDB match: {:?}", media.title.as_ref().or(media.name.as_ref()));
        
        if let Some(poster) = &media.poster_path {
            let _ = db::update_item_poster(&pool, item_id, poster).await;
        }

        if let Some(overview) = &media.overview {
            info!("Pipeline: Rewriting summary...");
            let spoiler_free = ollama.rewrite_summary(overview).await.unwrap_or_else(|_| overview.to_string());

            db::update_summaries(&pool, item_id, media.id, overview, &spoiler_free).await?;
            
            let library_dir = env::var("NEURARR_LIBRARY_DIR").unwrap_or_else(|_| "./library".to_string());
            let clean_title = item.title.replace(|c: char| !c.is_alphanumeric() && c != ' ', "").trim().to_string();
            let year = media.release_date.as_deref().unwrap_or("").split('-').next().unwrap_or("Unknown");
            let folder_name = format!("{} ({})", clean_title, year);
            let mut dest_dir = PathBuf::from(&library_dir);
            
            if item.season.is_some() {
                dest_dir.push("TV");
                dest_dir.push(&folder_name);
                dest_dir.push(format!("Season {}", item.season.unwrap()));
            } else {
                dest_dir.push("Movies");
                dest_dir.push(&folder_name);
            }

            if !dest_dir.exists() {
                tokio::fs::create_dir_all(&dest_dir).await?;
            }

            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("mkv");
            let mut new_filename = clean_title.clone();
            if let Some(s) = item.season {
                new_filename.push_str(&format!(" S{:02}", s));
                if let Some(e) = item.episode {
                    new_filename.push_str(&format!("E{:02}", e));
                }
            } else {
                new_filename.push_str(&format!(" ({})", year));
            }
            
            // Re-detect resolution for renaming if lost
            let res = if path.to_string_lossy().contains("2160p") { " [2160p]" } 
                     else if path.to_string_lossy().contains("1080p") { " [1080p]" } 
                     else { "" };
            new_filename.push_str(res);
            
            let dest_file = dest_dir.join(format!("{}.{}", new_filename, ext));
            let nfo_file = dest_dir.join(format!("{}.nfo", new_filename));

            if path.exists() {
                info!("Pipeline: Moving file to {:?}", dest_file);
                if let Err(e) = tokio::fs::rename(&path, &dest_file).await {
                    error!("Pipeline: Failed to move file: {}", e);
                }
            }

            let nfo_content = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\" ?>\n<movie>\n  <title>{}</title>\n  <plot>{}</plot>\n</movie>",
                media.title.as_deref().unwrap_or(&item.title),
                spoiler_free
            );
            let _ = tokio::fs::write(&nfo_file, nfo_content).await;

            sqlx::query("UPDATE media_items SET status = 'completed' WHERE id = ?").bind(item_id).execute(&pool).await?;

            if let Ok(plex) = crate::integrations::plex::PlexClient::new() {
                let _ = plex.refresh_library().await;
            }
        }
    }
    Ok(())
}
