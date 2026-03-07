use anyhow::{Context, Result};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{error, info};

pub struct Scanner {
    watcher: RecommendedWatcher,
    event_rx: mpsc::Receiver<Result<Event, notify::Error>>,
}

impl Scanner {
    pub fn new() -> Result<Self> {
        let (event_tx, event_rx) = mpsc::channel(100);

        let watcher = RecommendedWatcher::new(
            move |res| {
                if let Err(e) = event_tx.blocking_send(res) {
                    error!("failed to send watcher event: {}", e);
                }
            },
            Config::default(),
        ).context("Failed to initialize notify watcher")?;

        Ok(Self { watcher, event_rx })
    }

    pub fn watch(&mut self, path: PathBuf) -> Result<()> {
        info!("Starting to watch directory: {:?}", path);
        self.watcher
            .watch(&path, RecursiveMode::Recursive)
            .context(format!("Failed to watch directory: {:?}", path))?;
        Ok(())
    }

    pub async fn next_event(&mut self) -> Option<Result<Event, notify::Error>> {
        self.event_rx.recv().await
    }

    pub async fn scan(&mut self, pool: sqlx::SqlitePool, tmdb: crate::integrations::tmdb::TmdbClient, ollama: std::sync::Arc<crate::llm::OllamaClient>, qbit: std::sync::Arc<crate::integrations::torrent::QBittorrentClient>, plex: std::sync::Arc<crate::integrations::plex::PlexClient>, path: PathBuf) -> Result<()> {
        info!("Scanning ingest directory: {:?}", path);
        for entry in walkdir::WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file() {
                let _ = crate::process_file(entry.path().to_path_buf(), pool.clone(), tmdb.clone(), ollama.clone(), qbit.clone(), plex.clone()).await;
            }
        }
        Ok(())
    }
}
