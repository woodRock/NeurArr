use anyhow::Result;
use axum::{
    extract::{State, Query, Path},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post, delete},
    middleware::{self, Next},
    http::{Request, StatusCode},
    Json, Router,
};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::net::SocketAddr;
use std::sync::{Mutex as StdMutex, Arc};
use tracing::info;
use sysinfo::{System, Disks};

use crate::integrations::tmdb::TmdbClient;
use crate::integrations::torrent::TorrentInfo;
use crate::db::{self, TrackedShow, QualityProfile};

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
        .route("/api/search/semantic", post(semantic_search))
        .route("/api/upcoming", get(get_upcoming))
        .route("/api/calendar", get(get_calendar))
        .route("/api/track", post(track_show))
        .route("/api/tracked", get(get_tracked))
        .route("/api/tracked/{id}", delete(delete_tracked))
        .route("/api/tracked/{id}/status", post(set_tracked_status))
        .route("/api/tracked/{id}/episodes", get(get_episodes))
        .route("/api/tracked/{id}/watched", post(mark_watched))
        .route("/api/tracked/{id}/rating", post(rate_item))
        .route("/api/tracked/{id}/seasons/{season}/status", post(bulk_set_season_status))
        .route("/api/tracked/{id}/trailers", get(get_trailers))
        .route("/api/tracked/{id}/credits", get(get_credits))
        .route("/api/tracked/{id}/subtitles", post(fetch_subtitles_for_tracked))
        .route("/api/external/{type}/{id}/trailers", get(get_external_trailers))
        .route("/api/external/{type}/{id}/credits", get(get_external_credits))
        .route("/api/episodes/{id}/status", post(set_episode_status))
        .route("/api/episodes/{id}/search", post(manual_search_episode))
        .route("/api/recommendations", get(get_recommendations))
        .route("/api/recommendations/vote", post(vote_recommendation))
        .route("/api/next-up", get(get_next_up))
        .route("/api/preferences/chips", get(get_preference_chips))
        .route("/api/interactive-search", post(interactive_search))
        .route("/api/download-torrent", post(download_torrent))
        .route("/api/torrents", get(get_torrents))
        .route("/api/activity", get(get_activity))
        .route("/api/scan-status", get(get_scan_status))
        .route("/api/bot/chat", post(bot_chat))
        .route("/api/sysinfo", get(get_sysinfo))
        .route("/api/disks", get(get_disks))
        .route("/api/logs", get(stream_logs))
        .route("/api/quality-profiles", get(get_profiles))
        .route("/api/settings/config", get(get_config).post(update_config))
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

async fn auth_middleware(jar: CookieJar, req: Request<axum::body::Body>, next: Next) -> Result<Response, StatusCode> {
    if jar.get("auth").is_none() {
        let path = req.uri().path();
        if path == "/login" { return Ok(next.run(req).await); }
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}

async fn login_page() -> Html<&'static str> {
    Html(r#"
<!DOCTYPE html><html><head><title>Login - NeurArr</title><script src="https://cdn.tailwindcss.com"></script></head>
<body class="bg-slate-950 flex items-center justify-center min-h-screen">
    <form action="/login" method="POST" class="bg-slate-900 p-8 rounded-2xl border border-slate-800 w-full max-w-sm">
        <h1 class="text-2xl font-bold text-white mb-6">NeurArr Login</h1>
        <input type="text" name="username" placeholder="Username" class="w-full bg-slate-800 border border-slate-700 rounded-lg p-3 text-white mb-4">
        <input type="password" name="password" placeholder="Password" class="w-full bg-slate-800 border border-slate-700 rounded-lg p-3 text-white mb-6">
        <button type="submit" class="w-full bg-sky-600 text-white font-bold py-3 rounded-lg shadow-lg shadow-sky-900/20">Login</button>
    </form>
</body></html>"#)
}

#[derive(Deserialize)]
struct LoginData { username: String, password: String }

async fn handle_login(State(state): State<AppState>, jar: CookieJar, axum::Form(data): axum::Form<LoginData>) -> impl IntoResponse {
    if let Ok(Some(hash)) = db::get_user_hash(&state.pool, &data.username).await {
        if crate::utils::auth::verify_password(&data.password, &hash) {
            let cookie = Cookie::build(("auth", "true")).path("/").permanent().http_only(true);
            return (jar.add(cookie), Redirect::to("/"));
        }
    }
    (jar, Redirect::to("/login"))
}

async fn dashboard() -> impl IntoResponse {
    Html(r#"
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8"><title>NeurArr Pro</title>
    <script src="https://cdn.tailwindcss.com"></script>
    <style>
        body { background-color: #020617; color: #f8fafc; font-family: sans-serif; overflow-x: hidden; }
        .glass { background: rgba(15, 23, 42, 0.8); backdrop-filter: blur(16px); border: 1px solid rgba(255,255,255,0.05); }
        .active { color: #38bdf8 !important; }
        .hidden { display: none !important; }
        .modal { display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.85); z-index: 100; align-items: center; justify-content: center; backdrop-filter: blur(4px); }
        .modal.active { display: flex; }
        #backdrop-overlay { position: fixed; inset: 0; background-size: cover; background-position: center; opacity: 0; transition: opacity 0.8s ease-in-out; z-index: -1; filter: brightness(0.15) blur(8px); }
        ::-webkit-scrollbar { width: 6px; }
        ::-webkit-scrollbar-track { background: transparent; }
        ::-webkit-scrollbar-thumb { background: #1e293b; border-radius: 10px; }
        ::-webkit-scrollbar-thumb:hover { background: #334155; }
    </style>
</head>
<body class="p-4">
    <div id="backdrop-overlay"></div>

    <nav class="glass sticky top-0 flex gap-8 p-5 mb-10 rounded-2xl items-center z-50">
        <div class="font-black text-2xl mr-4 text-white tracking-tighter flex items-center gap-3">
            <span class="text-sky-500">Neur</span>Arr
            <div id="scan-indicator" class="hidden"><div class="w-2 h-2 bg-sky-500 rounded-full animate-ping"></div></div>
        </div>
        <button onclick="showTab('recommendations')" id="nav-recommendations" class="font-black text-[11px] tracking-[0.2em] uppercase opacity-50 hover:opacity-100 transition-all">For You</button>
        <button onclick="showTab('upcoming')" id="nav-upcoming" class="font-black text-[11px] tracking-[0.2em] uppercase opacity-50 hover:opacity-100 transition-all">Discover</button>
        <button onclick="showTab('tracked')" id="nav-tracked" class="font-black text-[11px] tracking-[0.2em] uppercase opacity-50 hover:opacity-100 transition-all">Collection</button>
        <button onclick="showTab('watchlist')" id="nav-watchlist" class="font-black text-[11px] tracking-[0.2em] uppercase opacity-50 hover:opacity-100 transition-all">Watchlist</button>
        <button onclick="showTab('activity')" id="nav-activity" class="font-black text-[11px] tracking-[0.2em] uppercase opacity-50 hover:opacity-100 transition-all">Activity</button>
        
        <div class="ml-auto flex items-center gap-6">
            <button onclick="toggleBot()" class="bg-purple-600/10 text-purple-400 p-2.5 rounded-xl border border-purple-500/20 hover:bg-purple-600 hover:text-white transition-all">
                <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5" viewBox="0 0 20 20" fill="currentColor"><path fill-rule="evenodd" d="M18 10c0 3.866-3.582 7-8 7a8.841 8.841 0 01-4.083-.98L2 17l1.338-3.123C2.493 12.767 2 11.434 2 10c0-3.866 3.582-7 8-7s8 3.134 8 7zM7 9H5v2h2V9zm8 0h-2v2h2V9zM9 9h2v2H9V9z" clip-rule="evenodd" /></svg>
            </button>
            <button onclick="toggleSettingsMenu()" class="text-slate-500 hover:text-white transition-colors">
                <svg xmlns="http://www.w3.org/2000/svg" class="h-6 w-6" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.065 2.572c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.572 1.065c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.065-2.572c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z" />
                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
                </svg>
            </button>
        </div>
    </nav>

    <div id="settings-menu" class="hidden fixed right-4 top-24 w-72 glass rounded-3xl p-5 z-50 shadow-2xl border border-white/10 animate-in fade-in slide-in-from-top-4 duration-300">
        <div class="flex flex-col gap-3">
            <div class="text-[10px] font-black text-slate-500 uppercase tracking-widest mb-1 ml-4">Utilities</div>
            <button onclick="showTab('calendar')" class="text-left px-4 py-3 hover:bg-white/5 rounded-2xl text-xs font-bold uppercase tracking-wider text-slate-300 hover:text-sky-400 transition-all flex items-center gap-3"><svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M8 7V3m8 4V3m-9 8h10M5 21h14a2 2 0 002-2V7a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z" /></svg> Calendar</button>
            <button onclick="showTab('settings')" class="text-left px-4 py-3 hover:bg-white/5 rounded-2xl text-xs font-bold uppercase tracking-wider text-slate-300 hover:text-sky-400 transition-all flex items-center gap-3"><svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 6V4m0 2a2 2 0 100 4m0-4a2 2 0 110 4m-6 8a2 2 0 100-4m0 4a2 2 0 110-4m0 4v2m0-6V4m6 6v10m6-2a2 2 0 100-4m0 4a2 2 0 110-4m0 4v2m0-6V4" /></svg> System Settings</button>
            <button onclick="showTab('logs')" class="text-left px-4 py-3 hover:bg-white/5 rounded-2xl text-xs font-bold uppercase tracking-wider text-slate-300 hover:text-sky-400 transition-all flex items-center gap-3"><svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z" /></svg> Live Logs</button>
            <hr class="border-white/5 my-2">
            <button onclick="updateApp()" class="text-left px-4 py-3 hover:bg-amber-500/10 rounded-2xl text-xs font-bold uppercase tracking-wider text-amber-500 flex items-center gap-3"><svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.001 0 01-15.357-2m15.357 2H15" /></svg> Update App</button>
        </div>
    </div>

    <div class="max-w-7xl mx-auto">
        <div class="flex flex-col md:flex-row gap-4 mb-16">
            <input type="text" id="search-query" placeholder="Search title, or describe what you're in the mood for..." class="flex-grow bg-white/5 border border-white/10 rounded-3xl px-8 py-5 outline-none focus:ring-2 focus:ring-sky-500/50 text-lg font-medium placeholder:text-slate-600 transition-all shadow-2xl">
            <div class="flex gap-2">
                <button onclick="performGlobalSearch()" class="bg-sky-600 px-8 py-5 rounded-3xl font-black text-xs tracking-widest uppercase hover:bg-sky-500 transition-all shadow-lg shadow-sky-900/20">SEARCH</button>
                <button onclick="performSemanticSearch()" class="bg-purple-600 px-8 py-5 rounded-3xl font-black text-xs tracking-widest uppercase hover:bg-purple-500 transition-all shadow-lg shadow-purple-900/20 flex items-center gap-3">
                    <svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" viewBox="0 0 20 20" fill="currentColor"><path d="M13 6a3 3 0 11-6 0 3 3 0 016 0zM18 8a2 2 0 11-4 0 2 2 0 014 0zM14 15a4 4 0 00-8 0v3h8v-3zM6 8a2 2 0 11-4 0 2 2 0 014 0zM16 18v-3a5.972 5.972 0 00-.75-2.906A3.005 3.005 0 0119 15v3h-3zM4.75 12.094A5.973 5.973 0 004 15v3H1v-3a3 3 0 013.75-2.906z" /></svg>
                    AI DISCOVER
                </button>
            </div>
        </div>

        <div id="tab-recommendations" class="space-y-16">
            <div id="next-up-section" class="hidden space-y-8">
                <h2 class="text-3xl font-black text-white uppercase tracking-tighter">Continue Watching</h2>
                <div id="next-up-results" class="grid grid-cols-1 md:grid-cols-3 gap-8"></div>
            </div>

            <div class="space-y-8">
                <div class="flex justify-between items-end border-b border-white/5 pb-6">
                    <h2 class="text-3xl font-black text-white uppercase tracking-tighter">For You</h2>
                    <div id="preference-chips" class="flex gap-2 flex-wrap max-w-xl justify-end"></div>
                </div>
                <div id="recommendation-results" class="grid grid-cols-2 md:grid-cols-5 gap-8"></div>
            </div>
        </div>

        <div id="tab-upcoming" class="hidden space-y-10">
            <h2 class="text-3xl font-black text-white uppercase tracking-tighter border-b border-white/5 pb-6">Discover</h2>
            <div id="upcoming-results" class="grid grid-cols-2 md:grid-cols-5 gap-8"></div>
        </div>

        <div id="tab-watchlist" class="hidden grid grid-cols-2 md:grid-cols-5 gap-8"></div>
        <div id="tab-tracked" class="hidden grid grid-cols-2 md:grid-cols-5 gap-8"></div>
        
        <div id="tab-activity" class="hidden space-y-6">
            <h2 class="text-3xl font-black text-white uppercase tracking-tighter mb-8 border-b border-white/5 pb-6">Lifecycle Activity</h2>
            <div id="activity-list" class="space-y-4"></div>
        </div>

        <div id="tab-calendar" class="hidden space-y-6"></div>
        <div id="tab-search" class="hidden grid grid-cols-2 md:grid-cols-5 gap-8"></div>
        <div id="tab-logs" class="hidden glass p-8 rounded-3xl font-mono text-[11px] h-[75vh] overflow-y-auto space-y-1 border border-white/5" id="log-container"></div>
        
        <div id="tab-settings" class="hidden glass p-10 rounded-3xl border border-white/5 space-y-10">
            <h2 class="font-black text-3xl uppercase tracking-tighter text-white">System Settings</h2>
            <div id="config-form" class="grid grid-cols-1 md:grid-cols-2 gap-6"></div>
            <button onclick="saveConfig()" class="bg-emerald-600 px-10 py-4 rounded-2xl font-black text-xs tracking-widest uppercase hover:bg-emerald-500 transition-all shadow-lg shadow-emerald-900/20">Save Configuration</button>
            <div class="border-t border-white/10 pt-10 space-y-6" id="disk-info"></div>
            <div class="mt-10 border-t border-white/10 pt-10 flex flex-wrap gap-4">
                <button onclick="scanLibrary()" class="bg-white/5 border border-white/10 px-6 py-3 rounded-xl font-black text-[10px] uppercase tracking-widest hover:bg-sky-600 hover:text-white transition-all">Full Library Sync</button>
                <button onclick="triggerIngest()" class="bg-white/5 border border-white/10 px-6 py-3 rounded-xl font-black text-[10px] uppercase tracking-widest hover:bg-amber-600 hover:text-white transition-all">Ingest Folder Scan</button>
                <button onclick="clearQueue()" class="bg-white/5 border border-white/10 px-6 py-3 rounded-xl font-black text-[10px] uppercase tracking-widest hover:bg-rose-600 hover:text-white transition-all">Clear Activity History</button>
            </div>
        </div>
    </div>

    <!-- Modals -->
    <div id="item-details-modal" class="modal"><div class="glass p-10 rounded-[2.5rem] w-full max-w-6xl max-h-[90vh] overflow-y-auto relative shadow-2xl">
        <div class="flex justify-between items-start mb-8">
            <div>
                <h2 id="details-title" class="text-4xl font-black text-white uppercase tracking-tighter"></h2>
                <div id="details-genres" class="flex gap-2 mt-3"></div>
                <button id="details-subtitles-btn" class="hidden mt-6 bg-amber-600/10 text-amber-500 px-4 py-2 rounded-xl text-[10px] font-black uppercase tracking-widest border border-amber-500/20 hover:bg-amber-600 hover:text-white transition-all" onclick="downloadSubtitlesForCurrent()">Fetch Subtitles</button>
            </div>
            <button onclick="closeItemDetails()" class="text-slate-500 hover:text-white transition-colors bg-white/5 p-3 rounded-full"><svg xmlns="http://www.w3.org/2000/svg" class="h-6 w-6" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="3" d="M6 18L18 6M6 6l12 12" /></svg></button>
        </div>
        <div class="grid grid-cols-1 lg:grid-cols-3 gap-12">
            <div class="lg:col-span-2 space-y-10">
                <div id="details-trailer" class="aspect-video bg-black rounded-[2rem] overflow-hidden shadow-2xl border border-white/5 ring-1 ring-white/10"></div>
                <div>
                    <h3 class="text-sky-500 font-black text-xs uppercase mb-6 tracking-[0.2em] ml-2">Main Cast</h3>
                    <div id="details-cast" class="grid grid-cols-3 md:grid-cols-5 gap-6"></div>
                </div>
            </div>
            <div class="space-y-10">
                <div id="details-overview" class="text-slate-300 text-base leading-relaxed font-medium border-l-4 border-sky-500/30 pl-6 py-2"></div>
                <div id="details-recommendations" class="space-y-6">
                    <h3 class="text-sky-500 font-black text-xs uppercase tracking-[0.2em] ml-2">Similar Titles</h3>
                    <div id="details-recs-list" class="space-y-3"></div>
                </div>
            </div>
        </div>
    </div></div>

    <div id="episodes-modal" class="modal"><div class="glass p-10 rounded-[2.5rem] w-full max-w-4xl max-h-[85vh] overflow-y-auto shadow-2xl border border-white/10">
        <div class="flex justify-between items-center mb-8">
            <h2 id="episode-modal-title" class="text-3xl font-black text-white uppercase tracking-tighter"></h2>
            <button onclick="closeEpisodeModal()" class="text-slate-500 hover:text-white transition-colors bg-white/5 p-3 rounded-full"><svg xmlns="http://www.w3.org/2000/svg" class="h-6 w-6" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="3" d="M6 18L18 6M6 6l12 12" /></svg></button>
        </div>
        <div id="episodes-list" class="space-y-4"></div>
    </div></div>

    <div id="interactive-modal" class="modal"><div class="glass p-10 rounded-[2.5rem] w-full max-w-5xl max-h-[85vh] overflow-y-auto shadow-2xl border border-white/10">
        <div class="flex justify-between items-center mb-8">
            <h2 id="interactive-modal-title" class="text-2xl font-black text-sky-400 uppercase tracking-tighter">Manual Selection</h2>
            <button onclick="closeInteractiveModal()" class="text-slate-500 hover:text-white transition-colors bg-white/5 p-3 rounded-full"><svg xmlns="http://www.w3.org/2000/svg" class="h-6 w-6" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="3" d="M6 18L18 6M6 6l12 12" /></svg></button>
        </div>
        <div class="flex gap-4 mb-8">
            <input type="text" id="interactive-search-input" class="flex-grow bg-white/5 border border-white/10 rounded-2xl px-6 py-4 outline-none focus:ring-2 focus:ring-sky-500/50 font-bold">
            <button onclick="performInteractiveSearch()" class="bg-sky-600 px-10 py-4 rounded-2xl font-black text-xs tracking-widest uppercase hover:bg-sky-500 transition-all">SEARCH</button>
        </div>
        <div class="overflow-x-auto">
            <table class="w-full text-left border-separate border-spacing-y-2">
                <thead><tr class="text-[10px] font-black uppercase text-slate-500 tracking-widest"><th class="px-4">Title</th><th>Size</th><th class="text-center">Seeders</th><th>Indexer</th><th class="text-right pr-4">Action</th></tr></thead>
                <tbody id="interactive-results" class="text-xs"></tbody>
            </table>
        </div>
    </div></div>

    <div id="bot-modal" class="modal"><div class="glass p-0 rounded-3xl w-full max-w-lg h-[600px] flex flex-col overflow-hidden shadow-2xl">
        <div class="bg-purple-600/20 p-6 flex justify-between items-center border-b border-purple-500/20">
            <div class="flex items-center gap-3">
                <div class="w-8 h-8 bg-purple-500 rounded-full flex items-center justify-center font-black text-white text-xs shadow-lg shadow-purple-500/20">NB</div>
                <h2 class="font-black text-white uppercase tracking-widest text-sm">NeurArr Assistant</h2>
            </div>
            <button onclick="toggleBot()" class="text-slate-400 hover:text-white"><svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5" viewBox="0 0 20 20" fill="currentColor"><path fill-rule="evenodd" d="M4.293 4.293a1 1 0 011.414 0L10 8.586l4.293-4.293a1 1 0 111.414 1.414L11.414 10l4.293 4.293a1 1 0 01-1.414 1.414L10 11.414l-4.293 4.293a1 1 0 01-1.414-1.414L8.586 10 4.293 5.707a1 1 0 010-1.414z" clip-rule="evenodd" /></svg></button>
        </div>
        <div id="bot-messages" class="flex-grow overflow-y-auto p-6 space-y-4 bg-black/20">
            <div class="flex gap-3">
                <div class="w-6 h-6 bg-purple-500 rounded-full flex-shrink-0 flex items-center justify-center text-[8px] font-black">NB</div>
                <div class="bg-white/5 border border-white/5 p-4 rounded-2xl rounded-tl-none text-xs text-slate-300 leading-relaxed max-w-[85%]">
                    Hi! I'm your NeurArr Assistant. I can help you find new movies or shows based on your collection. What are you in the mood for?
                </div>
            </div>
        </div>
        <div class="p-4 bg-white/5 border-t border-white/5 flex gap-2">
            <input type="text" id="bot-input" placeholder="Ask me anything..." class="flex-grow bg-white/5 border border-white/10 rounded-xl px-4 py-3 text-sm outline-none focus:border-purple-500 transition-all" onkeypress="if(event.key==='Enter') sendBotMessage()">
            <button onclick="sendBotMessage()" class="bg-purple-600 p-3 rounded-xl hover:bg-purple-500 transition-all"><svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5" viewBox="0 0 20 20" fill="currentColor"><path d="M10.894 2.553a1 1 0 00-1.788 0l-7 14a1 1 0 001.169 1.409l5-1.429A1 1 0 009 15.571V11a1 1 0 112 0v4.571a1 1 0 00.725.962l5 1.428a1 1 0 001.17-1.408l-7-14z" /></svg></button>
        </div>
    </div></div>

    <script>
        let currentSearchEpisodeId = null; let currentSearchShowId = null; let currentDetailsId = null;
        const placeholder = 'data:image/svg+xml;base64,PHN2ZyB3aWR0aD0iMjAwIiBoZWlnaHQ9IjMwMCIgeG1sbnM9Imh0dHA6Ly93d3cudzMub3JnLzIwMDAvc3ZnIj48cmVjdCB3aWR0aD0iMTAwJSIgaGVpZ2h0PSIxMDAlIiBmaWxsPSIjMWUyOTNiIi8+PHRleHQgeD0iNTAlIiB5PSI1MCUiIGZpbGw9IiM0NzU1NjkiIGZvbnQtc2l6ZT0iMTQiIGZvbnQtZmFtaWx5PSJzYW5zLXNlcmlmIiBkeT0iLjNlbSIgdGV4dC1hbmNob3I9Im1pZGRsZSI+Tk8gUE9TVEVSPC90ZXh0Pjwvc3ZnPg==';

        function toggleSettingsMenu() { document.getElementById('settings-menu').classList.toggle('hidden'); }
        function toggleBot() { document.getElementById('bot-modal').classList.toggle('active'); }

        function showTab(tab) {
            ['tracked', 'watchlist', 'upcoming', 'settings', 'search', 'calendar', 'recommendations', 'activity', 'logs'].forEach(t => {
                const el = document.getElementById('tab-' + t);
                if (el) el.classList.toggle('hidden', t !== tab);
                const nav = document.getElementById('nav-' + t);
                if (nav) { nav.classList.toggle('active', t === tab); nav.style.opacity = t === tab ? '1' : '0.5'; }
            });
            document.getElementById('settings-menu').classList.add('hidden');
            if(tab === 'tracked' || tab === 'watchlist') fetchTracked(); 
            if(tab === 'upcoming') { fetchUpcoming(); fetchChips(); }
            if(tab === 'recommendations') { fetchRecommendations(); fetchNextUp(); fetchChips(); }
            if(tab === 'activity') fetchActivity();
            if(tab === 'settings') { fetchDisks(); fetchConfig(); }
            if(tab === 'calendar') fetchCalendar();
        }

        async function fetchTracked() {
            const res = await fetch('/api/tracked'); const allData = await res.json();
            const watchlistData = allData.filter(i => i.status === 'watchlist');
            const collectionData = allData.filter(i => i.status !== 'watchlist');
            const renderCard = (item) => `
                <div class="glass rounded-[2rem] overflow-hidden group border border-white/5 cursor-pointer shadow-2xl hover:border-sky-500/30 transition-all duration-500" onclick="openItemDetails(${item.id}, '${item.title.replace(/'/g, "\\'")}')">
                    <div class="relative overflow-hidden">
                        <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w500${item.poster_path}` : placeholder}" class="h-72 w-full object-cover group-hover:scale-110 transition-transform duration-700">
                        <div class="absolute inset-0 bg-black/70 opacity-0 group-hover:opacity-100 flex flex-col items-center justify-center transition-all gap-2" onclick="event.stopPropagation()">
                            <button onclick="deleteTracked(${item.id})" class="bg-rose-600/20 text-rose-500 border border-rose-500/20 px-6 py-2 rounded-xl text-[10px] font-black uppercase tracking-widest hover:bg-rose-600 hover:text-white transition-all">Remove</button>
                            ${item.status === 'watchlist' ? `<button onclick="markWanted(${item.id})" class="bg-sky-600 px-6 py-2 rounded-xl text-[10px] font-black uppercase tracking-widest shadow-lg shadow-sky-900/40">Download</button>` : ''}
                            ${item.status !== 'watched' && item.status !== 'watchlist' ? `<button onclick="markWatched(${item.id})" class="bg-emerald-600/20 text-emerald-500 border border-emerald-500/20 px-6 py-2 rounded-xl text-[10px] font-black uppercase tracking-widest hover:bg-emerald-600 hover:text-white transition-all">Watched</button>` : ''}
                        </div>
                    </div>
                    <div class="p-5">
                        <div class="flex justify-between items-start gap-2 mb-2">
                            <div class="font-black text-xs uppercase tracking-tighter truncate text-slate-200">${item.title}</div>
                            ${item.status !== 'watchlist' ? `
                            <button onclick="event.stopPropagation(); openInteractiveSearch('${item.title.replace(/'/g, "\\'")}', null, ${item.id}, '', '${item.year || ''}')" class="text-amber-500 hover:text-amber-400"><svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="3" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" /></svg></button>` : ''}
                        </div>
                        <div class="flex items-center gap-1 mb-3">${[1,2,3,4,5].map(i => `<span onclick="event.stopPropagation(); rateItem(${item.id}, ${i})" class="cursor-pointer text-[10px] ${i <= item.rating ? 'text-amber-400' : 'text-slate-700'}">★</span>`).join('')}</div>
                        ${item.status === 'downloading' ? '<div class="text-[8px] font-black bg-sky-500/10 text-sky-400 px-2 py-1 rounded-lg inline-block uppercase animate-pulse border border-sky-500/20">Downloading</div>' : ''}
                        ${item.status === 'watched' ? '<div class="text-[8px] font-black bg-emerald-500/10 text-emerald-400 px-2 py-1 rounded-lg inline-block uppercase border border-emerald-500/20">Watched</div>' : ''}
                        ${item.status === 'wanted' ? '<div class="text-[8px] font-black bg-slate-800 text-slate-500 px-2 py-1 rounded-lg inline-block uppercase">Monitoring</div>' : ''}
                        ${item.media_type === 'tv' && item.status !== 'watchlist' ? `<button onclick="event.stopPropagation(); openEpisodeModal(${item.id}, '${item.title.replace(/'/g, "\\'")}')" class="text-[9px] text-sky-500 mt-3 font-black uppercase hover:underline block tracking-widest">Manage Seasons</button>` : ''}
                    </div>
                </div>`;
            document.getElementById('tab-watchlist').innerHTML = watchlistData.length ? watchlistData.map(renderCard).join('') : '<div class="col-span-5 text-center text-slate-600 py-32 font-black uppercase tracking-[0.3em] text-xs">Watchlist Empty</div>';
            document.getElementById('tab-tracked').innerHTML = collectionData.length ? collectionData.map(renderCard).join('') : '<div class="col-span-5 text-center text-slate-600 py-32 font-black uppercase tracking-[0.3em] text-xs">Collection Empty</div>';
        }

        async function fetchUpcoming() {
            const res = await fetch('/api/upcoming'); const data = await res.json();
            document.getElementById('upcoming-results').innerHTML = data.map(item => `
                <div class="glass rounded-3xl overflow-hidden p-5 text-center cursor-pointer group hover:border-sky-500/50 transition-all duration-500" onclick="openItemDetailsExternal('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.media_type}')">
                    <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w500${item.poster_path}` : placeholder}" class="h-56 w-full object-cover rounded-2xl shadow-2xl group-hover:scale-105 transition-transform duration-500">
                    <div class="mt-4 text-[11px] font-black uppercase tracking-tighter truncate text-slate-200">${item.title || item.name}</div>
                    <div class="flex gap-2 mt-4">
                        <button onclick="event.stopPropagation(); track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}', 'watchlist')" class="flex-grow bg-white/5 text-slate-400 py-2.5 rounded-xl font-black text-[8px] hover:bg-white/10 hover:text-white transition-all uppercase tracking-widest">Watchlist</button>
                        <button onclick="event.stopPropagation(); track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}', 'wanted')" class="flex-grow bg-sky-600/10 text-sky-400 py-2.5 rounded-xl font-black text-[8px] hover:bg-sky-600 hover:text-white transition-all uppercase tracking-widest">Download</button>
                    </div>
                </div>`).join('');
        }

        async function fetchRecommendations() {
            const res = await fetch('/api/recommendations'); const data = await res.json();
            document.getElementById('recommendation-results').innerHTML = data.length ? data.map(item => `
                <div class="glass rounded-3xl overflow-hidden p-5 text-center cursor-pointer group hover:border-purple-500/50 transition-all duration-500 relative" onclick="openItemDetailsExternal('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.media_type}')">
                    <div class="absolute top-7 right-7 flex flex-col gap-2 z-10 opacity-0 group-hover:opacity-100 transition-opacity duration-300">
                        <button onclick="event.stopPropagation(); vote(${item.id}, '${item.media_type}', 1)" class="p-2 bg-emerald-600/80 hover:bg-emerald-500 rounded-full text-white shadow-lg backdrop-blur-md transition-all scale-90 hover:scale-110">
                            <svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" viewBox="0 0 20 20" fill="currentColor"><path d="M2 10.5a1.5 1.5 0 113 0v6a1.5 1.5 0 01-3 0v-6zM6 10.333v5.43a2 2 0 001.106 1.79l.05.025A4 4 0 008.943 18h5.416a2 2 0 001.962-1.608l1.2-6A2 2 0 0015.56 8H12V4a2 2 0 00-2-2 1 1 0 00-1 1v.667a4 4 0 01-.8 2.4L6.8 10.2a1 1 0 00-.8.133z" /></svg>
                        </button>
                        <button onclick="event.stopPropagation(); vote(${item.id}, '${item.media_type}', -1)" class="p-2 bg-rose-600/80 hover:bg-rose-500 rounded-full text-white shadow-lg backdrop-blur-md transition-all scale-90 hover:scale-110">
                            <svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" viewBox="0 0 20 20" fill="currentColor"><path d="M18 9.5a1.5 1.5 0 11-3 0v-6a1.5 1.5 0 013 0v6zM14 9.667v-5.43a2 2 0 00-1.106-1.79l-.05-.025A4 4 0 0011.057 2H5.64a2 2 0 00-1.962 1.608l-1.2 6A2 2 0 004.44 12H8v4a2 2 0 002 2 1 1 0 001-1v-.667a4 4 0 01.8-2.4l1.4-1.867a1 1 0 00.8-.133z" /></svg>
                        </button>
                    </div>
                    <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w500${item.poster_path}` : placeholder}" class="h-56 w-full object-cover rounded-2xl shadow-2xl group-hover:scale-105 transition-transform duration-500">
                    <div class="mt-4 text-[11px] font-black uppercase tracking-tighter truncate text-slate-200">${item.title || item.name}</div>
                    <div class="flex gap-2 mt-4">
                        <button onclick="event.stopPropagation(); track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}', 'watchlist')" class="flex-grow bg-white/5 text-slate-400 py-2.5 rounded-xl font-black text-[8px] hover:bg-white/10 hover:text-white transition-all uppercase tracking-widest">Watchlist</button>
                        <button onclick="event.stopPropagation(); track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}', 'wanted')" class="flex-grow bg-sky-600/10 text-sky-400 py-2.5 rounded-xl font-black text-[8px] hover:bg-sky-600 hover:text-white transition-all uppercase tracking-widest">Download</button>
                    </div>
                </div>`).join('') : '<div class="col-span-5 text-center text-slate-600 py-32 font-black uppercase tracking-[0.3em] text-xs">Rate shows to trigger recommendations</div>';
        }
        async function vote(tmdbId, mediaType, vote) {
            await fetch('/api/recommendations/vote', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ tmdb_id: tmdbId, media_type: mediaType, vote })
            });
            fetchRecommendations();
        }

        async function fetchNextUp() {
            const res = await fetch('/api/next-up'); const data = await res.json();
            const section = document.getElementById('next-up-section');
            if(data.length > 0) {
                section.classList.remove('hidden');
                document.getElementById('next-up-results').innerHTML = data.map(item => `
                    <div class="glass p-5 rounded-3xl flex gap-5 items-center border border-white/5 hover:border-sky-500/40 transition-all duration-500 cursor-pointer group shadow-2xl" onclick="openEpisodeModal(${item.show_id}, '${item.show_title.replace(/'/g, "\\'")}')">
                        <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w200${item.poster_path}` : placeholder}" class="w-20 h-28 object-cover rounded-2xl shadow-2xl group-hover:scale-105 transition-transform">
                        <div class="flex-grow overflow-hidden">
                            <div class="text-[9px] font-black text-sky-500 uppercase tracking-[0.2em] mb-2">Continue Watching</div>
                            <div class="font-black text-white text-base uppercase tracking-tighter truncate">${item.show_title}</div>
                            <div class="text-[11px] text-slate-400 mt-1 font-bold">S${item.season}E${item.episode} - ${item.title || 'TBA'}</div>
                        </div>
                    </div>`).join('');
            } else { section.classList.add('hidden'); }
        }

        async function fetchActivity() {
            const res = await fetch('/api/activity'); const data = await res.json();
            document.getElementById('activity-list').innerHTML = data.length ? data.map(item => `
                <div class="glass p-5 rounded-[2rem] flex items-center gap-8 border border-white/5 group hover:border-sky-500/20 transition-all duration-500 shadow-xl">
                    <div class="w-1.5 h-14 rounded-full ${item.status === 'Downloading' ? 'bg-sky-500 animate-pulse' : 'bg-slate-800'}"></div>
                    <div class="flex-grow">
                        <div class="flex justify-between items-end mb-3">
                            <span class="font-black text-sm text-white uppercase tracking-tight">${item.title}</span>
                            <div class="flex items-center gap-4">
                                <span class="text-[9px] font-black uppercase bg-white/5 text-slate-500 px-3 py-1 rounded-full border border-white/5">${item.source}</span>
                                <span class="text-[10px] font-black uppercase text-sky-500 tracking-[0.2em]">${item.status}</span>
                            </div>
                        </div>
                        <div class="w-full h-1.5 bg-white/5 rounded-full overflow-hidden shadow-inner">
                            <div class="h-full bg-sky-500 transition-all duration-1000 shadow-[0_0_10px_rgba(56,189,248,0.5)]" style="width: ${item.progress * 100}%"></div>
                        </div>
                    </div>
                    <div class="text-xs font-black font-mono text-slate-500 w-12 text-right">${(item.progress * 100).toFixed(0)}%</div>
                </div>`).join('') : '<div class="text-center py-40 text-slate-700 font-black uppercase tracking-[0.4em] text-xs">Operational Silence</div>';
        }

        async function checkScanStatus() { const res = await fetch('/api/scan-status'); const isScanning = await res.json(); document.getElementById('scan-indicator').classList.toggle('hidden', !isScanning); }
        async function fetchChips() { const res = await fetch('/api/preferences/chips'); const data = await res.json(); document.getElementById('preference-chips').innerHTML = data.map(chip => `<button onclick="performGenreSearch('${chip}')" class="px-4 py-2 rounded-full bg-white/5 border border-white/10 text-slate-400 text-[9px] font-black hover:bg-sky-600 hover:text-white hover:border-sky-600 transition-all uppercase tracking-widest shadow-lg hover:scale-105">${chip}</button>`).join(''); }
        async function performGenreSearch(genre) { showTab('search'); document.getElementById('tab-search').innerHTML = '<div class="col-span-5 text-center py-40 text-sky-500 uppercase font-black animate-pulse tracking-[0.3em]">Discovering '+genre+'...</div>'; const res = await fetch('/api/search/genre?genre=' + encodeURIComponent(genre)); const data = await res.json(); renderSearchResults(data); }
        function renderSearchResults(data) { document.getElementById('tab-search').innerHTML = data.map(item => `<div class="glass rounded-[2rem] overflow-hidden p-5 border border-white/5 group hover:border-sky-500/50 transition-all duration-500 cursor-pointer" onclick="openItemDetailsExternal('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.media_type}')"><div class="relative overflow-hidden rounded-2xl shadow-2xl"><img src="${item.poster_path ? `https://image.tmdb.org/t/p/w500${item.poster_path}` : placeholder}" class="h-64 w-full object-cover group-hover:scale-110 transition-transform duration-700" onerror="this.src='${placeholder}'"></div><div class="mt-5 text-[11px] font-black uppercase tracking-tighter truncate text-white">${item.title || item.name}</div><div class="text-[9px] text-slate-500 mb-5 font-bold uppercase tracking-widest">${item.release_date || item.first_air_date || 'Unknown'}</div><div class="flex gap-2"><button onclick="event.stopPropagation(); track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}', 'watchlist')" class="flex-grow bg-white/5 text-slate-400 py-3 rounded-xl font-black text-[8px] hover:bg-white/10 hover:text-white transition-all uppercase tracking-widest">Watchlist</button><button onclick="event.stopPropagation(); track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}', 'wanted')" class="flex-grow bg-sky-600/10 text-sky-400 py-3 rounded-xl font-black text-[8px] hover:bg-sky-600 hover:text-white transition-all uppercase tracking-widest">Download</button></div></div>`).join(''); }
        async function performSemanticSearch() { const prompt = document.getElementById('search-query').value; if(!prompt) return; showTab('search'); document.getElementById('tab-search').innerHTML = '<div class="col-span-5 text-center py-40 text-purple-400 uppercase font-black animate-pulse tracking-[0.3em] flex flex-col items-center gap-6"><svg class="w-16 h-16 animate-spin" fill="none" viewBox="0 0 24 24"><circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle><path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path></svg>Neural Discovery in Progress...</div>'; const res = await fetch('/api/search/semantic', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ prompt }) }); const data = await res.json(); renderSearchResults(data); }
        async function performGlobalSearch() { const query = document.getElementById('search-query').value; if(!query) return; showTab('search'); document.getElementById('tab-search').innerHTML = '<div class="col-span-5 text-center py-40 text-slate-600 uppercase font-black animate-pulse tracking-[0.3em]">Searching Index...</div>'; const res = await fetch('/api/search?q=' + encodeURIComponent(query)); const data = await res.json(); renderSearchResults(data); }
        async function openItemDetails(id, title) { currentDetailsId = id; document.getElementById('item-details-modal').classList.add('active'); document.getElementById('details-subtitles-btn').classList.remove('hidden'); const res = await fetch('/api/tracked'); const data = await res.json(); const item = data.find(t => t.id === id); if(item) { if(item.backdrop_path) { const overlay = document.getElementById('backdrop-overlay'); overlay.style.backgroundImage = `url(https://image.tmdb.org/t/p/original${item.backdrop_path})`; overlay.style.opacity = "1"; } document.getElementById('details-title').innerText = title + (item.year ? ` (${item.year})` : ''); await loadDetails(`/api/tracked/${id}/trailers`, `/api/tracked/${id}/credits`, item); } }
        async function openItemDetailsExternal(tmdbId, title, type) { currentDetailsId = null; document.getElementById('details-title').innerText = title; document.getElementById('item-details-modal').classList.add('active'); document.getElementById('details-subtitles-btn').classList.add('hidden'); await loadDetails(`/api/external/${type}/${tmdbId}/trailers`, `/api/external/${type}/${tmdbId}/credits`, { genres: 'Loading...', overview: 'Fetching from TMDB...' }); }
        async function loadDetails(trailersUrl, creditsUrl, item) { document.getElementById('details-trailer').innerHTML = '<div class="flex items-center justify-center h-full text-slate-700 uppercase font-black tracking-widest animate-pulse">Loading Trailer...</div>'; document.getElementById('details-cast').innerHTML = ''; document.getElementById('details-recs-list').innerHTML = ''; const [trailers, credits] = await Promise.all([ fetch(trailersUrl).then(r => r.json()), fetch(creditsUrl).then(r => r.json()) ]); if(item) { document.getElementById('details-overview').innerText = item.overview || "No plot summary available."; document.getElementById('details-genres').innerHTML = (item.genres || '').split(',').map(g => `<span class="text-[9px] bg-sky-500/10 text-sky-400 px-3 py-1 rounded-full border border-white/5 font-black uppercase tracking-widest">${g.trim()}</span>`).join(''); } if(trailers.length > 0) { document.getElementById('details-trailer').innerHTML = `<iframe class="w-full h-full" src="https://www.youtube.com/embed/${trailers[0].key}" frameborder="0" allowfullscreen></iframe>`; } else { document.getElementById('details-trailer').innerHTML = '<div class="flex items-center justify-center h-full text-slate-800 uppercase font-black tracking-widest">Cinema Trailer Unavailable</div>'; } document.getElementById('details-cast').innerHTML = credits.cast.slice(0, 5).map(c => `<div class="text-center"><img src="${c.profile_path ? `https://image.tmdb.org/t/p/w200${c.profile_path}` : placeholder}" class="w-full aspect-[2/3] object-cover rounded-2xl mb-3 border border-white/5 shadow-2xl grayscale hover:grayscale-0 transition-all duration-500"><div class="text-[10px] font-black text-slate-200 truncate uppercase tracking-tighter">${c.name}</div><div class="text-[8px] font-bold text-slate-500 truncate uppercase tracking-widest mt-1">${c.character}</div></div>`).join(''); const recsRes = await fetch(`/api/recommendations`); const recs = await recsRes.json(); document.getElementById('details-recs-list').innerHTML = recs.slice(0, 4).map(r => `<div class="flex gap-4 items-center p-3 rounded-2xl hover:bg-white/5 cursor-pointer border border-white/5 transition-all" onclick="closeItemDetails(); performGlobalSearchDirect('${r.title || r.name}')"><img src="${r.poster_path ? `https://image.tmdb.org/t/p/w92${r.poster_path}` : placeholder}" class="w-10 h-14 rounded-xl object-cover shadow-xl"><div class="overflow-hidden"><div class="text-[10px] font-black text-white uppercase tracking-tighter truncate">${r.title || r.name}</div><div class="text-[8px] text-slate-500 uppercase tracking-widest mt-1 font-bold">${r.media_type}</div></div></div>`).join(''); }
        function closeItemDetails() { document.getElementById('item-details-modal').classList.remove('active'); document.getElementById('details-trailer').innerHTML = ''; document.getElementById('backdrop-overlay').style.opacity = "0"; }
        async function sendBotMessage() { const input = document.getElementById('bot-input'); const message = input.value.trim(); if(!message) return; const container = document.getElementById('bot-messages'); container.innerHTML += `<div class="flex justify-end"><div class="bg-sky-600 text-white p-4 rounded-3xl rounded-tr-none text-xs font-medium leading-relaxed max-w-[85%] shadow-2xl">${message}</div></div>`; input.value = ''; container.scrollTop = container.scrollHeight; const res = await fetch('/api/bot/chat', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ message }) }); const reply = await res.json(); container.innerHTML += `<div class="flex gap-3"><div class="w-8 h-8 bg-purple-500 rounded-full flex-shrink-0 flex items-center justify-center text-[10px] font-black shadow-lg shadow-purple-900/20">NB</div><div class="glass p-5 rounded-3xl rounded-tl-none text-xs text-slate-300 leading-relaxed max-w-[85%] border border-white/5">${reply}</div></div>`; container.scrollTop = container.scrollHeight; }
        async function openEpisodeModal(id, title) { document.getElementById('episode-modal-title').innerText = title; document.getElementById('episodes-modal').classList.add('active'); fetchEpisodes(id); }
        function closeEpisodeModal() { document.getElementById('episodes-modal').classList.remove('active'); }
        async function fetchEpisodes(id) { const res = await fetch(`/api/tracked/${id}/episodes`); const data = await res.json(); const showTitle = document.getElementById('episode-modal-title').innerText; const seasons = {}; data.forEach(ep => { if(!seasons[ep.season]) seasons[ep.season] = []; seasons[ep.season].push(ep); }); document.getElementById('episodes-list').innerHTML = Object.keys(seasons).map(s => `<div class="mb-8"><div class="flex justify-between items-center bg-white/5 p-4 rounded-2xl mb-4 border border-white/10"><div class="flex items-center gap-4"><h3 class="font-black text-sky-400 uppercase tracking-[0.2em] text-[10px]">Season ${s}</h3></div><div class="flex gap-2"><button onclick="bulkSetSeasonStatus(${id}, ${s}, 'wanted')" class="text-[8px] bg-sky-600/10 text-sky-400 px-3 py-1.5 rounded-lg font-black hover:bg-sky-600 hover:text-white transition-all uppercase tracking-widest">Wanted</button><button onclick="bulkSetSeasonStatus(${id}, ${s}, 'skipped')" class="text-[8px] bg-white/5 text-slate-500 px-3 py-1.5 rounded-lg font-black hover:bg-white/10 hover:text-white transition-all uppercase tracking-widest">Skip</button><button onclick="bulkSetSeasonStatus(${id}, ${s}, 'completed')" class="text-[8px] bg-emerald-600/10 text-emerald-500 px-3 py-1.5 rounded-lg font-black hover:bg-emerald-600 hover:text-white transition-all uppercase tracking-widest">Owned</button><button onclick="openInteractiveSearch('${showTitle.replace(/'/g, "\\'")}', null, id, 'S${s.toString().padStart(2,'0')}')" class="text-[8px] bg-amber-600/10 text-amber-400 px-3 py-1.5 rounded-lg font-black hover:bg-amber-600 hover:text-white transition-all uppercase tracking-widest">Search Pack</button></div></div><div class="grid grid-cols-1 gap-1 ml-10">${seasons[s].map(ep => `<div class="text-[10px] p-2.5 hover:bg-white/5 rounded-xl flex justify-between items-center transition-all duration-300"><span class="text-slate-300 font-medium"><b class="text-slate-600 mr-3 font-black">E${ep.episode}</b> ${ep.title || 'Episode ' + ep.episode}</span><div class="flex items-center gap-4"><span class="text-[8px] font-black uppercase tracking-widest ${ep.status === 'downloading' ? 'text-sky-400 animate-pulse' : (ep.status === 'completed' ? 'text-emerald-500' : 'text-slate-600')}">${ep.status}</span><button onclick="openInteractiveSearch('${showTitle.replace(/'/g, "\\'")}', ${ep.id}, null, 'S${ep.season.toString().padStart(2,'0')}E${ep.episode.toString().padStart(2,'0')}')" class="text-amber-500/40 hover:text-amber-400 transition-colors"><svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="3" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" /></svg></button></div></div>`).join('')}</div></div>`).join('') || '<div class="text-center text-slate-700 py-20 font-black uppercase tracking-widest text-xs">No episodes identified</div>'; }
        async function bulkSetSeasonStatus(id, season, status) { await fetch(`/api/tracked/${id}/seasons/${season}/status`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ status }) }); fetchEpisodes(id); }
        async function markWatched(id) { await fetch(`/api/tracked/${id}/watched`, { method: 'POST' }); fetchTracked(); }
        async function markWanted(id) { await fetch(`/api/tracked/${id}/status`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ status: 'wanted' }) }); fetchTracked(); }
        async function rateItem(id, rating) { await fetch(`/api/tracked/${id}/rating`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ rating }) }); fetchTracked(); }
        async function track(id, title, poster, date, type, status = 'wanted') {
            const cleanId = parseInt(id);
            if (isNaN(cleanId)) { alert('Invalid Neural ID'); return; }
            const res = await fetch('/api/track', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    id: cleanId,
                    title,
                    poster_path: (poster === 'null' || poster === 'undefined' || !poster) ? null : poster,
                    release_date: (date === 'null' || date === 'undefined' || !date) ? null : date,
                    media_type: type,
                    status
                })
            });
            if (res.ok) {
                alert('Tracked: ' + title);
                if(status==='wanted') showTab('activity'); else fetchTracked();
            } else {
                alert('Neural uplink failed: ' + res.status);
            }
        }
        async function fetchConfig() {
            const res = await fetch('/api/settings/config');
            const config = await res.json();
            const groups = {};
            config.forEach(item => {
                if (!groups[item.group]) groups[item.group] = [];
                groups[item.group].push(item);
            });
            document.getElementById('config-form').innerHTML = Object.entries(groups).map(([group, items]) => `
                <div class="col-span-full mt-6 mb-2"><h3 class="text-sky-500 font-black text-xs uppercase tracking-[0.2em] ml-2">${group}</h3></div>
                ${items.map(item => `
                    <div class="flex flex-col gap-2">
                        <label class="text-[9px] font-black text-slate-500 uppercase tracking-[0.2em] ml-2">${item.label}</label>
                        <input type="${item.key.includes('PASS') || item.key.includes('KEY') ? 'password' : 'text'}" id="cfg-${item.key}" value="${item.value}" class="bg-white/5 border border-white/10 rounded-2xl px-5 py-4 outline-none focus:border-sky-500 text-xs font-mono text-slate-300 shadow-inner">
                    </div>
                `).join('')}
            `).join('');
        }
        async function saveConfig() { const inputs = document.querySelectorAll('[id^="cfg-"]'); const config = {}; inputs.forEach(input => { const key = input.id.replace('cfg-', ''); config[key] = input.value; }); const res = await fetch('/api/settings/config', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(config) }); if(await res.json()) alert('System Core Updated'); }
        async function fetchDisks() { const res = await fetch('/api/disks'); const data = await res.json(); document.getElementById('disk-info').innerHTML = data.map(d => `<div class="glass p-6 rounded-[2rem] border border-white/5 shadow-xl"><div class="flex justify-between text-[10px] font-black uppercase text-slate-400 mb-3 tracking-widest"><span>${d.name}</span><span class="text-sky-500">${(d.available/1024/1024/1024).toFixed(1)}GB AVAILABLE</span></div><div class="w-full h-1.5 bg-white/5 rounded-full overflow-hidden shadow-inner"><div class="h-full bg-sky-500 shadow-[0_0_10px_rgba(56,189,248,0.3)]" style="width:${(1 - d.available/d.total)*100}%"></div></div></div>`).join(''); }
        async function fetchSysInfo() { const res = await fetch('/api/sysinfo'); const data = await res.json(); document.getElementById('sys-cpu').innerText = `CPU: ${data.cpu_usage.toFixed(1)}%`; document.getElementById('sys-ram').innerText = `RAM: ${data.memory_used}MB`; }
        async function clearQueue() { if(confirm('Purge history?')) { await fetch('/api/media/clear', {method:'DELETE'}); fetchActivity(); } }
        async function scanLibrary() { await fetch('/api/scan-library', {method:'POST'}); alert('Library Synchronization Started'); }
        async function triggerIngest() { await fetch('/api/ingest', {method:'POST'}); alert('Ingest Processor Triggered'); }
        async function updateApp() { if(confirm('Update?')) { await fetch('/api/update', {method:'POST'}); alert('Deployment in Progress...'); } }
        async function deleteTracked(id) { if(confirm('Remove show?')) { await fetch(`/api/tracked/${id}`, {method:'DELETE'}); fetchTracked(); } }
        async function performGlobalSearchDirect(q) { document.getElementById('search-query').value = q; performGlobalSearch(); }
        async function downloadSubtitlesForCurrent() { if(!currentDetailsId) return; const btn = document.getElementById('details-subtitles-btn'); const originalText = btn.innerText; btn.innerText = 'Searching neural indices...'; const res = await fetch(`/api/tracked/${currentDetailsId}/subtitles`, { method: 'POST' }); if(await res.json()) btn.innerText = 'Subtitles Acquired'; else btn.innerText = 'Source Exhausted'; setTimeout(() => { btn.innerText = originalText; }, 3000); }
        function initLogs() { const eventSource = new EventSource('/api/logs'); const logContainer = document.getElementById('tab-logs'); eventSource.onmessage = (e) => { const el = document.createElement('div'); el.innerText = e.data; logContainer.appendChild(el); logContainer.scrollTop = logContainer.scrollHeight; if(logContainer.children.length > 500) logContainer.removeChild(logContainer.firstChild); }; }
        function openInteractiveSearch(title, episodeId, showId, epCode = '', year = '') { currentSearchEpisodeId = episodeId; currentSearchShowId = showId; let query = epCode ? `${title} ${epCode}` : title; if (year) query += ` ${year}`; document.getElementById('interactive-modal-title').innerText = 'Search: ' + query; document.getElementById('interactive-search-input').value = query; document.getElementById('interactive-modal').classList.add('active'); document.getElementById('interactive-results').innerHTML = ''; performInteractiveSearch(); }
        function closeInteractiveModal() { document.getElementById('interactive-modal').classList.remove('active'); }
        async function performInteractiveSearch() { const query = document.getElementById('interactive-search-input').value; if(!query) return; document.getElementById('interactive-results').innerHTML = '<tr><td colspan="5" class="text-center py-10 text-slate-500 uppercase font-bold tracking-widest animate-pulse">Neural Index Query...</td></tr>'; const res = await fetch('/api/interactive-search', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ query }) }); const data = await res.json(); document.getElementById('interactive-results').innerHTML = data.length ? data.map(item => `<tr class="hover:bg-white/5 transition-colors text-[11px]"><td class="py-3 px-4 max-w-md"><div class="font-bold text-slate-200 truncate">${item.title}</div></td><td class="py-3 font-mono text-slate-400">${(item.size / 1024 / 1024 / 1024).toFixed(2)} GB</td><td class="py-3 text-center"><span class="px-2 py-0.5 rounded bg-emerald-500/10 text-emerald-500 font-black">${item.seeders}</span></td><td class="py-3 text-slate-500 italic">${item.indexer}</td><td class="py-3 pr-4 text-right"><button onclick="downloadTorrent('${item.link.replace(/'/g, "\\'")}', '${item.title.replace(/'/g, "\\'")}')" class="bg-sky-600 hover:bg-sky-500 text-white px-4 py-1.5 rounded-lg font-black text-[9px] uppercase transition-all shadow-lg shadow-sky-900/20">Download</button></td></tr>`).join('') : '<tr><td colspan="5" class="text-center py-20 text-rose-500 font-black uppercase tracking-widest">No matching clusters found</td></tr>'; }
        async function downloadTorrent(link, title) { const res = await fetch('/api/download-torrent', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ link, title, episode_id: currentSearchEpisodeId, show_id: currentSearchShowId }) }); if(await res.json()) { alert('Added to neural ingest'); closeInteractiveModal(); if(currentSearchEpisodeId) { closeEpisodeModal(); fetchActivity(); } if(currentSearchShowId) fetchTracked(); } else { alert('Ingest collision'); } }

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
    let mut query = params.q.clone();
    let mut year = None;
    let re = regex::Regex::new(r"\s+(\d{4})$").unwrap();
    if let Some(caps) = re.captures(&params.q) {
        if let Some(y_match) = caps.get(1) {
            if let Ok(y) = y_match.as_str().parse::<u32>() {
                year = Some(y);
                query = re.replace(&params.q, "").to_string();
            }
        }
    }
    let mut movies = state.tmdb.search_movie(&query, year).await.unwrap_or_default();
    let mut tv = state.tmdb.search_tv(&query, year).await.unwrap_or_default();
    for m in &mut movies { m.media_type = Some("movie".to_string()); }
    for t in &mut tv { t.media_type = Some("tv".to_string()); }
    movies.append(&mut tv);

    let tracked = db::get_tracked_shows(&state.pool).await.unwrap_or_default();
    let disapproved = db::get_disapproved_ids(&state.pool).await.unwrap_or_default();
    let tracked_ids: std::collections::HashSet<_> = tracked.into_iter().map(|s| s.tmdb_id as i64).collect();
    
    movies.retain(|m| !tracked_ids.contains(&(m.id as i64)) && !disapproved.contains(&(m.id as i64)));

    Json(movies)
}

#[derive(Deserialize)]
struct SearchQuery { q: String }

#[derive(Deserialize)]
struct SemanticSearchRequest { prompt: String }

async fn semantic_search(State(state): State<AppState>, Json(req): Json<SemanticSearchRequest>) -> Json<Vec<crate::integrations::tmdb::TmdbMedia>> {
    if let Ok(titles_str) = state.ollama.semantic_search_translate(&req.prompt).await {
        info!("Semantic Search identified titles: '{}'", titles_str);
        let mut all_results = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        let tracked_ids = db::get_tracked_shows(&state.pool).await.unwrap_or_default()
            .into_iter().map(|s| s.tmdb_id as i64).collect::<std::collections::HashSet<_>>();
        let disapproved = db::get_disapproved_ids(&state.pool).await.unwrap_or_default();

        for title in titles_str.split(',') {
            let t = title.trim();
            if t.is_empty() { continue; }
            let movies = state.tmdb.search_movie(t, None).await.unwrap_or_default();
            let tv = state.tmdb.search_tv(t, None).await.unwrap_or_default();
            
            if let Some(mut m) = movies.into_iter().next() { 
                if !tracked_ids.contains(&(m.id as i64)) && !disapproved.contains(&(m.id as i64)) && seen_ids.insert(m.id) { 
                    m.media_type = Some("movie".to_string()); all_results.push(m); 
                } 
            }
            if let Some(mut t) = tv.into_iter().next() { 
                if !tracked_ids.contains(&(t.id as i64)) && !disapproved.contains(&(t.id as i64)) && seen_ids.insert(t.id) { 
                    t.media_type = Some("tv".to_string()); all_results.push(t); 
                } 
            }
        }
        return Json(all_results);
    }
    Json(vec![])
}

async fn get_upcoming(State(state): State<AppState>) -> Json<Vec<crate::integrations::tmdb::TmdbMedia>> {
    let mut results = state.tmdb.get_upcoming_movies().await.unwrap_or_default();
    let mut tv = state.tmdb.get_trending_tv().await.unwrap_or_default();
    for m in &mut results { m.media_type = Some("movie".to_string()); }
    for t in &mut tv { t.media_type = Some("tv".to_string()); }
    results.append(&mut tv);

    let tracked = db::get_tracked_shows(&state.pool).await.unwrap_or_default();
    let disapproved = db::get_disapproved_ids(&state.pool).await.unwrap_or_default();
    let tracked_ids: std::collections::HashSet<_> = tracked.into_iter().map(|s| s.tmdb_id as i64).collect();
    
    results.retain(|m| !tracked_ids.contains(&(m.id as i64)) && !disapproved.contains(&(m.id as i64)));

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
struct TrackRequest { id: u32, title: String, poster_path: Option<String>, release_date: Option<String>, media_type: String, status: String }

async fn track_show(State(state): State<AppState>, Json(req): Json<TrackRequest>) -> Json<bool> {
    let mut genres_vec = Vec::new();
    let mut total_seasons = 1;
    if let Ok(details) = if req.media_type == "tv" { state.tmdb.get_tv_details(req.id).await } else { state.tmdb.get_movie_details(req.id).await } {
        if let Some(gs) = details.genres { for g in gs { genres_vec.push(g.name); } }
        total_seasons = details.number_of_seasons.unwrap_or(1);
    }
    let genres_str = if genres_vec.is_empty() { None } else { Some(genres_vec.join(",")) };
    match db::insert_tracked_show(&state.pool, &req.title, req.id, &req.media_type, &req.status, req.poster_path, req.release_date, genres_str, total_seasons).await {
        Ok(_) => {
            let pool = state.pool.clone();
            tokio::spawn(async move { let _ = crate::scan_library(pool).await; });
            Json(true)
        },
        Err(_) => Json(false)
    }
}

async fn get_tracked(State(state): State<AppState>) -> Json<Vec<TrackedShow>> { Json(db::get_tracked_shows(&state.pool).await.unwrap_or_default()) }

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

async fn delete_tracked(State(state): State<AppState>, Path(id): Path<i64>) -> Json<bool> { let _ = db::delete_tracked_show(&state.pool, id).await; Json(true) }
async fn get_episodes(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Vec<db::Episode>> { Json(db::get_episodes_for_show(&state.pool, id).await.unwrap_or_default()) }

#[derive(Deserialize)]
struct StatusRequest { status: String }

async fn set_episode_status(State(state): State<AppState>, Path(id): Path<i64>, Json(req): Json<StatusRequest>) -> Json<bool> { let _ = db::update_episode_status(&state.pool, id, &req.status).await; Json(true) }
async fn set_tracked_status(State(state): State<AppState>, Path(id): Path<i64>, Json(req): Json<StatusRequest>) -> Json<bool> { let _ = sqlx::query("UPDATE tracked_shows SET status = ? WHERE id = ?").bind(&req.status).bind(id).execute(&state.pool).await; Json(true) }

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
    let stream = async_stream::stream! { while let Ok(msg) = rx.recv().await { yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(msg)); } };
    axum::response::sse::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

async fn get_profiles(State(state): State<AppState>) -> Json<Vec<QualityProfile>> { Json(db::get_all_quality_profiles(&state.pool).await.unwrap_or_default()) }

async fn scan_library(State(state): State<AppState>) -> Json<bool> {
    let pool = state.pool.clone(); let is_scanning = state.is_scanning.clone();
    tokio::spawn(async move { is_scanning.store(true, std::sync::atomic::Ordering::SeqCst); let _ = crate::scan_library(pool).await; is_scanning.store(false, std::sync::atomic::Ordering::SeqCst); });
    Json(true)
}

async fn get_scan_status(State(state): State<AppState>) -> Json<bool> { Json(state.is_scanning.load(std::sync::atomic::Ordering::SeqCst)) }

#[derive(Deserialize)]
struct GenreSearchQuery { genre: String }

async fn search_by_genre(State(state): State<AppState>, Query(params): Query<GenreSearchQuery>) -> Json<Vec<crate::integrations::tmdb::TmdbMedia>> {
    let genres_movie = state.tmdb.get_genres(false).await.unwrap_or_default();
    let genres_tv = state.tmdb.get_genres(true).await.unwrap_or_default();
    let gid = genres_movie.iter().find(|g| g.name.to_lowercase() == params.genre.to_lowercase()).or_else(|| genres_tv.iter().find(|g| g.name.to_lowercase() == params.genre.to_lowercase())).map(|g| g.id);
    if let Some(id) = gid {
        let mut results = Vec::new();
        let url = format!("https://api.themoviedb.org/3/discover/movie?api_key={}&with_genres={}", state.tmdb.api_key, id);
        if let Ok(res) = state.tmdb.client.get(&url).send().await {
            let json_res: Result<crate::integrations::tmdb::TmdbSearchResult, _> = res.json().await;
            if let Ok(json) = json_res {
                for mut m in json.results { m.media_type = Some("movie".to_string()); results.push(m); }
            }
        }

        let tracked = db::get_tracked_shows(&state.pool).await.unwrap_or_default();
        let disapproved = db::get_disapproved_ids(&state.pool).await.unwrap_or_default();
        let tracked_ids: std::collections::HashSet<_> = tracked.into_iter().map(|s| s.tmdb_id as i64).collect();
        
        results.retain(|m| !tracked_ids.contains(&(m.id as i64)) && !disapproved.contains(&(m.id as i64)));

        return Json(results);
    }
    Json(vec![])
}

#[derive(Serialize)]
struct ActivityItem { id: String, title: String, status: String, progress: f32, media_type: String, source: String }

async fn get_activity(State(state): State<AppState>) -> Json<Vec<ActivityItem>> {
    let mut activity = Vec::new();
    if let Ok(qbit) = crate::integrations::torrent::QBittorrentClient::new() {
        if qbit.login().await.is_ok() {
            if let Ok(torrents) = qbit.get_torrents().await {
                for t in torrents { activity.push(ActivityItem { id: t.hash.clone(), title: t.name.clone(), status: "Downloading".to_string(), progress: t.progress, media_type: "unknown".to_string(), source: "tracked".to_string() }); }
            }
        }
    }
    let items = sqlx::query_as::<_, MediaItem>("SELECT * FROM media_items WHERE status != 'completed'").fetch_all(&state.pool).await.unwrap_or_default();
    for i in items {
        if activity.iter().any(|a| i.original_filename.contains(&a.title) || a.title.contains(&i.original_filename)) { continue; }
        activity.push(ActivityItem { id: i.id.to_string(), title: i.title.clone(), status: match i.status.as_str() { "parsed" => "Matched", "summarized" => "Processing", _ => "New File" }.to_string(), progress: 1.0, media_type: if i.season.is_some() { "tv" } else { "movie" }.to_string(), source: "ingest".to_string() });
    }
    if let Ok(wanted_eps) = db::get_wanted_episodes(&state.pool).await {
        for (ep, show) in wanted_eps.iter().take(10) {
            let title = format!("{} S{:02}E{:02}", show.title, ep.season, ep.episode);
            if !activity.iter().any(|a| a.title.contains(&show.title) || title.contains(&a.title)) {
                activity.push(ActivityItem { id: format!("ep_{}", ep.id), title, status: "Tracked".to_string(), progress: 0.0, media_type: "tv".to_string(), source: "tracked".to_string() });
            }
        }
    }
    Json(activity)
}

async fn trigger_ingest(State(state): State<AppState>) -> Json<bool> {
    let pool = state.pool.clone(); let tmdb = state.tmdb.clone(); let ollama = state.ollama.clone();
    if let Ok(qbit) = crate::integrations::torrent::QBittorrentClient::new() {
        if qbit.login().await.is_ok() {
            let qbit_arc = Arc::new(qbit);
            tokio::spawn(async move { let _ = crate::scan_ingest_folder(pool, tmdb, ollama, qbit_arc).await; });
            return Json(true);
        }
    }
    Json(false)
}

#[derive(Serialize)]
struct ConfigItem {
    key: String,
    value: String,
    label: String,
    group: String,
}

async fn get_config() -> Json<Vec<ConfigItem>> {
    let keys = vec![
        ("TMDB_API_KEY", "TMDB API Key", "External Services"),
        ("OPENSUBTITLES_API_KEY", "OpenSubtitles API Key", "External Services"),
        ("QBITTORRENT_URL", "qBittorrent URL", "Automation"),
        ("QBITTORRENT_USER", "qBittorrent User", "Automation"),
        ("QBITTORRENT_PASS", "qBittorrent Password", "Automation"),
        ("INDEXER_URL", "Indexer URL", "Automation"),
        ("INDEXER_API_KEY", "Indexer API Key", "Automation"),
        ("OLLAMA_BASE_URL", "Ollama Base URL", "AI Discovery"),
        ("OLLAMA_MODEL", "Ollama Model", "AI Discovery"),
        ("NEURARR_INGEST_DIR", "Ingest Directory", "Storage"),
        ("NEURARR_LIBRARY_DIR", "Library Directory", "Storage"),
    ];
    let mut config = Vec::new();
    for (key, label, group) in keys {
        config.push(ConfigItem {
            key: key.to_string(),
            value: std::env::var(key).unwrap_or_default(),
            label: label.to_string(),
            group: group.to_string(),
        });
    }
    Json(config)
}

async fn update_config(Json(config): Json<std::collections::HashMap<String, String>>) -> Json<bool> {
    let mut env_content = tokio::fs::read_to_string(".env").await.unwrap_or_default();
    for (k, v) in config {
        let re = regex::Regex::new(&format!(r"(?m)^{}=(.*)$", k)).unwrap();
        if re.is_match(&env_content) { env_content = re.replace(&env_content, format!("{}={}", k, v).as_str()).to_string(); }
        else { env_content.push_str(&format!("\n{}={}", k, v)); }
        unsafe { std::env::set_var(&k, &v); }
    }
    let _ = tokio::fs::write(".env", env_content).await;
    Json(true)
}

#[derive(Deserialize)]
struct BotChatRequest { message: String }

async fn bot_chat(State(state): State<AppState>, Json(req): Json<BotChatRequest>) -> Json<String> {
    let mut context = String::from("You are the NeurArr Bot, a helpful media assistant. Here is the user's collection context:\n");
    if let Ok(tracked) = db::get_tracked_shows(&state.pool).await {
        let mut top_rated = tracked.clone();
        top_rated.sort_by(|a, b| b.rating.cmp(&a.rating));
        context.push_str("- Top Rated: ");
        for item in top_rated.iter().filter(|i| i.rating > 0).take(10) { context.push_str(&format!("{} ({} stars), ", item.title, item.rating)); }
        context.push_str("\n- Recent Collection: ");
        for item in tracked.iter().take(10) { context.push_str(&format!("{}, ", item.title)); }
    }
    context.push_str("\nUse this to inform recommendations. Be concise.");
    match state.ollama.chat(&context, &req.message, false).await { Ok(res) => Json(res), Err(_) => Json("Thinking error.".to_string()) }
}

#[derive(Deserialize)]
struct InteractiveSearchRequest { query: String }

#[derive(Serialize)]
struct InteractiveSearchResult { title: String, link: String, size: u64, seeders: u32, indexer: String }

async fn interactive_search(State(_state): State<AppState>, Json(req): Json<InteractiveSearchRequest>) -> Json<Vec<InteractiveSearchResult>> {
    let indexer = crate::integrations::indexer::IndexerClient::new().unwrap();
    if let Ok(res) = indexer.search(&req.query).await {
        let results = res.into_iter().map(|item| InteractiveSearchResult { title: item.title, link: item.link, size: item.size, seeders: item.seeders, indexer: item.indexer }).collect();
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
            if let Ok(rows) = sqlx::query("SELECT s.tmdb_id, s.media_type, s.id as sid FROM episodes e JOIN tracked_shows s ON e.show_id = s.id WHERE e.id = ?").bind(eid).fetch_all(&state.pool).await {
                if let Some(r) = rows.first() {
                    use sqlx::Row;
                    let _ = db::insert_pending_download(&state.pool, &req.title, Some(r.get::<i64, _>("sid")), Some(eid), r.get::<i64, _>("tmdb_id") as u32, &r.get::<String, _>("media_type"), None).await;
                }
            }
        } else if let Some(sid) = req.show_id {
            let _ = db::update_tracked_show_status(&state.pool, sid, "downloading").await;
            if let Ok(tracked) = db::get_tracked_shows(&state.pool).await {
                if let Some(s) = tracked.iter().find(|t| t.id == sid) {
                    let _ = db::insert_pending_download(&state.pool, &req.title, Some(sid), None, s.tmdb_id as u32, &s.media_type, None).await;
                }
            }
        }
        return Json(true);
    }
    Json(false)
}

async fn mark_watched(State(state): State<AppState>, Path(id): Path<i64>) -> Json<bool> { let _ = db::update_tracked_show_info(&state.pool, id, Some("watched"), None, None).await; Json(true) }

#[derive(Deserialize)]
struct RateRequest { rating: i64 }

async fn rate_item(State(state): State<AppState>, Path(id): Path<i64>, Json(req): Json<RateRequest>) -> Json<bool> { let _ = db::update_tracked_show_info(&state.pool, id, None, None, Some(req.rating)).await; Json(true) }

async fn get_recommendations(State(state): State<AppState>) -> Json<Vec<crate::integrations::tmdb::TmdbMedia>> {
    let mut recs = Vec::new();
    let tracked_shows = db::get_tracked_shows(&state.pool).await.unwrap_or_default();
    let tracked_ids: std::collections::HashSet<_> = tracked_shows.iter().map(|s| s.tmdb_id as i64).collect();
    let disapproved = db::get_disapproved_ids(&state.pool).await.unwrap_or_default();
    let approved = db::get_approved_ids(&state.pool).await.unwrap_or_default();

    let mut seeds = Vec::new();
    let mut top_rated = tracked_shows.clone();
    top_rated.sort_by(|a, b| b.rating.cmp(&a.rating));
    for item in top_rated.iter().filter(|i| i.rating >= 4).take(5) {
        seeds.push((item.tmdb_id as i64, item.media_type.clone()));
    }
    for app in approved {
        if !seeds.iter().any(|s| s.0 == app.0) { seeds.push(app); }
    }

    for (tmdb_id, media_type) in seeds.iter().take(10) {
        if media_type == "movie" { 
            if let Ok(results) = state.tmdb.get_movie_recommendations(*tmdb_id as u32).await { 
                for mut m in results { m.media_type = Some("movie".to_string()); recs.push(m); } 
            } 
        } else { 
            if let Ok(results) = state.tmdb.get_tv_recommendations(*tmdb_id as u32).await { 
                for mut m in results { m.media_type = Some("tv".to_string()); recs.push(m); } 
            } 
        }
    }
    
    let mut seen = std::collections::HashSet::new(); 
    recs.retain(|m| {
        seen.insert(m.id) && 
        !tracked_ids.contains(&(m.id as i64)) && 
        !disapproved.contains(&(m.id as i64))
    });
    
    Json(recs.into_iter().take(20).collect())
}

#[derive(Deserialize)]
struct VoteRequest { tmdb_id: u32, media_type: String, vote: i32 }

async fn vote_recommendation(State(state): State<AppState>, Json(req): Json<VoteRequest>) -> Json<bool> {
    match db::insert_recommendation_vote(&state.pool, req.tmdb_id, &req.media_type, req.vote).await {
        Ok(_) => Json(true),
        Err(_) => Json(false)
    }
}

#[derive(Serialize)]
struct NextUpItem { show_id: i64, show_title: String, episode_id: i64, season: i64, episode: i64, title: String, poster_path: Option<String> }

async fn get_next_up(State(state): State<AppState>) -> Json<Vec<NextUpItem>> {
    let rows = sqlx::query("SELECT s.id as sid, s.title as stitle, s.poster_path, e.id as eid, e.season, e.episode, e.title as etitle FROM tracked_shows s JOIN episodes e ON e.show_id = s.id WHERE s.media_type = 'tv' AND e.status NOT IN ('completed', 'skipped') AND EXISTS (SELECT 1 FROM episodes e2 WHERE e2.show_id = s.id AND e2.status = 'completed') GROUP BY s.id HAVING e.season = MIN(e.season) AND e.episode = MIN(e.episode) ORDER BY s.last_updated DESC LIMIT 10").fetch_all(&state.pool).await.unwrap_or_default();
    let mut results = Vec::new();
    for r in rows { use sqlx::Row; results.push(NextUpItem { show_id: r.get("sid"), show_title: r.get("stitle"), episode_id: r.get("eid"), season: r.get("season"), episode: r.get("episode"), title: r.get("etitle"), poster_path: r.get("poster_path") }); }
    Json(results)
}

async fn get_preference_chips(State(state): State<AppState>) -> Json<Vec<String>> {
    let mut genre_counts = std::collections::HashMap::new();
    if let Ok(tracked) = db::get_tracked_shows(&state.pool).await { for item in tracked { if let Some(genres) = item.genres { for g in genres.split(',') { *genre_counts.entry(g.trim().to_string()).or_insert(0) += 1; } } } }
    let mut chips: Vec<_> = genre_counts.into_iter().collect(); chips.sort_by(|a, b| b.1.cmp(&a.1));
    Json(chips.into_iter().take(10).map(|(name, _)| name).collect())
}

async fn get_trailers(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Vec<crate::integrations::tmdb::TmdbVideo>> {
    if let Ok(tracked) = db::get_tracked_shows(&state.pool).await { if let Some(s) = tracked.iter().find(|t| t.id == id) { if let Ok(videos) = state.tmdb.get_videos(s.tmdb_id as u32, s.media_type == "tv").await { let trailers: Vec<_> = videos.into_iter().filter(|v| v.r#type == "Trailer").collect(); return Json(trailers); } } }
    Json(vec![])
}

async fn get_credits(State(state): State<AppState>, Path(id): Path<i64>) -> Json<crate::integrations::tmdb::TmdbCredits> {
    if let Ok(tracked) = db::get_tracked_shows(&state.pool).await { if let Some(s) = tracked.iter().find(|t| t.id == id) { if let Ok(credits) = state.tmdb.get_credits(s.tmdb_id as u32, s.media_type == "tv").await { return Json(credits); } } }
    Json(crate::integrations::tmdb::TmdbCredits { cast: vec![] })
}

#[derive(Deserialize)]
struct BulkStatusRequest { status: String }

async fn bulk_set_season_status(State(state): State<AppState>, Path((id, season)): Path<(i64, i64)>, Json(req): Json<BulkStatusRequest>) -> Json<bool> { let _ = db::bulk_update_episodes_status(&state.pool, id, season, &req.status).await; Json(true) }

async fn get_external_trailers(State(state): State<AppState>, Path((media_type, id)): Path<(String, u32)>) -> Json<Vec<crate::integrations::tmdb::TmdbVideo>> { if let Ok(videos) = state.tmdb.get_videos(id, media_type == "tv").await { let trailers: Vec<_> = videos.into_iter().filter(|v| v.r#type == "Trailer").collect(); return Json(trailers); } Json(vec![]) }

async fn get_external_credits(State(state): State<AppState>, Path((media_type, id)): Path<(String, u32)>) -> Json<crate::integrations::tmdb::TmdbCredits> { if let Ok(credits) = state.tmdb.get_credits(id, media_type == "tv").await { return Json(credits); } Json(crate::integrations::tmdb::TmdbCredits { cast: vec![] }) }

async fn fetch_subtitles_for_tracked(State(state): State<AppState>, Path(id): Path<i64>) -> Json<bool> {
    if let Ok(tracked) = db::get_tracked_shows(&state.pool).await {
        if let Some(s) = tracked.iter().find(|t| t.id == id) {
            if let Ok(sub_client) = crate::integrations::subtitles::SubtitleClient::new() {
                let lib_dir = std::env::var("NEURARR_LIBRARY_DIR").unwrap_or_else(|_| "./library".to_string());
                let mut dest = std::path::PathBuf::from(&lib_dir);
                if s.media_type == "tv" { dest.push("TV"); } else { dest.push("Movies"); }
                dest.push(&s.title);
                dest.push(&s.title);
                let _ = tokio::fs::create_dir_all(dest.parent().unwrap()).await;
                if sub_client.download_subtitles(&s.title, &dest).await.is_ok() { return Json(true); }
            }
        }
    }
    Json(false)
}

async fn trigger_update() -> Json<bool> {
    tokio::spawn(async { let _ = std::process::Command::new(std::env::current_exe().unwrap()).arg("update").spawn(); });
    Json(true)
}
