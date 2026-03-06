use anyhow::Result;
use axum::{
    extract::{State, Query, Path},
    response::Html,
    routing::{get, post, delete},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::net::SocketAddr;
use tracing::info;
use crate::integrations::tmdb::TmdbClient;
use crate::integrations::torrent::TorrentInfo;
use crate::db::{self, TrackedShow, QualityProfile};
use sysinfo::{System, Disks};
use std::sync::{Mutex as StdMutex, Arc};

#[derive(Serialize, Deserialize, sqlx::FromRow, Clone)]
pub struct MediaItem {
    pub id: i64,
    pub original_filename: String,
    pub title: String,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub status: String,
    pub spoiler_free_summary: Option<String>,
    pub poster_path: Option<String>,
}

#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
    tmdb: TmdbClient,
    ollama: Arc<crate::llm::OllamaClient>,
    sys: Arc<StdMutex<System>>,
}

pub async fn start_web_server(pool: SqlitePool) -> Result<()> {
    let tmdb = TmdbClient::new()?;
    let ollama = Arc::new(crate::llm::OllamaClient::new()?);
    let mut sys = System::new_all();
    sys.refresh_all();
    let state = AppState { 
        pool, 
        tmdb, 
        ollama,
        sys: Arc::new(StdMutex::new(sys)) 
    };

    let app = Router::new()
        .route("/", get(dashboard))
        .route("/api/media", get(get_media))
        .route("/api/media/clear", delete(clear_queue))
        .route("/api/media/{id}/match", post(match_media))
        .route("/api/search", get(search_media))
        .route("/api/upcoming", get(get_upcoming))
        .route("/api/track", post(track_show))
        .route("/api/tracked", get(get_tracked))
        .route("/api/tracked/{id}", delete(delete_tracked))
        .route("/api/tracked/{id}/episodes", get(get_episodes))
        .route("/api/episodes/{id}/status", post(set_episode_status))
        .route("/api/episodes/{id}/search", post(manual_search_episode))
        .route("/api/torrents", get(get_torrents))
        .route("/api/sysinfo", get(get_sysinfo))
        .route("/api/disks", get(get_disks))
        .route("/api/quality-profiles", get(get_profiles))
        .route("/api/update", post(trigger_update))
        .route("/api/scan-library", post(scan_library))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    info!("Dashboard available at http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn dashboard() -> Html<&'static str> {
    Html(r#"
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8"><title>NeurArr Pro</title>
    <script src="https://cdn.tailwindcss.com"></script>
    <style>
        body { background-color: #020617; color: #f8fafc; font-family: sans-serif; }
        .glass { background: rgba(30, 41, 59, 0.7); backdrop-filter: blur(12px); border: 1px solid rgba(255,255,255,0.1); }
        .active { color: #38bdf8 !important; border-bottom: 2px solid #38bdf8; }
        .hidden { display: none; }
        .card-content { word-wrap: break-word; overflow-wrap: break-word; word-break: break-word; }
        .modal { display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.8); z-index: 100; align-items: center; justify-content: center; }
        .modal.active { display: flex; }
    </style>
</head>
<body class="p-4">
    <nav class="glass sticky top-0 flex gap-4 p-4 mb-4 rounded-xl items-center">
        <div class="font-bold text-xl mr-4">NeurArr</div>
        <button onclick="showTab('queue')" id="nav-queue" class="active">QUEUE</button>
        <button onclick="showTab('tracked')" id="nav-tracked">COLLECTION</button>
        <button onclick="showTab('upcoming')" id="nav-upcoming">DISCOVER</button>
        <button onclick="showTab('downloads')" id="nav-downloads">DOWNLOADS</button>
        <button onclick="showTab('settings')" id="nav-settings">SETTINGS</button>
        <div class="ml-auto text-xs text-slate-400 flex gap-4">
            <span id="sys-cpu">CPU: 0%</span><span id="sys-ram">RAM: 0MB</span>
        </div>
    </nav>

    <div id="tab-queue" class="grid grid-cols-1 md:grid-cols-3 gap-4"></div>
    <div id="tab-tracked" class="hidden grid grid-cols-2 md:grid-cols-5 gap-4"></div>
    <div id="tab-upcoming" class="hidden grid grid-cols-2 md:grid-cols-5 gap-4"></div>
    <div id="tab-downloads" class="hidden space-y-2"></div>
    <div id="tab-search" class="hidden grid grid-cols-2 md:grid-cols-5 gap-4"></div>
    <div id="tab-settings" class="hidden glass p-8 rounded-xl">
        <h2 class="font-bold mb-4">System</h2>
        <div id="disk-info" class="text-sm"></div>
        <button onclick="scanLibrary()" class="bg-sky-600 px-4 py-2 rounded mt-4 text-sm font-bold">SCAN LIBRARY</button>
    </div>

    <div id="match-modal" class="modal"><div class="glass p-8 rounded-2xl w-full max-w-4xl max-h-[80vh] overflow-y-auto">
        <button onclick="closeModal()" class="mb-4">Close</button>
        <div id="modal-results" class="grid grid-cols-3 gap-4"></div>
    </div></div>

    <div id="episodes-modal" class="modal"><div class="glass p-8 rounded-2xl w-full max-w-4xl max-h-[80vh] overflow-y-auto">
        <h2 id="episode-modal-title" class="font-bold mb-4"></h2>
        <button onclick="closeEpisodeModal()" class="mb-4">Close</button>
        <div id="episodes-list" class="space-y-2"></div>
    </div></div>

    <script>
        let currentMatchId = null;
        function showTab(tab) {
            ['queue', 'tracked', 'upcoming', 'downloads', 'settings', 'search'].forEach(t => {
                document.getElementById('tab-' + t)?.classList.toggle('hidden', t !== tab);
                document.getElementById('nav-' + t)?.classList.toggle('active', t === tab);
            });
            if(tab === 'queue') fetchQueue(); if(tab === 'tracked') fetchTracked(); if(tab === 'upcoming') fetchUpcoming(); if(tab === 'downloads') fetchTorrents(); if(tab === 'settings') fetchDisks();
        }
        async function fetchQueue() {
            const res = await fetch('/api/media'); const data = await res.json();
            document.getElementById('tab-queue').innerHTML = data.map(item => `
                <div class="glass p-4 rounded-xl card-content">
                    <img src="https://image.tmdb.org/t/p/w200${item.poster_path}" class="w-16 h-24 float-left mr-4 rounded">
                    <div class="font-bold truncate text-sm">${item.title}</div>
                    <div class="text-[10px] text-slate-500">${item.status}</div>
                    <button onclick="openMatchModal(${item.id}, '${item.title}')" class="text-[10px] text-sky-400 font-bold mt-2">MATCH</button>
                </div>
            `).join('');
        }
        async function fetchTracked() {
            const res = await fetch('/api/tracked'); const data = await res.json();
            document.getElementById('tab-tracked').innerHTML = data.map(item => `
                <div class="glass rounded-xl overflow-hidden">
                    <img src="https://image.tmdb.org/t/p/w500${item.poster_path}" class="h-64 w-full object-cover">
                    <div class="p-3">
                        <div class="font-bold text-sm truncate">${item.title}</div>
                        <button onclick="openEpisodeModal(${item.id}, '${item.title}')" class="text-xs text-sky-400">EPISODES</button>
                    </div>
                </div>
            `).join('');
        }
        async function fetchTorrents() {
            const res = await fetch('/api/torrents'); const data = await res.json();
            document.getElementById('tab-downloads').innerHTML = data.map(t => `
                <div class="glass p-4 rounded-xl">
                    <div class="flex justify-between text-xs"><span>${t.name}</span><span>${(t.progress*100).toFixed(1)}%</span></div>
                    <div class="w-full h-1 bg-slate-800 mt-2"><div class="bg-sky-500 h-full" style="width:${t.progress*100}%"></div></div>
                </div>
            `).join('');
        }
        async function fetchSysInfo() {
            const res = await fetch('/api/sysinfo'); const data = await res.json();
            document.getElementById('sys-cpu').innerText = `CPU: ${data.cpu_usage.toFixed(1)}%`;
            document.getElementById('sys-ram').innerText = `RAM: ${data.memory_used}MB`;
        }
        async function fetchDisks() {
            const res = await fetch('/api/disks'); const data = await res.json();
            document.getElementById('disk-info').innerHTML = data.map(d => `<div>${d.name}: ${d.available}GB / ${d.total}GB</div>`).join('');
        }
        async function openMatchModal(id, title) { currentMatchId = id; document.getElementById('match-modal').classList.add('active'); }
        function closeModal() { document.getElementById('match-modal').classList.remove('active'); }
        async function openEpisodeModal(id, title) { document.getElementById('episode-modal-title').innerText = title; document.getElementById('episodes-modal').classList.add('active'); fetchEpisodes(id); }
        function closeEpisodeModal() { document.getElementById('episodes-modal').classList.remove('active'); }
        async function fetchEpisodes(id) {
            const res = await fetch(`/api/tracked/${id}/episodes`); const data = await res.json();
            document.getElementById('episodes-list').innerHTML = data.map(ep => `<div class="text-xs p-2 bg-slate-900 rounded flex justify-between"><span>S${ep.season}E${ep.episode} - ${ep.title}</span><span class="text-slate-500">${ep.status}</span></div>`).join('');
        }
        async function scanLibrary() { await fetch('/api/scan-library', {method:'POST'}); alert('Scanning...'); }
        showTab('queue'); setInterval(fetchSysInfo, 3000);
    </script>
</body>
</html>
"#)
}

async fn get_media(State(state): State<AppState>) -> Json<Vec<MediaItem>> {
    let items = sqlx::query_as::<_, MediaItem>("SELECT * FROM media_items ORDER BY processed_at DESC").fetch_all(&state.pool).await.unwrap_or_default();
    Json(items)
}

async fn clear_queue(State(state): State<AppState>) -> Json<bool> {
    let _ = db::clear_media_queue(&state.pool).await;
    Json(true)
}

async fn search_media(State(state): State<AppState>, Query(params): Query<SearchQuery>) -> Json<Vec<crate::integrations::tmdb::TmdbMedia>> {
    let mut movies = state.tmdb.search_movie(&params.q).await.unwrap_or_default();
    let mut tv = state.tmdb.search_tv(&params.q).await.unwrap_or_default();
    for m in &mut movies { m.media_type = Some("movie".to_string()); }
    for t in &mut tv { t.media_type = Some("tv".to_string()); }
    movies.append(&mut tv);
    Json(movies)
}

#[derive(Deserialize)]
struct SearchQuery { q: String }

async fn get_upcoming(State(state): State<AppState>) -> Json<Vec<crate::integrations::tmdb::TmdbMedia>> {
    let mut results = state.tmdb.get_upcoming_movies().await.unwrap_or_default();
    let mut tv = state.tmdb.get_trending_tv().await.unwrap_or_default();
    results.append(&mut tv);
    Json(results)
}

#[derive(Deserialize)]
struct TrackRequest { id: u32, title: String, poster_path: Option<String>, release_date: Option<String>, media_type: String }

async fn track_show(State(state): State<AppState>, Json(req): Json<TrackRequest>) -> Json<bool> {
    let _ = db::insert_tracked_show(&state.pool, &req.title, req.id, &req.media_type, req.poster_path, req.release_date).await;
    Json(true)
}

async fn get_tracked(State(state): State<AppState>) -> Json<Vec<TrackedShow>> {
    Json(db::get_tracked_shows(&state.pool).await.unwrap_or_default())
}

#[derive(Deserialize)]
struct MatchRequest { tmdb_id: u32, title: String, poster_path: Option<String>, apply_to_all: bool }

async fn match_media(State(state): State<AppState>, Path(id): Path<i64>, Json(req): Json<MatchRequest>) -> Json<bool> {
    let item = match db::get_item_by_id(&state.pool, id).await { Ok(Some(i)) => i, _ => return Json(false) };
    let mut ids = vec![id];
    if req.apply_to_all {
        if let Ok(others) = db::get_items_by_title(&state.pool, &item.title).await {
            for o in others { if o.id != id { ids.push(o.id); } }
        }
    }
    for tid in ids {
        let _ = db::insert_manual_match(&state.pool, &item.title, req.tmdb_id, &req.title, req.poster_path.clone()).await;
        let _ = db::manual_match_item(&state.pool, tid, req.tmdb_id, &req.title, req.poster_path.clone()).await;
        let p = state.pool.clone(); let t = state.tmdb.clone(); let o = state.ollama.clone(); let mid = req.tmdb_id;
        tokio::spawn(async move {
            if let Ok(Some(i)) = db::get_item_by_id(&p, tid).await {
                let path = std::path::PathBuf::from("./ingest").join(&i.original_filename);
                let _ = crate::run_pipeline(tid, path, p, t, o, Some(mid)).await;
            }
        });
    }
    Json(true)
}

async fn delete_tracked(State(state): State<AppState>, Path(id): Path<i64>) -> Json<bool> {
    let _ = db::delete_tracked_show(&state.pool, id).await;
    Json(true)
}

async fn get_episodes(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Vec<db::Episode>> {
    Json(db::get_episodes_for_show(&state.pool, id).await.unwrap_or_default())
}

#[derive(Deserialize)]
struct StatusRequest { status: String }

async fn set_episode_status(State(state): State<AppState>, Path(id): Path<i64>, Json(req): Json<StatusRequest>) -> Json<bool> {
    let _ = db::update_episode_status(&state.pool, id, &req.status).await;
    Json(true)
}

async fn manual_search_episode(State(state): State<AppState>, Path(id): Path<i64>) -> Json<bool> {
    let pool = state.pool.clone();
    tokio::spawn(async move {
        let indexer = crate::integrations::indexer::IndexerClient::new().unwrap();
        let qbit = crate::integrations::torrent::QBittorrentClient::new().unwrap();
        let _ = qbit.login().await;
        if let Ok(rows) = sqlx::query("SELECT e.*, s.title as show_title FROM episodes e JOIN tracked_shows s ON e.show_id = s.id WHERE e.id = ?").bind(id).fetch_all(&pool).await {
            use sqlx::Row;
            if let Some(r) = rows.first() {
                let q = format!("{} S{:02}E{:02}", r.get::<String, _>("show_title"), r.get::<i64, _>("season"), r.get::<i64, _>("episode"));
                if let Ok(res) = indexer.search(&q).await {
                    if let Some(b) = res.first() {
                        let ing = std::fs::canonicalize("./ingest").unwrap_or_else(|_| std::path::PathBuf::from("./ingest"));
                        if qbit.add_torrent_url(&b.link, Some(&ing.to_string_lossy())).await.is_ok() {
                            let _ = db::update_episode_status(&pool, id, "downloading").await;
                        }
                    }
                }
            }
        }
    });
    Json(true)
}

async fn get_torrents() -> Json<Vec<TorrentInfo>> {
    if let Ok(qbit) = crate::integrations::torrent::QBittorrentClient::new() {
        if let Ok(_) = qbit.login().await { return Json(qbit.get_torrents().await.unwrap_or_default()); }
    }
    Json(vec![])
}

async fn get_sysinfo(State(state): State<AppState>) -> Json<SysInfo> {
    let mut sys = state.sys.lock().unwrap();
    sys.refresh_cpu_usage(); sys.refresh_memory();
    Json(SysInfo { cpu_usage: sys.global_cpu_info().cpu_usage(), memory_used: sys.used_memory() / 1_048_576, memory_total: sys.total_memory() / 1_048_576 })
}

#[derive(Serialize)]
struct SysInfo { cpu_usage: f32, memory_used: u64, memory_total: u64 }

#[derive(Serialize)]
struct DiskInfo { name: String, available: u64, total: u64 }

async fn get_disks() -> Json<Vec<DiskInfo>> {
    let mut disks = Disks::new();
    disks.refresh_list();
    Json(disks.iter().map(|d| DiskInfo { name: d.mount_point().to_string_lossy().to_string(), available: d.available_space() / 1_073_741_824, total: d.total_space() / 1_073_741_824 }).collect())
}

async fn get_profiles(State(state): State<AppState>) -> Json<Vec<QualityProfile>> {
    Json(db::get_all_quality_profiles(&state.pool).await.unwrap_or_default())
}

async fn scan_library(State(state): State<AppState>) -> Json<bool> {
    let pool = state.pool.clone();
    tokio::spawn(async move { let _ = crate::scan_library(pool).await; });
    Json(true)
}

async fn trigger_update() -> Json<bool> {
    tokio::spawn(async { let _ = std::process::Command::new(std::env::current_exe().unwrap()).arg("update").spawn(); });
    Json(true)
}
