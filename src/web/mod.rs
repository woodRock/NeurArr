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
use crate::db::{self, TrackedShow};
use sysinfo::System;
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
        .route("/api/sysinfo", get(get_sysinfo))
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
<html>
<head>
    <title>NeurArr Dashboard</title>
    <style>
        body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; background: #0f172a; color: #f8fafc; margin: 0; }
        .nav { background: #1e293b; padding: 1rem 2rem; display: flex; gap: 2rem; border-bottom: 1px solid #334155; position: sticky; top: 0; z-index: 10; align-items: center; }
        .nav a { color: #94a3b8; text-decoration: none; font-weight: 600; cursor: pointer; }
        .nav a.active { color: #38bdf8; }
        .sysinfo { margin-left: auto; color: #94a3b8; font-size: 0.875rem; display: flex; gap: 1rem; }
        .search-container { background: #1e293b; padding: 1rem 2rem; border-bottom: 1px solid #334155; display: flex; gap: 1rem; }
        .search-input { background: #0f172a; border: 1px solid #334155; color: #f8fafc; padding: 0.5rem 1rem; border-radius: 0.375rem; flex-grow: 1; outline: none; }
        .search-input:focus { border-color: #38bdf8; }
        .container { max-width: 1200px; margin: 2rem auto; padding: 0 1rem; }
        .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: 1.5rem; }
        .card { background: #1e293b; border-radius: 0.75rem; padding: 1.5rem; border: 1px solid #334155; display: flex; flex-direction: column; }
        .poster { width: 100%; height: 400px; object-fit: cover; border-radius: 0.5rem; margin-bottom: 1rem; background: #334155; }
        .title { font-size: 1.25rem; font-weight: 600; margin: 0 0 0.5rem 0; color: #f8fafc; }
        .status { font-size: 0.75rem; font-weight: 700; text-transform: uppercase; padding: 0.25rem 0.5rem; border-radius: 0.25rem; display: inline-block; margin-bottom: 1rem; align-self: flex-start; }
        .status-wanted { background: #1e3a8a; color: #60a5fa; }
        .status-completed { background: #065f46; color: #34d399; }
        .status-downloading { background: #b45309; color: #fef08a; }
        .btn { background: #38bdf8; color: #0f172a; border: none; padding: 0.5rem 1rem; border-radius: 0.375rem; font-weight: 600; cursor: pointer; margin-top: auto; }
        .btn:hover { background: #7dd3fc; }
        .btn-secondary { background: #475569; color: white; }
        .btn-danger { background: #ef4444; color: white; margin-top: 0.5rem; }
        .hidden { display: none; }
        
        /* Modal for matching */
        .modal { position: fixed; top: 0; left: 0; width: 100%; height: 100%; background: rgba(0,0,0,0.8); display: none; align-items: center; justify-content: center; z-index: 100; }
        .modal.active { display: flex; }
        .modal-content { background: #1e293b; padding: 2rem; border-radius: 1rem; width: 90%; max-width: 800px; max-height: 80vh; overflow-y: auto; }
    </style>
</head>
<body>
    <div class="nav">
        <a id="nav-queue" onclick="showTab('queue')" class="active">Processing Queue</a>
        <a id="nav-upcoming" onclick="showTab('upcoming')">Upcoming & Trending</a>
        <a id="nav-tracked" onclick="showTab('tracked')">Tracked Shows</a>
        <div class="sysinfo">
            <span id="sys-cpu">CPU: 0%</span>
            <span id="sys-ram">RAM: 0 MB</span>
        </div>
    </div>

    <div class="search-container">
        <input type="text" id="search-query" class="search-input" placeholder="Search for a movie or TV show...">
        <button class="btn" onclick="performGlobalSearch()">Search</button>
    </div>

    <div class="container">
        <div id="tab-search" class="hidden">
            <h1 class="title">Search Results</h1>
            <div id="search-grid" class="grid"></div>
        </div>
        <div id="tab-queue">
            <div style="display: flex; align-items: center; gap: 2rem;">
                <h1 class="title">Media Queue</h1>
                <button class="btn btn-danger" onclick="clearQueue()" style="margin-top: 0">Clear Queue</button>
            </div>
            <div id="queue-grid" class="grid"></div>
        </div>
        <div id="tab-upcoming" class="hidden">
            <h1 class="title">Upcoming & Trending</h1>
            <div id="upcoming-grid" class="grid"></div>
        </div>
        <div id="tab-tracked" class="hidden">
            <h1 class="title">Tracked Shows</h1>
            <div id="tracked-grid" class="grid"></div>
        </div>
    </div>

    <div id="match-modal" class="modal">
        <div class="modal-content">
            <h2 class="title">Manual Match</h2>
            <input type="text" id="modal-search-query" class="search-input" placeholder="Refine search...">
            <button class="btn" onclick="searchInModal()">Search</button>
            <button class="btn btn-secondary" onclick="closeModal()">Cancel</button>
            <div id="modal-results" class="grid" style="margin-top: 1rem"></div>
        </div>
    </div>

    <script>
        let currentMatchId = null;

        function showTab(tab) {
            ['queue', 'upcoming', 'tracked', 'search'].forEach(t => {
                const el = document.getElementById('tab-' + t);
                if (el) el.classList.toggle('hidden', t !== tab);
                const nav = document.getElementById('nav-' + t);
                if (nav) nav.classList.toggle('active', t === tab);
            });
            if (tab === 'queue') fetchQueue();
            if (tab === 'upcoming') fetchUpcoming();
            if (tab === 'tracked') fetchTracked();
        }

        async function performGlobalSearch() {
            const query = document.getElementById('search-query').value;
            if (!query) return;
            showTab('search');
            await fetchAndRenderResults('/api/search?q=' + encodeURIComponent(query), 'search-grid', false);
        }

        async function fetchAndRenderResults(url, gridId, isModal) {
            const res = await fetch(url);
            const data = await res.json();
            const grid = document.getElementById(gridId);
            grid.innerHTML = data.map(item => `
                <div class="card">
                    <img src="https://image.tmdb.org/t/p/w500${item.poster_path}" class="poster" style="height: 300px" onerror="this.src='https://via.placeholder.com/500x750?text=No+Poster'">
                    <h2 class="title">${item.title || item.name}</h2>
                    <div style="margin-bottom: 1rem; color: #94a3b8; font-size: 0.875rem;">${item.release_date || item.first_air_date}</div>
                    ${isModal ? 
                        `<button class="btn" onclick="applyMatch('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}')">Select This Match</button>` :
                        `<button class="btn" onclick="track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}')">Track Show</button>`
                    }
                </div>
            `).join('');
        }

        async function openMatchModal(id, currentTitle) {
            currentMatchId = id;
            document.getElementById('match-modal').classList.add('active');
            document.getElementById('modal-search-query').value = currentTitle;
            searchInModal();
        }

        function closeModal() {
            document.getElementById('match-modal').classList.remove('active');
        }

        async function searchInModal() {
            const query = document.getElementById('modal-search-query').value;
            await fetchAndRenderResults('/api/search?q=' + encodeURIComponent(query), 'modal-results', true);
        }

        async function applyMatch(tmdbId, title, poster) {
            const applyToAll = confirm('Match all other items with the same detected title to this as well? (Smart Match)');
            await fetch(`/api/media/${currentMatchId}/match`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ tmdb_id: parseInt(tmdbId), title, poster_path: poster, apply_to_all: applyToAll })
            });
            closeModal();
            fetchQueue();
        }

        async function clearQueue() {
            if (!confirm('Clear all items from the processing queue history?')) return;
            await fetch('/api/media/clear', { method: 'DELETE' });
            fetchQueue();
        }

        async function fetchQueue() {
            const res = await fetch('/api/media');
            const data = await res.json();
            const grid = document.getElementById('queue-grid');
            grid.innerHTML = data.map(item => `
                <div class="card">
                    ${item.poster_path ? `<img src="https://image.tmdb.org/t/p/w500${item.poster_path}" class="poster" style="height: 200px">` : ''}
                    <span class="status status-${item.status}">${item.status}</span>
                    <h2 class="title">${item.title}</h2>
                    <div style="color: #94a3b8; font-size: 0.875rem; margin-bottom: 1rem;">${item.original_filename}</div>
                    <button class="btn btn-secondary" onclick="openMatchModal(${item.id}, '${item.title.replace(/'/g, "\\'")}')">Manual Match</button>
                    <p style="font-size: 0.875rem; color: #cbd5e1; margin-top: 1rem;">${item.spoiler_free_summary || 'Processing...'}</p>
                </div>
            `).join('');
        }

        async function fetchUpcoming() {
            const res = await fetch('/api/upcoming');
            const data = await res.json();
            const grid = document.getElementById('upcoming-grid');
            grid.innerHTML = data.map(item => `
                <div class="card">
                    <img src="https://image.tmdb.org/t/p/w500${item.poster_path}" class="poster" onerror="this.src='https://via.placeholder.com/500x750?text=No+Poster'">
                    <h2 class="title">${item.title || item.name}</h2>
                    <div style="margin-bottom: 1rem; color: #94a3b8;">${item.release_date || item.first_air_date}</div>
                    <button class="btn" onclick="track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}')">Track Show</button>
                </div>
            `).join('');
        }

        async function track(id, title, poster, date, type) {
            await fetch('/api/track', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ id: parseInt(id), title, poster_path: poster, release_date: date, media_type: type || 'movie' })
            });
            alert('Added ' + title + ' to tracked shows!');
        }

        async function fetchTracked() {
            const res = await fetch('/api/tracked');
            const data = await res.json();
            const grid = document.getElementById('tracked-grid');
            grid.innerHTML = data.map(item => `
                <div class="card">
                    <img src="https://image.tmdb.org/t/p/w500${item.poster_path}" class="poster" onerror="this.src='https://via.placeholder.com/500x750?text=No+Poster'">
                    <span class="status status-${item.status}">${item.status}</span>
                    <h2 class="title">${item.title}</h2>
                    <div style="color: #94a3b8; margin-bottom: 1rem;">Released: ${item.release_date || 'Unknown'}</div>
                    <button class="btn btn-danger" onclick="deleteTracked(${item.id})">Stop Tracking</button>
                </div>
            `).join('');
        }

        async function deleteTracked(id) {
            if (!confirm('Stop tracking this show?')) return;
            await fetch('/api/tracked/' + id, { method: 'DELETE' });
            fetchTracked();
        }

        async function fetchSysInfo() {
            try {
                const res = await fetch('/api/sysinfo');
                const data = await res.json();
                document.getElementById('sys-cpu').innerText = `CPU: ${data.cpu_usage.toFixed(1)}%`;
                document.getElementById('sys-ram').innerText = `RAM: ${data.memory_used} MB / ${data.memory_total} MB`;
            } catch (e) {}
        }

        fetchQueue();
        fetchSysInfo();
        setInterval(fetchQueue, 5000);
        setInterval(fetchSysInfo, 2000);
    </script>
</body>
</html>
"#)
}

async fn get_media(State(state): State<AppState>) -> Json<Vec<MediaItem>> {
    let items = sqlx::query_as::<_, MediaItem>(
        "SELECT id, original_filename, title, season, episode, status, spoiler_free_summary, poster_path 
         FROM media_items 
         ORDER BY processed_at DESC"
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    Json(items)
}

async fn clear_queue(State(state): State<AppState>) -> Json<bool> {
    let _ = db::clear_media_queue(&state.pool).await;
    Json(true)
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search_media(State(state): State<AppState>, Query(params): Query<SearchQuery>) -> Json<Vec<crate::integrations::tmdb::TmdbMedia>> {
    let mut movies = state.tmdb.search_movie(&params.q).await.unwrap_or_default();
    let mut tv = state.tmdb.search_tv(&params.q).await.unwrap_or_default();
    
    // Tag them for the UI
    for m in &mut movies { m.media_type = Some("movie".to_string()); }
    for t in &mut tv { t.media_type = Some("tv".to_string()); }
    
    movies.append(&mut tv);
    Json(movies)
}

async fn get_upcoming(State(state): State<AppState>) -> Json<Vec<crate::integrations::tmdb::TmdbMedia>> {
    let mut results = state.tmdb.get_upcoming_movies().await.unwrap_or_default();
    let mut tv = state.tmdb.get_trending_tv().await.unwrap_or_default();
    results.append(&mut tv);
    Json(results)
}

#[derive(Deserialize)]
struct TrackRequest {
    id: u32,
    title: String,
    poster_path: Option<String>,
    release_date: Option<String>,
    media_type: String,
}

async fn track_show(State(state): State<AppState>, Json(req): Json<TrackRequest>) -> Json<bool> {
    match db::insert_tracked_show(&state.pool, &req.title, req.id, &req.media_type, req.poster_path, req.release_date).await {
        Ok(_) => Json(true),
        Err(e) => {
            tracing::error!("Failed to track show: {}", e);
            Json(false)
        }
    }
}

async fn get_tracked(State(state): State<AppState>) -> Json<Vec<TrackedShow>> {
    let items = db::get_tracked_shows(&state.pool).await.unwrap_or_default();
    Json(items)
}

#[derive(Deserialize)]
struct MatchRequest {
    tmdb_id: u32,
    title: String,
    poster_path: Option<String>,
    apply_to_all: bool,
}

async fn match_media(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<MatchRequest>,
) -> Json<bool> {
    let item = match db::get_item_by_id(&state.pool, id).await {
        Ok(Some(i)) => i,
        _ => return Json(false),
    };

    let mut ids_to_process = vec![id];
    let original_title = item.title.clone();

    // Smart matching: find others with same title
    if req.apply_to_all {
        let mut others = db::get_items_by_title(&state.pool, &original_title).await.unwrap_or_default();
        
        // Fallback: if no exact matches, try finding items where titles are partially similar
        if others.is_empty() {
            let all_pending = sqlx::query_as::<_, MediaItem>("SELECT * FROM media_items WHERE status != 'completed'")
                .fetch_all(&state.pool).await.unwrap_or_default();
            
            for item in all_pending {
                let t1 = item.title.to_lowercase();
                let t2 = original_title.to_lowercase();
                if (t1.contains(&t2) || t2.contains(&t1)) && item.id != id {
                    others.push(item);
                }
            }
        }

        for other in others {
            if other.id != id {
                ids_to_process.push(other.id);
            }
        }
    }

    for target_id in ids_to_process {
        // Save to manual match history so future files with this title are auto-matched
        let _ = db::insert_manual_match(&state.pool, &original_title, req.tmdb_id, &req.title, req.poster_path.clone()).await;

        if let Err(e) = db::manual_match_item(&state.pool, target_id, req.tmdb_id, &req.title, req.poster_path.clone()).await {
            tracing::error!("Failed to match item {}: {}", target_id, e);
            continue;
        }

        // Trigger processing pipeline for each matched item
        let pool = state.pool.clone();
        let tmdb = state.tmdb.clone();
        let ollama = state.ollama.clone();
        let tid = req.tmdb_id;
        
        tokio::spawn(async move {
            if let Ok(Some(item)) = db::get_item_by_id(&pool, target_id).await {
                let path = std::path::PathBuf::from("./ingest").join(&item.original_filename);
                if let Err(e) = crate::run_pipeline(target_id, path, pool, tmdb, ollama, Some(tid)).await {
                    tracing::error!("Failed manual pipeline for {}: {}", target_id, e);
                }
            }
        });
    }

    Json(true)
}

async fn delete_tracked(State(state): State<AppState>, Path(id): Path<i64>) -> Json<bool> {
    let _ = db::delete_tracked_show(&state.pool, id).await;
    Json(true)
}

#[derive(Serialize)]
struct SysInfo {
    cpu_usage: f32,
    memory_used: u64,
    memory_total: u64,
}

async fn get_sysinfo(State(state): State<AppState>) -> Json<SysInfo> {
    let mut sys = state.sys.lock().unwrap();
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    
    Json(SysInfo {
        cpu_usage: sys.global_cpu_info().cpu_usage(),
        memory_used: sys.used_memory() / 1_048_576, // MB
        memory_total: sys.total_memory() / 1_048_576,
    })
}
