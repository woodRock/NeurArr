use anyhow::Result;
use axum::{
    extract::{State, Query, Path},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post, delete},
    middleware::{self, Next},
    http::{Request, StatusCode},
    Json, Router,
};

async fn auth_middleware(
    jar: CookieJar,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    if jar.get("auth").is_none() {
        let path = req.uri().path();
        if path == "/login" {
            return Ok(next.run(req).await);
        }
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}
use axum_extra::extract::cookie::{Cookie, CookieJar};
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
    log_tx: tokio::sync::broadcast::Sender<String>,
    is_scanning: Arc<std::sync::atomic::AtomicBool>,
}

pub async fn start_web_server(pool: SqlitePool, log_tx: tokio::sync::broadcast::Sender<String>) -> Result<()> {
    let tmdb = TmdbClient::new()?;
    let ollama = Arc::new(crate::llm::OllamaClient::new()?);
    let mut sys = System::new_all();
    sys.refresh_all();
    let state = AppState { 
        pool, 
        tmdb, 
        ollama,
        sys: Arc::new(StdMutex::new(sys)),
        log_tx,
        is_scanning: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let app = Router::new()
        .route("/", get(dashboard))
        .route("/api/media", get(get_media))
        .route("/api/media/clear", delete(clear_queue))
        .route("/api/media/{id}/match", post(match_media))
        .route("/api/search", get(search_media))
        .route("/api/search/genre", get(search_by_genre))
        .route("/api/upcoming", get(get_upcoming))
        .route("/api/calendar", get(get_calendar))
        .route("/api/track", post(track_show))
        .route("/api/tracked", get(get_tracked))
        .route("/api/tracked/{id}", delete(delete_tracked))
        .route("/api/tracked/{id}/episodes", get(get_episodes))
        .route("/api/tracked/{id}/watched", post(mark_watched))
        .route("/api/tracked/{id}/rating", post(rate_item))
        .route("/api/tracked/{id}/trailers", get(get_trailers))
        .route("/api/tracked/{id}/credits", get(get_credits))
        .route("/api/episodes/{id}/status", post(set_episode_status))
        .route("/api/episodes/{id}/search", post(manual_search_episode))
        .route("/api/recommendations", get(get_recommendations))
        .route("/api/preferences/chips", get(get_preference_chips))
        .route("/api/interactive-search", post(interactive_search))
        .route("/api/download-torrent", post(download_torrent))
        .route("/api/torrents", get(get_torrents))
        .route("/api/activity", get(get_activity))
        .route("/api/scan-status", get(get_scan_status))
        .route("/api/sysinfo", get(get_sysinfo))
        .route("/api/disks", get(get_disks))
        .route("/api/logs", get(stream_logs))
        .route("/api/quality-profiles", get(get_profiles))
        .route("/api/update", post(trigger_update))
        .route("/api/scan-library", post(scan_library))
        .route("/api/ingest", post(trigger_ingest))
        .layer(middleware::from_fn(auth_middleware))
        .route("/login", get(login_page).post(handle_login))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    info!("Dashboard available at http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn login_page() -> Html<&'static str> {
    Html(r#"
<!DOCTYPE html><html><head><title>Login - NeurArr</title><script src="https://cdn.tailwindcss.com"></script></head>
<body class="bg-slate-950 flex items-center justify-center min-h-screen">
    <form action="/login" method="POST" class="bg-slate-900 p-8 rounded-2xl border border-slate-800 w-full max-w-sm">
        <h1 class="text-2xl font-bold text-white mb-6">NeurArr Login</h1>
        <input type="text" name="username" placeholder="Username" class="w-full bg-slate-800 border border-slate-700 rounded-lg p-3 text-white mb-4">
        <input type="password" name="password" placeholder="Password" class="w-full bg-slate-800 border border-slate-700 rounded-lg p-3 text-white mb-6">
        <button type="submit" class="w-full bg-sky-600 text-white font-bold py-3 rounded-lg">Login</button>
    </form>
</body></html>"#)
}

#[derive(Deserialize)]
struct LoginData { username: String, password: String }

async fn handle_login(State(state): State<AppState>, jar: CookieJar, axum::Form(data): axum::Form<LoginData>) -> impl IntoResponse {
    if let Ok(Some(hash)) = db::get_user_hash(&state.pool, &data.username).await {
        if crate::utils::auth::verify_password(&data.password, &hash) {
            let cookie = Cookie::build(("auth", "true")).path("/").permanent();
            return (jar.add(cookie), Redirect::to("/"));
        }
    }
    (jar, Redirect::to("/login"))
}

async fn dashboard(jar: CookieJar) -> impl IntoResponse {
    if jar.get("auth").is_none() { return Redirect::to("/login").into_response(); }
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
    <nav class="glass sticky top-0 flex gap-6 p-4 mb-8 rounded-2xl items-center z-50">
        <div class="font-black text-2xl mr-6 text-white tracking-tighter flex items-center gap-2">
            <span class="text-sky-500">Neur</span>Arr
            <div id="scan-indicator" class="hidden"><div class="w-2 h-2 bg-sky-500 rounded-full animate-ping"></div></div>
        </div>
        <button onclick="showTab('recommendations')" id="nav-recommendations" class="active font-bold text-xs tracking-widest uppercase opacity-50 hover:opacity-100 transition-opacity">For You</button>
        <button onclick="showTab('upcoming')" id="nav-upcoming" class="font-bold text-xs tracking-widest uppercase opacity-50 hover:opacity-100 transition-opacity">Discover</button>
        <button onclick="showTab('tracked')" id="nav-tracked" class="font-bold text-xs tracking-widest uppercase opacity-50 hover:opacity-100 transition-opacity">Collection</button>
        <button onclick="showTab('activity')" id="nav-activity" class="font-bold text-xs tracking-widest uppercase opacity-50 hover:opacity-100 transition-opacity">Activity</button>
        
        <div class="ml-auto flex items-center gap-6">
            <div class="hidden md:flex gap-4 text-[10px] text-slate-500 font-mono">
                <span id="sys-cpu">CPU: 0%</span>
                <span id="sys-ram">RAM: 0MB</span>
            </div>
            <button onclick="toggleSettingsMenu()" class="text-slate-400 hover:text-white transition-colors">
                <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.065 2.572c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.572 1.065c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.065-2.572c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z" />
                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
                </svg>
            </button>
        </div>
    </nav>

    <div id="settings-menu" class="hidden fixed right-4 top-20 w-64 glass rounded-2xl p-4 z-50 shadow-2xl border border-white/5">
        <div class="flex flex-col gap-2">
            <button onclick="showTab('calendar')" class="text-left px-4 py-2 hover:bg-white/5 rounded-lg text-xs font-bold uppercase tracking-wider text-slate-400 hover:text-white">Calendar</button>
            <button onclick="showTab('settings')" class="text-left px-4 py-2 hover:bg-white/5 rounded-lg text-xs font-bold uppercase tracking-wider text-slate-400 hover:text-white">System Settings</button>
            <button onclick="showTab('logs')" class="text-left px-4 py-2 hover:bg-white/5 rounded-lg text-xs font-bold uppercase tracking-wider text-slate-400 hover:text-white">Live Logs</button>
            <hr class="border-white/5 my-2">
            <button onclick="updateApp()" class="text-left px-4 py-2 hover:bg-white/5 rounded-lg text-xs font-bold uppercase tracking-wider text-amber-500">Update App</button>
        </div>
    </div>

    <div class="max-w-7xl mx-auto">
        <div class="flex gap-4 mb-12">
            <input type="text" id="search-query" placeholder="Search for something new..." class="flex-grow bg-white/5 border border-white/10 rounded-2xl px-8 py-4 outline-none focus:ring-2 focus:ring-sky-500/50 text-lg font-medium placeholder:text-slate-600 transition-all">
            <button onclick="performGlobalSearch()" class="bg-sky-600 px-10 py-4 rounded-2xl font-bold hover:bg-sky-500 transition-all shadow-lg shadow-sky-900/20 active:scale-95">SEARCH</button>
        </div>

        <div id="tab-recommendations" class="space-y-8">
            <div class="flex justify-between items-end">
                <h2 class="text-2xl font-black text-white uppercase tracking-tighter">For You</h2>
                <div id="preference-chips" class="flex gap-2"></div>
            </div>
            <div id="recommendation-results" class="grid grid-cols-2 md:grid-cols-5 gap-6"></div>
        </div>

        <div id="tab-upcoming" class="hidden space-y-8">
            <h2 class="text-2xl font-black text-white uppercase tracking-tighter">Discover</h2>
            <div id="upcoming-results" class="grid grid-cols-2 md:grid-cols-5 gap-6"></div>
        </div>

        <div id="tab-tracked" class="hidden grid grid-cols-2 md:grid-cols-5 gap-6"></div>
        
        <div id="tab-activity" class="hidden space-y-4">
            <h2 class="text-2xl font-black text-white uppercase tracking-tighter mb-6">Current Activity</h2>
            <div id="activity-list" class="space-y-3"></div>
        </div>

        <div id="tab-calendar" class="hidden space-y-4"></div>
        <div id="tab-search" class="hidden grid grid-cols-2 md:grid-cols-5 gap-6"></div>
        <div id="tab-logs" class="hidden glass p-6 rounded-2xl font-mono text-[11px] h-[70vh] overflow-y-auto space-y-1 border border-white/5" id="log-container"></div>
        
        <div id="tab-settings" class="hidden glass p-8 rounded-xl">
            <h2 class="font-bold mb-4 text-xl">System Status</h2>
            <div id="disk-info" class="space-y-4"></div>
            <div class="mt-8 border-t border-slate-800 pt-8 flex gap-4">
                <button onclick="scanLibrary()" class="bg-sky-600 px-6 py-3 rounded-xl font-bold hover:bg-sky-500 transition-colors">FULL LIBRARY RE-SCAN</button>
                <button onclick="triggerIngest()" class="bg-amber-600 px-6 py-3 rounded-xl font-bold hover:bg-amber-500 transition-colors">SCAN INGEST FOLDER</button>
                <button onclick="clearQueue()" class="bg-rose-600 px-6 py-3 rounded-xl font-bold hover:bg-rose-500 transition-colors">PURGE QUEUE HISTORY</button>
            </div>
        </div>
    </div>

    <!-- Modals -->
    <div id="match-modal" class="modal"><div class="glass p-8 rounded-3xl w-full max-w-4xl max-h-[80vh] overflow-y-auto">
        <div class="flex justify-between items-center mb-6">
            <h2 class="text-xl font-bold">Manual Match Selection</h2>
            <button onclick="closeModal()" class="text-slate-400 hover:text-white font-bold">CLOSE</button>
        </div>
        <div class="flex gap-4 mb-6">
            <input type="text" id="modal-search-input" class="flex-grow glass rounded-xl px-4 py-2 outline-none focus:ring-2 focus:ring-sky-500/50">
            <button onclick="searchInModal()" class="bg-sky-600 px-6 py-2 rounded-xl font-bold hover:bg-sky-500 transition-colors text-sm">SEARCH</button>
        </div>
        <div id="modal-results" class="grid grid-cols-2 md:grid-cols-3 gap-4"></div>
    </div></div>

    <div id="episodes-modal" class="modal"><div class="glass p-8 rounded-3xl w-full max-w-4xl max-h-[80vh] overflow-y-auto">
        <div class="flex justify-between items-center mb-6">
            <h2 id="episode-modal-title" class="text-xl font-bold"></h2>
            <button onclick="closeEpisodeModal()" class="text-slate-400 hover:text-white font-bold">CLOSE</button>
        </div>
        <div id="episodes-list" class="space-y-2"></div>
    </div></div>

    <div id="interactive-modal" class="modal"><div class="glass p-8 rounded-3xl w-full max-w-5xl max-h-[80vh] overflow-y-auto">
        <div class="flex justify-between items-center mb-6">
            <h2 id="interactive-modal-title" class="text-xl font-bold text-sky-400">Interactive Search</h2>
            <button onclick="closeInteractiveModal()" class="text-slate-400 hover:text-white font-bold">CLOSE</button>
        </div>
        <div class="flex gap-4 mb-6">
            <input type="text" id="interactive-search-input" class="flex-grow glass rounded-xl px-4 py-2 outline-none focus:ring-2 focus:ring-sky-500/50">
            <button onclick="performInteractiveSearch()" class="bg-sky-600 px-6 py-2 rounded-xl font-bold hover:bg-sky-500 transition-colors text-sm">SEARCH</button>
        </div>
        <div class="overflow-x-auto">
            <table class="w-full text-left border-collapse">
                <thead><tr class="text-[10px] uppercase text-slate-500 border-b border-slate-800"><th class="pb-2 px-2">Title</th><th class="pb-2 px-2">Size</th><th class="pb-2 px-2 text-center">Seeders</th><th class="pb-2 px-2">Indexer</th><th class="pb-2 px-2 text-right">Action</th></tr></thead>
                <tbody id="interactive-results" class="text-xs"></tbody>
            </table>
        </div>
    </div></div>

    <div id="item-details-modal" class="modal"><div class="glass p-8 rounded-3xl w-full max-w-6xl max-h-[90vh] overflow-y-auto">
        <div class="flex justify-between items-start mb-6">
            <div>
                <h2 id="details-title" class="text-3xl font-black text-white uppercase tracking-tighter"></h2>
                <div id="details-genres" class="flex gap-2 mt-2"></div>
            </div>
            <button onclick="closeItemDetails()" class="text-slate-400 hover:text-white font-bold text-xs border border-white/10 px-3 py-1 rounded">CLOSE</button>
        </div>
        <div class="grid grid-cols-1 lg:grid-cols-3 gap-8">
            <div class="lg:col-span-2 space-y-8">
                <div id="details-trailer" class="aspect-video bg-black rounded-2xl overflow-hidden shadow-2xl border border-white/5"></div>
                <div>
                    <h3 class="text-sky-400 font-black text-xs uppercase mb-4 tracking-widest">Cast & Characters</h3>
                    <div id="details-cast" class="grid grid-cols-3 md:grid-cols-5 gap-4"></div>
                </div>
            </div>
            <div class="space-y-6">
                <div id="details-overview" class="text-slate-300 text-sm leading-relaxed italic border-l-2 border-sky-500/30 pl-4"></div>
                <div id="details-recommendations" class="space-y-4">
                    <h3 class="text-sky-400 font-black text-xs uppercase tracking-widest">Similar Titles</h3>
                    <div id="details-recs-list" class="space-y-2"></div>
                </div>
            </div>
        </div>
    </div></div>

    <script>
        let currentMatchId = null;
        let currentSearchEpisodeId = null;
        let currentSearchShowId = null;
        const placeholder = 'data:image/svg+xml;base64,PHN2ZyB3aWR0aD0iMjAwIiBoZWlnaHQ9IjMwMCIgeG1sbnM9Imh0dHA6Ly93d3cudzMub3JnLzIwMDAvc3ZnIj48cmVjdCB3aWR0aD0iMTAwJSIgaGVpZ2h0PSIxMDAlIiBmaWxsPSIjMWUyOTNiIi8+PHRleHQgeD0iNTAlIiB5PSI1MCUiIGZpbGw9IiM0NzU1NjkiIGZvbnQtc2l6ZT0iMTQiIGZvbnQtZmFtaWx5PSJzYW5zLXNlcmlmIiBkeT0iLjNlbSIgdGV4dC1hbmNob3I9Im1pZGRsZSI+Tk8gUE9TVEVSPC90ZXh0Pjwvc3ZnPg==';

        function openInteractiveSearch(title, episodeId, showId, epCode = '') {
            currentSearchEpisodeId = episodeId;
            currentSearchShowId = showId;
            const query = epCode ? `${title} ${epCode}` : title;
            document.getElementById('interactive-modal-title').innerText = 'Search: ' + query;
            document.getElementById('interactive-search-input').value = query;
            document.getElementById('interactive-modal').classList.add('active');
            document.getElementById('interactive-results').innerHTML = '';
            performInteractiveSearch();
        }

        function closeInteractiveModal() { document.getElementById('interactive-modal').classList.remove('active'); }

        async function performInteractiveSearch() {
            const query = document.getElementById('interactive-search-input').value;
            if(!query) return;
            document.getElementById('interactive-results').innerHTML = '<tr><td colspan="5" class="text-center py-10 text-slate-500 uppercase font-bold tracking-widest animate-pulse">Querying Indexers...</td></tr>';
            const res = await fetch('/api/interactive-search', {
                method: 'POST', headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ query })
            });
            const data = await res.json();
            document.getElementById('interactive-results').innerHTML = data.length ? data.map(item => `
                <tr class="border-b border-slate-800/50 hover:bg-white/5 transition-colors text-[11px]">
                    <td class="py-3 px-2 max-w-md"><div class="font-bold text-slate-200 truncate">${item.title}</div></td>
                    <td class="py-3 px-2 text-slate-400 font-mono">${(item.size / 1024 / 1024 / 1024).toFixed(2)} GB</td>
                    <td class="py-3 px-2 text-center"><span class="px-2 py-0.5 rounded bg-emerald-500/10 text-emerald-500 font-bold">${item.seeders}</span></td>
                    <td class="py-3 px-2 text-slate-500 italic">${item.indexer}</td>
                    <td class="py-3 px-2 text-right">
                        <button onclick="downloadTorrent('${item.link.replace(/'/g, "\\'")}', '${item.title.replace(/'/g, "\\'")}')" class="bg-sky-600 hover:bg-sky-500 text-white px-3 py-1 rounded font-bold text-[10px] uppercase transition-all">Download</button>
                    </td>
                </tr>
            `).join('') : '<tr><td colspan="5" class="text-center py-10 text-rose-400 font-bold">No results found on any indexers.</td></tr>';
        }

        async function downloadTorrent(link, title) {
            const res = await fetch('/api/download-torrent', {
                method: 'POST', headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ 
                    link, 
                    title, 
                    episode_id: currentSearchEpisodeId,
                    show_id: currentSearchShowId
                })
            });
            if(await res.json()) {
                alert('Added to downloader!');
                closeInteractiveModal();
                if(currentSearchEpisodeId) { closeEpisodeModal(); fetchQueue(); }
                if(currentSearchShowId) fetchTracked();
            } else {
                alert('Failed to add torrent.');
            }
        }

        function showTab(tab) {
            ['queue', 'tracked', 'upcoming', 'downloads', 'settings', 'search', 'calendar', 'recommendations'].forEach(t => {
                const el = document.getElementById('tab-' + t);
                if (el) el.classList.toggle('hidden', t !== tab);
                const nav = document.getElementById('nav-' + t);
                if (nav) nav.classList.toggle('active', t === tab);
            });
            if(tab === 'queue') fetchQueue(); 
            if(tab === 'tracked') fetchTracked(); 
            if(tab === 'upcoming') { fetchUpcoming(); fetchChips(); }
            if(tab === 'recommendations') { fetchRecommendations(); fetchChips(); }
            if(tab === 'downloads') fetchTorrents(); 
            if(tab === 'settings') fetchDisks(); 
            if(tab === 'calendar') fetchCalendar();
        }

        async function fetchQueue() {
            const res = await fetch('/api/media'); const data = await res.json();
            document.getElementById('tab-queue').innerHTML = data.length ? data.map(item => `
                <div class="glass p-4 rounded-xl card-content">
                    <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w200${item.poster_path}` : placeholder}" class="w-16 h-24 float-left mr-4 rounded shadow-lg" onerror="this.src='${placeholder}'">
                    <div class="font-bold truncate text-sm text-sky-400">${item.title}</div>
                    <div class="text-[9px] font-black bg-slate-800 text-slate-400 px-1.5 py-0.5 rounded inline-block mt-1 uppercase">${item.status}</div>
                    <div class="text-[10px] text-slate-500 mt-2 truncate">${item.original_filename}</div>
                    <button onclick="openMatchModal(${item.id}, '${item.title.replace(/'/g, "\\'")}')" class="text-[10px] font-bold text-sky-400 mt-3 block hover:underline uppercase">Manual Match</button>
                </div>
            `).join('') : '<div class="col-span-3 text-center text-slate-500 py-20">Queue is empty.</div>';
        }

        async function fetchTracked() {
            const res = await fetch('/api/tracked'); const data = await res.json();
            document.getElementById('tab-tracked').innerHTML = data.length ? data.map(item => `
                <div class="glass rounded-xl overflow-hidden group border border-slate-800/50">
                    <div class="relative"><img src="${item.poster_path ? `https://image.tmdb.org/t/p/w500${item.poster_path}` : placeholder}" class="h-64 w-full object-cover" onerror="this.src='${placeholder}'">
                    <div class="absolute inset-0 bg-black/60 opacity-0 group-hover:opacity-100 flex flex-col items-center justify-center transition-all gap-2">
                        <button onclick="deleteTracked(${item.id})" class="bg-rose-600 px-4 py-2 rounded text-xs font-bold w-32">REMOVE</button>
                        ${item.status !== 'watched' ? `<button onclick="markWatched(${item.id})" class="bg-emerald-600 px-4 py-2 rounded text-xs font-bold w-32">WATCHED</button>` : ''}
                    </div></div>
                    <div class="p-3">
                        <div class="font-bold text-sm truncate">${item.title}</div>
                        <div class="flex items-center gap-1 mt-1">
                            ${[1,2,3,4,5].map(i => `<span onclick="rateItem(${item.id}, ${i})" class="cursor-pointer text-[10px] ${i <= item.rating ? 'text-amber-400' : 'text-slate-600'}">★</span>`).join('')}
                        </div>
                        ${item.status === 'downloading' ? '<div class="text-[9px] font-black bg-sky-500/20 text-sky-400 px-1.5 py-0.5 rounded inline-block mt-1 uppercase animate-pulse">Downloading</div>' : ''}
                        ${item.status === 'watched' ? '<div class="text-[9px] font-black bg-emerald-500/20 text-emerald-400 px-1.5 py-0.5 rounded inline-block mt-1 uppercase">Watched</div>' : ''}
                        <button onclick="openItemDetails(${item.id}, '${item.title.replace(/'/g, "\\'")}')" class="text-xs text-sky-400 mt-1 font-bold uppercase hover:underline block">Details</button>
                        ${item.media_type === 'movie' ? 
                            `<button onclick="openInteractiveSearch('${item.title.replace(/'/g, "\\'")}', null, ${item.id})" class="text-xs text-amber-400 mt-1 font-bold uppercase hover:underline block">Manual Search</button>` :
                            `<button onclick="openEpisodeModal(${item.id}, '${item.title.replace(/'/g, "\\'")}')" class="text-xs text-sky-400 mt-1 font-bold uppercase hover:underline block">View Episodes</button>`
                        }
                    </div>
                </div>
            `).join('') : '<div class="col-span-5 text-center text-slate-500 py-20 uppercase font-bold text-xs tracking-widest">No shows tracked yet.</div>';
        }

        async function performGlobalSearch() {
            const query = document.getElementById('search-query').value; if(!query) return; showTab('search');
            const res = await fetch('/api/search?q=' + encodeURIComponent(query)); const data = await res.json();
            document.getElementById('tab-search').innerHTML = data.map(item => `
                <div class="glass rounded-xl overflow-hidden p-4 border border-slate-800/50">
                    <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w500${item.poster_path}` : placeholder}" class="h-48 w-full object-cover rounded shadow-lg" onerror="this.src='${placeholder}'">
                    <div class="mt-3 text-sm font-bold truncate">${item.title || item.name}</div>
                    <div class="text-[10px] text-slate-500 mb-3">${item.release_date || item.first_air_date || 'Unknown'}</div>
                    <button onclick="track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}')" class="w-full bg-sky-600/20 text-sky-400 py-2 rounded font-bold text-[10px] hover:bg-sky-600 hover:text-white transition-all uppercase tracking-widest">Track</button>
                </div>
            `).join('');
        }

        async function openMatchModal(id, title) { 
            currentMatchId = id; 
            document.getElementById('match-modal').classList.add('active');
            document.getElementById('modal-search-input').value = title;
            searchInModal();
        }

        async function searchInModal() {
            const query = document.getElementById('modal-search-input').value;
            if(!query) return;
            document.getElementById('modal-results').innerHTML = '<div class="col-span-3 text-center text-slate-500 py-10">Searching matches...</div>';
            const res = await fetch('/api/search?q=' + encodeURIComponent(query)); const data = await res.json();
            document.getElementById('modal-results').innerHTML = data.length ? data.map(item => `
                <div class="glass p-3 rounded-xl text-center border border-slate-800">
                    <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w200${item.poster_path}` : placeholder}" class="w-full h-32 object-cover rounded shadow-md" onerror="this.src='${placeholder}'">
                    <div class="font-bold text-[10px] mt-2 truncate w-full">${item.title || item.name}</div>
                    <button onclick="applyMatch('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}')" class="text-sky-400 text-[9px] font-black mt-2 hover:underline uppercase tracking-tighter">Select Match</button>
                </div>
            `).join('') : '<div class="col-span-3 text-center text-rose-400 py-10">No matches found.</div>';
        }

        async function applyMatch(tmdbId, title, poster) {
            const applyToAll = confirm('Smart Match all items with this title?');
            await fetch(`/api/media/${currentMatchId}/match`, {
                method: 'POST', headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ tmdb_id: parseInt(tmdbId), title, poster_path: poster, apply_to_all: applyToAll })
            });
            closeModal(); fetchQueue();
        }

        function closeModal() { document.getElementById('match-modal').classList.remove('active'); }
        async function openEpisodeModal(id, title) { document.getElementById('episode-modal-title').innerText = title; document.getElementById('episodes-modal').classList.add('active'); fetchEpisodes(id); }
        function closeEpisodeModal() { document.getElementById('episodes-modal').classList.remove('active'); }
        
        async function fetchEpisodes(id) {
            const res = await fetch(`/api/tracked/${id}/episodes`); const data = await res.json();
            const showTitle = document.getElementById('episode-modal-title').innerText;
            document.getElementById('episodes-list').innerHTML = data.length ? data.map(ep => `
                <div class="text-[11px] p-3 bg-slate-900 rounded-xl flex justify-between items-center border border-slate-800/50">
                    <span><b class="text-sky-400 mr-2">S${ep.season}E${ep.episode}</b> ${ep.title || 'Episode ' + ep.episode}</span>
                    <div class="flex items-center gap-3">
                        <span class="text-[9px] font-black uppercase ${ep.status === 'downloading' ? 'text-sky-400 animate-pulse' : 'text-slate-500'}">${ep.status}</span>
                        <button onclick="openInteractiveSearch('${showTitle.replace(/'/g, "\\'")}', ${ep.id}, null, 'S${ep.season.toString().padStart(2,'0')}E${ep.episode.toString().padStart(2,'0')}')" class="text-amber-400 hover:text-amber-300 font-bold uppercase text-[9px]">Search</button>
                    </div>
                </div>
            `).join('') : '<div class="text-center text-slate-500 py-10">No episodes found.</div>';
        }

        async function fetchTorrents() {
            const res = await fetch('/api/torrents'); const data = await res.json();
            document.getElementById('tab-downloads').innerHTML = data.length ? data.map(t => `
                <div class="glass p-4 rounded-xl border border-slate-800">
                    <div class="flex justify-between text-xs font-bold mb-2"><span>${t.name}</span><span class="text-sky-400">${(t.progress*100).toFixed(1)}%</span></div>
                    <div class="w-full h-1.5 bg-slate-800 rounded-full overflow-hidden"><div class="bg-sky-500 h-full" style="width:${t.progress*100}%"></div></div>
                    <div class="flex justify-between text-[9px] text-slate-500 mt-2 font-black uppercase"><span>${t.state}</span><span>${(t.dlspeed / 1024 / 1024).toFixed(1)} MB/S</span></div>
                </div>
            `).join('') : '<div class="text-center text-slate-500 py-20 font-bold uppercase tracking-widest text-xs">No active downloads.</div>';
        }

        async function fetchUpcoming() {
            const res = await fetch('/api/upcoming'); const data = await res.json();
            document.getElementById('upcoming-results').innerHTML = data.map(item => `
                <div class="glass rounded-xl overflow-hidden p-4 border border-slate-800/50 text-center">
                    <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w500${item.poster_path}` : placeholder}" class="h-48 w-full object-cover rounded shadow-lg" onerror="this.src='${placeholder}'">
                    <div class="mt-3 text-sm font-bold truncate">${item.title || item.name}</div>
                    <button onclick="track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}')" class="text-sky-400 text-[10px] font-bold mt-2 hover:underline uppercase">Track</button>
                </div>
            `).join('');
        }

        async function fetchRecommendations() {
            const res = await fetch('/api/recommendations'); const data = await res.json();
            document.getElementById('recommendation-results').innerHTML = data.length ? data.map(item => `
                <div class="glass rounded-xl overflow-hidden p-4 border border-slate-800/50 text-center">
                    <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w500${item.poster_path}` : placeholder}" class="h-48 w-full object-cover rounded shadow-lg" onerror="this.src='${placeholder}'">
                    <div class="mt-3 text-sm font-bold truncate">${item.title || item.name}</div>
                    <button onclick="track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}')" class="text-sky-400 text-[10px] font-bold mt-2 hover:underline uppercase">Track</button>
                </div>
            `).join('') : '<div class="col-span-5 text-center text-slate-500 py-20 uppercase font-bold text-xs tracking-widest">Rate some shows to get personalized recommendations!</div>';
        }

        function toggleSettingsMenu() { document.getElementById('settings-menu').classList.toggle('hidden'); }

        function showTab(tab) {
            ['tracked', 'upcoming', 'settings', 'search', 'calendar', 'recommendations', 'activity', 'logs'].forEach(t => {
                const el = document.getElementById('tab-' + t);
                if (el) el.classList.toggle('hidden', t !== tab);
                const nav = document.getElementById('nav-' + t);
                if (nav) {
                    nav.classList.toggle('active', t === tab);
                    nav.style.opacity = t === tab ? '1' : '0.5';
                }
            });
            document.getElementById('settings-menu').classList.add('hidden');
            if(tab === 'tracked') fetchTracked(); 
            if(tab === 'upcoming') { fetchUpcoming(); fetchChips(); }
            if(tab === 'recommendations') { fetchRecommendations(); fetchChips(); }
            if(tab === 'activity') fetchActivity();
            if(tab === 'settings') fetchDisks(); 
            if(tab === 'calendar') fetchCalendar();
        }

        async function fetchActivity() {
            const res = await fetch('/api/activity'); const data = await res.json();
            document.getElementById('activity-list').innerHTML = data.length ? data.map(item => `
                <div class="glass p-4 rounded-2xl flex items-center gap-6 border border-white/5 group hover:border-sky-500/30 transition-all">
                    <div class="w-1.5 h-12 rounded-full ${item.status === 'Downloading' ? 'bg-sky-500 animate-pulse' : 'bg-slate-700'}"></div>
                    <div class="flex-grow">
                        <div class="flex justify-between items-end mb-2">
                            <span class="font-bold text-sm text-slate-200">${item.title}</span>
                            <div class="flex items-center gap-3">
                                <span class="text-[8px] font-black uppercase bg-slate-800 text-slate-500 px-1.5 py-0.5 rounded tracking-tighter">${item.source}</span>
                                <span class="text-[10px] font-black uppercase text-sky-500 tracking-widest">${item.status}</span>
                            </div>
                        </div>
                        <div class="w-full h-1 bg-white/5 rounded-full overflow-hidden">
                            <div class="h-full bg-sky-500 transition-all duration-1000" style="width: ${item.progress * 100}%"></div>
                        </div>
                    </div>
                    <div class="text-[10px] font-mono text-slate-500 w-12 text-right">${(item.progress * 100).toFixed(0)}%</div>
                </div>
            `).join('') : '<div class="text-center py-20 text-slate-500 font-bold uppercase tracking-widest text-xs">No active tasks.</div>';
        }

        async function checkScanStatus() {
            const res = await fetch('/api/scan-status'); const isScanning = await res.json();
            document.getElementById('scan-indicator').classList.toggle('hidden', !isScanning);
        }

        async function fetchChips() {
            const res = await fetch('/api/preferences/chips'); const data = await res.json();
            document.getElementById('preference-chips').innerHTML = data.map(chip => `
                <button onclick="performGenreSearch('${chip}')" class="px-4 py-1.5 rounded-full bg-white/5 border border-white/10 text-slate-400 text-[10px] font-black hover:bg-sky-500 hover:text-white hover:border-sky-500 transition-all uppercase tracking-tighter">
                    ${chip}
                </button>
            `).join('');
        }

        async function performGenreSearch(genre) {
            showTab('search');
            document.getElementById('tab-search').innerHTML = '<div class="col-span-5 text-center py-20 text-slate-500 uppercase font-black animate-pulse tracking-widest">Discovering ${genre} titles...</div>';
            const res = await fetch('/api/search/genre?genre=' + encodeURIComponent(genre));
            const data = await res.json();
            renderSearchResults(data);
        }

        function renderSearchResults(data) {
            document.getElementById('tab-search').innerHTML = data.map(item => `
                <div class="glass rounded-2xl overflow-hidden p-4 border border-white/5 group hover:border-sky-500/50 transition-all">
                    <div class="relative overflow-hidden rounded-xl">
                        <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w500${item.poster_path}` : placeholder}" class="h-48 w-full object-cover group-hover:scale-110 transition-transform duration-500" onerror="this.src='${placeholder}'">
                    </div>
                    <div class="mt-4 text-sm font-bold truncate text-slate-200">${item.title || item.name}</div>
                    <div class="text-[10px] text-slate-500 mb-4">${item.release_date || item.first_air_date || 'Unknown'}</div>
                    <button onclick="track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}')" class="w-full bg-sky-600/10 text-sky-400 py-3 rounded-xl font-black text-[10px] hover:bg-sky-600 hover:text-white transition-all uppercase tracking-widest">Track Item</button>
                </div>
            `).join('');
        }

        async function markWatched(id) {
            await fetch(`/api/tracked/${id}/watched`, { method: 'POST' });
            fetchTracked();
        }

        async function rateItem(id, rating) {
            await fetch(`/api/tracked/${id}/rating`, {
                method: 'POST', headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ rating })
            });
            fetchTracked();
        }

        async function openItemDetails(id, title) {
            document.getElementById('details-title').innerText = title;
            document.getElementById('item-details-modal').classList.add('active');
            document.getElementById('details-trailer').innerHTML = '<div class="flex items-center justify-center h-full text-slate-500 uppercase font-black tracking-widest animate-pulse">Loading Trailer...</div>';
            document.getElementById('details-cast').innerHTML = '';
            document.getElementById('details-recs-list').innerHTML = '';
            
            // Fetch everything in parallel
            const [trailers, credits, tracked] = await Promise.all([
                fetch(`/api/tracked/${id}/trailers`).then(r => r.json()),
                fetch(`/api/tracked/${id}/credits`).then(r => r.json()),
                fetch('/api/tracked').then(r => r.json())
            ]);

            const item = tracked.find(t => t.id === id);
            if(item) {
                document.getElementById('details-overview').innerText = "Plot: " + (item.genres ? `[${item.genres}] ` : '') + "Checking TMDB...";
                // Get fresh details for overview if needed or use tracked info
                document.getElementById('details-genres').innerHTML = (item.genres || '').split(',').map(g => `<span class="text-[9px] bg-sky-500/10 text-sky-400 px-2 py-0.5 rounded-full border border-sky-500/20 font-bold uppercase">${g.trim()}</span>`).join('');
            }

            // Render Trailer
            if(trailers.length > 0) {
                document.getElementById('details-trailer').innerHTML = `<iframe class="w-full h-full" src="https://www.youtube.com/embed/${trailers[0].key}" frameborder="0" allowfullscreen></iframe>`;
            } else {
                document.getElementById('details-trailer').innerHTML = '<div class="flex items-center justify-center h-full text-slate-700 uppercase font-black tracking-widest">No Trailer Available</div>';
            }

            // Render Cast
            document.getElementById('details-cast').innerHTML = credits.cast.slice(0, 5).map(c => `
                <div class="text-center">
                    <img src="${c.profile_path ? `https://image.tmdb.org/t/p/w200${c.profile_path}` : placeholder}" class="w-full aspect-[2/3] object-cover rounded-lg mb-2 border border-white/5 shadow-lg">
                    <div class="text-[10px] font-bold text-slate-200 truncate">${c.name}</div>
                    <div class="text-[8px] text-slate-500 truncate">${c.character}</div>
                </div>
            `).join('');

            // Get Similar Titles (using the global recs API for now or specific recommendations)
            const recsRes = await fetch(`/api/recommendations`);
            const recs = await recsRes.json();
            document.getElementById('details-recs-list').innerHTML = recs.slice(0, 4).map(r => `
                <div class="flex gap-3 items-center p-2 rounded-lg hover:bg-white/5 cursor-pointer border border-white/5" onclick="closeItemDetails(); performGlobalSearchDirect('${r.title || r.name}')">
                    <img src="${r.poster_path ? `https://image.tmdb.org/t/p/w92${r.poster_path}` : placeholder}" class="w-8 h-12 rounded object-cover shadow-md">
                    <div>
                        <div class="text-[10px] font-bold text-slate-200">${r.title || r.name}</div>
                        <div class="text-[8px] text-slate-500 uppercase">${r.media_type}</div>
                    </div>
                </div>
            `).join('');
        }

        async function performGlobalSearchDirect(q) {
            document.getElementById('search-query').value = q;
            performGlobalSearch();
        }

        function closeItemDetails() { 
            document.getElementById('item-details-modal').classList.remove('active');
            document.getElementById('details-trailer').innerHTML = ''; // Stop video
        }

        async function fetchCalendar() {
            const res = await fetch('/api/calendar'); const data = await res.json();
            document.getElementById('tab-calendar').innerHTML = data.length ? data.map(ep => `
                <div class="glass p-4 rounded-xl flex justify-between items-center border border-sky-500/20">
                    <div><div class="font-bold text-sky-400 text-sm">${ep.show_title}</div><div class="text-xs text-slate-300">S${ep.season}E${ep.episode} - ${ep.title}</div></div>
                    <div class="text-[10px] font-black text-slate-500 uppercase">${ep.air_date}</div>
                </div>
            `).join('') : '<div class="text-center text-slate-500 py-20 font-bold text-xs uppercase tracking-widest">No releases in the next 7 days.</div>';
        }

        async function fetchSysInfo() {
            const res = await fetch('/api/sysinfo'); const data = await res.json();
            document.getElementById('sys-cpu').innerText = `CPU: ${data.cpu_usage.toFixed(1)}%`;
            document.getElementById('sys-ram').innerText = `RAM: ${data.memory_used}MB`;
        }

        async function fetchDisks() {
            const res = await fetch('/api/disks'); const data = await res.json();
            document.getElementById('disk-info').innerHTML = data.map(d => `<div class="mb-2"><span>${d.name}: ${d.available}GB free / ${d.total}GB total</span><div class="w-full h-1 bg-slate-800 mt-1"><div class="bg-sky-500 h-full" style="width:${(1 - d.available/d.total)*100}%"></div></div></div>`).join('');
        }

        async function track(id, title, poster, date, type) { await fetch('/api/track', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({id:parseInt(id),title,poster_path:poster,release_date:date,media_type:type||'movie'})}); alert('Tracked!'); }
        async function deleteTracked(id) { if(confirm('Remove?')) { await fetch('/api/tracked/'+id,{method:'DELETE'}); fetchTracked(); } }
        async function clearQueue() { if(confirm('Clear history?')) { await fetch('/api/media/clear', {method:'DELETE'}); fetchQueue(); } }
        async function scanLibrary() { await fetch('/api/scan-library', {method:'POST'}); alert('Scan started!'); }
        async function triggerIngest() { await fetch('/api/ingest', {method:'POST'}); alert('Ingest scan started!'); }
        async function updateApp() { if(confirm('Update?')) { await fetch('/api/update', {method:'POST'}); alert('Updating...'); } }

        function initLogs() {
            const eventSource = new EventSource('/api/logs');
            const logContainer = document.getElementById('tab-logs');
            eventSource.onmessage = (event) => {
                const div = document.createElement('div');
                div.className = 'border-b border-slate-800 pb-1';
                div.innerText = `[${new Date().toLocaleTimeString()}] ${event.data}`;
                logContainer.appendChild(div);
                logContainer.scrollTop = logContainer.scrollHeight;
                if(logContainer.children.length > 500) logContainer.removeChild(logContainer.firstChild);
            };
        }

        showTab('recommendations'); setInterval(fetchSysInfo, 3000); setInterval(checkScanStatus, 5000); setInterval(fetchActivity, 5000); initLogs();
    </script>
</body>
</html>
"#).into_response()
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

#[derive(Serialize)]
struct CalendarEpisode { show_title: String, season: i64, episode: i64, title: String, air_date: String }

async fn get_calendar(State(state): State<AppState>) -> Json<Vec<CalendarEpisode>> {
    let rows = sqlx::query("SELECT s.title as show_title, e.season, e.episode, e.title, e.air_date FROM episodes e JOIN tracked_shows s ON e.show_id = s.id WHERE e.air_date >= date('now') AND e.air_date <= date('now', '+7 days') ORDER BY e.air_date ASC")
        .fetch_all(&state.pool).await.unwrap_or_default();
    use sqlx::Row;
    let items = rows.into_iter().map(|r| CalendarEpisode { show_title: r.get(0), season: r.get(1), episode: r.get(2), title: r.get(3), air_date: r.get(4) }).collect();
    Json(items)
}

#[derive(Deserialize)]
struct TrackRequest { id: u32, title: String, poster_path: Option<String>, release_date: Option<String>, media_type: String }

async fn track_show(State(state): State<AppState>, Json(req): Json<TrackRequest>) -> Json<bool> {
    let mut genres_vec = Vec::new();
    if let Ok(details) = if req.media_type == "tv" { state.tmdb.get_tv_details(req.id).await } else { state.tmdb.get_movie_details(req.id).await } {
        if let Some(gs) = details.genres {
            for g in gs { genres_vec.push(g.name); }
        }
    }
    let genres_str = if genres_vec.is_empty() { None } else { Some(genres_vec.join(",")) };

    match db::insert_tracked_show(&state.pool, &req.title, req.id, &req.media_type, req.poster_path, req.release_date, genres_str).await {
        Ok(_) => {
            // Trigger an immediate scan for this specific title
            let pool = state.pool.clone();
            let title = req.title.clone();
            tokio::spawn(async move {
                let _ = crate::scan_library(pool).await;
                info!("Post-track scan completed for: {}", title);
            });
            Json(true)
        },
        Err(e) => {
            tracing::error!("Failed to track show: {}", e);
            Json(false)
        }
    }
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

async fn stream_logs(State(state): State<AppState>) -> impl IntoResponse {
    let mut rx = state.log_tx.subscribe();
    let stream = async_stream::stream! {
        while let Ok(msg) = rx.recv().await {
            yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(msg));
        }
    };
    axum::response::sse::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

async fn get_profiles(State(state): State<AppState>) -> Json<Vec<QualityProfile>> {
    Json(db::get_all_quality_profiles(&state.pool).await.unwrap_or_default())
}

async fn scan_library(State(state): State<AppState>) -> Json<bool> {
    let pool = state.pool.clone();
    let is_scanning = state.is_scanning.clone();
    tokio::spawn(async move { 
        is_scanning.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = crate::scan_library(pool).await; 
        is_scanning.store(false, std::sync::atomic::Ordering::SeqCst);
    });
    Json(true)
}

async fn get_scan_status(State(state): State<AppState>) -> Json<bool> {
    Json(state.is_scanning.load(std::sync::atomic::Ordering::SeqCst))
}

#[derive(Deserialize)]
struct GenreSearchQuery { genre: String }

async fn search_by_genre(State(state): State<AppState>, Query(params): Query<GenreSearchQuery>) -> Json<Vec<crate::integrations::tmdb::TmdbMedia>> {
    // TMDB genre search
    let genres_movie = state.tmdb.get_genres(false).await.unwrap_or_default();
    let genres_tv = state.tmdb.get_genres(true).await.unwrap_or_default();
    
    let gid = genres_movie.iter().find(|g| g.name.to_lowercase() == params.genre.to_lowercase())
        .or_else(|| genres_tv.iter().find(|g| g.name.to_lowercase() == params.genre.to_lowercase()))
        .map(|g| g.id);

    if let Some(id) = gid {
        let mut results = Vec::new();
        // Just getting movie discover for now as a representative search
        let url = format!("https://api.themoviedb.org/3/discover/movie?api_key={}&with_genres={}", state.tmdb.api_key, id);
        if let Ok(res) = state.tmdb.client.get(&url).send().await {
            let json_res: Result<crate::integrations::tmdb::TmdbSearchResult, _> = res.json().await;
            if let Ok(json) = json_res {
                for mut m in json.results {
                    m.media_type = Some("movie".to_string());
                    results.push(m);
                }
            }
        }
        return Json(results);
    }
    Json(vec![])
}

#[derive(Serialize)]
struct ActivityItem {
    id: String,
    title: String,
    status: String,
    progress: f32,
    media_type: String,
    source: String, // 'tracked' or 'ingest'
}

async fn get_activity(State(state): State<AppState>) -> Json<Vec<ActivityItem>> {
    let mut activity = Vec::new();

    // 1. Get Torrent Downloads
    if let Ok(qbit) = crate::integrations::torrent::QBittorrentClient::new() {
        if qbit.login().await.is_ok() {
            if let Ok(torrents) = qbit.get_torrents().await {
                for t in torrents {
                    activity.push(ActivityItem {
                        id: t.hash.clone(),
                        title: t.name.clone(),
                        status: "Downloading".to_string(),
                        progress: t.progress,
                        media_type: "unknown".to_string(),
                        source: "tracked".to_string(),
                    });
                }
            }
        }
    }

    // 2. Get Media Queue (Ingest folder items)
    let items = sqlx::query_as::<_, MediaItem>("SELECT * FROM media_items WHERE status != 'completed'").fetch_all(&state.pool).await.unwrap_or_default();
    for i in items {
        // Skip if already in torrent list (to avoid double showing)
        if activity.iter().any(|a| i.original_filename.contains(&a.title) || a.title.contains(&i.original_filename)) { continue; }
        
        activity.push(ActivityItem {
            id: i.id.to_string(),
            title: i.title.clone(),
            status: match i.status.as_str() {
                "parsed" => "Matched",
                "summarized" => "Processing",
                _ => "New File",
            }.to_string(),
            progress: 1.0,
            media_type: if i.season.is_some() { "tv" } else { "movie" }.to_string(),
            source: "ingest".to_string(),
        });
    }

    // 3. Get Wanted Episodes/Movies (Tracked but not indexed yet)
    // We only show these if they aren't already matched or downloading
    if let Ok(wanted_eps) = db::get_wanted_episodes(&state.pool).await {
        for (ep, show) in wanted_eps.iter().take(10) {
            let title = format!("{} S{:02}E{:02}", show.title, ep.season, ep.episode);
            if !activity.iter().any(|a| a.title.contains(&show.title) || title.contains(&a.title)) {
                activity.push(ActivityItem {
                    id: format!("ep_{}", ep.id),
                    title,
                    status: "Tracked".to_string(),
                    progress: 0.0,
                    media_type: "tv".to_string(),
                    source: "tracked".to_string(),
                });
            }
        }
    }

    Json(activity)
}

async fn trigger_ingest(State(state): State<AppState>) -> Json<bool> {
    let pool = state.pool.clone();
    let tmdb = state.tmdb.clone();
    let ollama = state.ollama.clone();
    // We need a qbit client here too
    if let Ok(qbit) = crate::integrations::torrent::QBittorrentClient::new() {
        if qbit.login().await.is_ok() {
            let qbit_arc = Arc::new(qbit);
            tokio::spawn(async move {
                let _ = crate::scan_ingest_folder(pool, tmdb, ollama, qbit_arc).await;
            });
            return Json(true);
        }
    }
    Json(false)
}

async fn trigger_update() -> Json<bool> {
    tokio::spawn(async { let _ = std::process::Command::new(std::env::current_exe().unwrap()).arg("update").spawn(); });
    Json(true)
}

#[derive(Deserialize)]
struct InteractiveSearchRequest { query: String }

#[derive(Serialize)]
struct InteractiveSearchResult {
    title: String,
    link: String,
    size: u64,
    seeders: u32,
    indexer: String,
}

async fn interactive_search(State(_state): State<AppState>, Json(req): Json<InteractiveSearchRequest>) -> Json<Vec<InteractiveSearchResult>> {
    let indexer = crate::integrations::indexer::IndexerClient::new().unwrap();
    if let Ok(res) = indexer.search(&req.query).await {
        let results = res.into_iter().map(|item| InteractiveSearchResult {
            title: item.title,
            link: item.link,
            size: item.size,
            seeders: item.seeders,
            indexer: item.indexer,
        }).collect();
        return Json(results);
    }
    Json(vec![])
}

#[derive(Deserialize)]
struct DownloadRequest { link: String, title: String, episode_id: Option<i64>, show_id: Option<i64> }

async fn download_torrent(State(state): State<AppState>, Json(req): Json<DownloadRequest>) -> Json<bool> {
    info!("Manual download requested for: {}", req.title);
    let qbit = crate::integrations::torrent::QBittorrentClient::new().unwrap();
    let _ = qbit.login().await;
    let ing = std::fs::canonicalize("./ingest").unwrap_or_else(|_| std::path::PathBuf::from("./ingest"));
    if qbit.add_torrent_url(&req.link, Some(&ing.to_string_lossy())).await.is_ok() {
        if let Some(eid) = req.episode_id {
            let _ = db::update_episode_status(&state.pool, eid, "downloading").await;
            // Get metadata to store in pending_downloads
            if let Ok(rows) = sqlx::query("SELECT s.tmdb_id, s.media_type, s.id as sid FROM episodes e JOIN tracked_shows s ON e.show_id = s.id WHERE e.id = ?").bind(eid).fetch_all(&state.pool).await {
                if let Some(r) = rows.first() {
                    use sqlx::Row;
                    let _ = db::insert_pending_download(&state.pool, &req.title, Some(r.get::<i64, _>("sid")), Some(eid), r.get::<i64, _>("tmdb_id") as u32, &r.get::<String, _>("media_type")).await;
                }
            }
        } else if let Some(sid) = req.show_id {
            let _ = db::update_tracked_show_status(&state.pool, sid, "downloading").await;
            if let Ok(tracked) = db::get_tracked_shows(&state.pool).await {
                if let Some(s) = tracked.iter().find(|t| t.id == sid) {
                    let _ = db::insert_pending_download(&state.pool, &req.title, Some(sid), None, s.tmdb_id as u32, &s.media_type).await;
                }
            }
        }
        return Json(true);
    }
    Json(false)
}

async fn mark_watched(State(state): State<AppState>, Path(id): Path<i64>) -> Json<bool> {
    let _ = db::update_tracked_show_info(&state.pool, id, Some("watched"), None, None).await;
    Json(true)
}

#[derive(Deserialize)]
struct RateRequest { rating: i64 }

async fn rate_item(State(state): State<AppState>, Path(id): Path<i64>, Json(req): Json<RateRequest>) -> Json<bool> {
    let _ = db::update_tracked_show_info(&state.pool, id, None, None, Some(req.rating)).await;
    Json(true)
}

async fn get_recommendations(State(state): State<AppState>) -> Json<Vec<crate::integrations::tmdb::TmdbMedia>> {
    let mut recs = Vec::new();
    if let Ok(tracked) = db::get_tracked_shows(&state.pool).await {
        // Take up to 3 highly rated or recently updated items to get recommendations for
        let mut seed_items = tracked.clone();
        seed_items.sort_by(|a, b| b.rating.cmp(&a.rating));
        
        for item in seed_items.iter().take(3) {
            if item.media_type == "movie" {
                if let Ok(results) = state.tmdb.get_movie_recommendations(item.tmdb_id as u32).await {
                    for mut m in results {
                        m.media_type = Some("movie".to_string());
                        recs.push(m);
                    }
                }
            } else {
                if let Ok(results) = state.tmdb.get_tv_recommendations(item.tmdb_id as u32).await {
                    for mut m in results {
                        m.media_type = Some("tv".to_string());
                        recs.push(m);
                    }
                }
            }
        }
    }
    
    // De-duplicate by TMDB ID
    let mut seen = std::collections::HashSet::new();
    recs.retain(|m| seen.insert(m.id));
    
    Json(recs.into_iter().take(20).collect())
}

async fn get_preference_chips(State(state): State<AppState>) -> Json<Vec<String>> {
    let mut genre_counts = std::collections::HashMap::new();
    if let Ok(tracked) = db::get_tracked_shows(&state.pool).await {
        for item in tracked {
            if let Some(genres) = item.genres {
                for g in genres.split(',') {
                    *genre_counts.entry(g.trim().to_string()).or_insert(0) += 1;
                }
            }
        }
    }
    
    let mut chips: Vec<_> = genre_counts.into_iter().collect();
    chips.sort_by(|a, b| b.1.cmp(&a.1));
    
    Json(chips.into_iter().take(10).map(|(name, _)| name).collect())
}

async fn get_trailers(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Vec<crate::integrations::tmdb::TmdbVideo>> {
    if let Ok(tracked) = db::get_tracked_shows(&state.pool).await {
        if let Some(s) = tracked.iter().find(|t| t.id == id) {
            if let Ok(videos) = state.tmdb.get_videos(s.tmdb_id as u32, s.media_type == "tv").await {
                let trailers: Vec<_> = videos.into_iter().filter(|v| v.r#type == "Trailer").collect();
                return Json(trailers);
            }
        }
    }
    Json(vec![])
}

async fn get_credits(State(state): State<AppState>, Path(id): Path<i64>) -> Json<crate::integrations::tmdb::TmdbCredits> {
    if let Ok(tracked) = db::get_tracked_shows(&state.pool).await {
        if let Some(s) = tracked.iter().find(|t| t.id == id) {
            if let Ok(credits) = state.tmdb.get_credits(s.tmdb_id as u32, s.media_type == "tv").await {
                return Json(credits);
            }
        }
    }
    Json(crate::integrations::tmdb::TmdbCredits { cast: vec![] })
}
