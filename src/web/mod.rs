use anyhow::Result;
use axum::{
    extract::{State, Query, Path},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post, delete},
    Json, Router,
};
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
        .route("/login", get(login_page).post(handle_login))
        .route("/api/media", get(get_media))
        .route("/api/media/clear", delete(clear_queue))
        .route("/api/media/{id}/match", post(match_media))
        .route("/api/search", get(search_media))
        .route("/api/upcoming", get(get_upcoming))
        .route("/api/calendar", get(get_calendar))
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
        .placeholder-poster { background: #1e293b; display: flex; align-items: center; justify-content: center; color: #475569; font-weight: bold; font-size: 10px; text-align: center; }
    </style>
</head>
<body class="p-4">
    <nav class="glass sticky top-0 flex gap-4 p-4 mb-4 rounded-xl items-center">
        <div class="font-bold text-xl mr-4 text-sky-400">NeurArr</div>
        <button onclick="showTab('queue')" id="nav-queue" class="active">QUEUE</button>
        <button onclick="showTab('tracked')" id="nav-tracked">COLLECTION</button>
        <button onclick="showTab('calendar')" id="nav-calendar">CALENDAR</button>
        <button onclick="showTab('upcoming')" id="nav-upcoming">DISCOVER</button>
        <button onclick="showTab('downloads')" id="nav-downloads">DOWNLOADS</button>
        <button onclick="showTab('settings')" id="nav-settings">SETTINGS</button>
        <div class="ml-auto text-[10px] text-slate-400 flex gap-4 items-center font-bold">
            <span id="sys-cpu">CPU: 0%</span><span id="sys-ram">RAM: 0MB</span>
            <button onclick="updateApp()" class="text-amber-400 px-2 py-1 border border-amber-400/30 rounded">UPDATE</button>
        </div>
    </nav>

    <div class="max-w-7xl mx-auto">
        <div class="flex gap-4 mb-8">
            <input type="text" id="search-query" placeholder="Search movies or shows..." class="flex-grow glass rounded-xl px-6 py-3 outline-none focus:ring-2 focus:ring-sky-500/50">
            <button onclick="performGlobalSearch()" class="bg-sky-600 px-8 py-3 rounded-xl font-semibold hover:bg-sky-500 transition-colors">Search</button>
        </div>

        <div id="tab-queue" class="grid grid-cols-1 md:grid-cols-3 gap-4"></div>
        <div id="tab-tracked" class="hidden grid grid-cols-2 md:grid-cols-5 gap-4"></div>
        <div id="tab-calendar" class="hidden space-y-4"></div>
        <div id="tab-upcoming" class="hidden grid grid-cols-2 md:grid-cols-5 gap-4"></div>
        <div id="tab-downloads" class="hidden space-y-2"></div>
        <div id="tab-search" class="hidden grid grid-cols-2 md:grid-cols-5 gap-4"></div>
        
        <div id="tab-settings" class="hidden glass p-8 rounded-xl">
            <h2 class="font-bold mb-4 text-xl">System Status</h2>
            <div id="disk-info" class="space-y-4"></div>
            <div class="mt-8 border-t border-slate-800 pt-8">
                <button onclick="scanLibrary()" class="bg-sky-600 px-6 py-3 rounded-xl font-bold hover:bg-sky-500 transition-colors">FULL LIBRARY RE-SCAN</button>
                <button onclick="clearQueue()" class="bg-rose-600 px-6 py-3 rounded-xl font-bold hover:bg-rose-500 transition-colors ml-4">PURGE QUEUE HISTORY</button>
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
        <div id="modal-results" class="grid grid-cols-2 md:grid-cols-3 gap-4">
            <div class="col-span-3 text-center text-slate-500 py-10">Use the search box above to find a match.</div>
        </div>
    </div></div>

    <div id="episodes-modal" class="modal"><div class="glass p-8 rounded-3xl w-full max-w-4xl max-h-[80vh] overflow-y-auto">
        <div class="flex justify-between items-center mb-6">
            <h2 id="episode-modal-title" class="text-xl font-bold"></h2>
            <button onclick="closeEpisodeModal()" class="text-slate-400 hover:text-white font-bold">CLOSE</button>
        </div>
        <div id="episodes-list" class="space-y-2"></div>
    </div></div>

    <script>
        let currentMatchId = null;
        const placeholder = 'data:image/svg+xml;base64,PHN2ZyB3aWR0aD0iMjAwIiBoZWlnaHQ9IjMwMCIgeG1sbnM9Imh0dHA6Ly93d3cudzMub3JnLzIwMDAvc3ZnIj48cmVjdCB3aWR0aD0iMTAwJSIgaGVpZ2h0PSIxMDAlIiBmaWxsPSIjMWUyOTNiIi8+PHRleHQgeD0iNTAlIiB5PSI1MCUiIGZpbGw9IiM0NzU1NjkiIGZvbnQtc2l6ZT0iMTQiIGZvbnQtZmFtaWx5PSJzYW5zLXNlcmlmIiBkeT0iLjNlbSIgdGV4dC1hbmNob3I9Im1pZGRsZSI+Tk8gUE9TVEVSPC90ZXh0Pjwvc3ZnPg==';

        function showTab(tab) {
            ['queue', 'tracked', 'upcoming', 'downloads', 'settings', 'search', 'calendar'].forEach(t => {
                const el = document.getElementById('tab-' + t);
                if (el) el.classList.toggle('hidden', t !== tab);
                const nav = document.getElementById('nav-' + t);
                if (nav) nav.classList.toggle('active', t === tab);
            });
            if(tab === 'queue') fetchQueue(); if(tab === 'tracked') fetchTracked(); if(tab === 'upcoming') fetchUpcoming(); if(tab === 'downloads') fetchTorrents(); if(tab === 'settings') fetchDisks(); if(tab === 'calendar') fetchCalendar();
        }

        async function fetchQueue() {
            const res = await fetch('/api/media'); const data = await res.json();
            document.getElementById('tab-queue').innerHTML = data.length ? data.map(item => `
                <div class="glass p-4 rounded-xl card-content">
                    <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w200${item.poster_path}` : placeholder}" class="w-16 h-24 float-left mr-4 rounded shadow-lg" onerror="this.src='${placeholder}'">
                    <div class="font-bold truncate text-sm text-sky-400">${item.title}</div>
                    <div class="text-[9px] font-black bg-slate-800 text-slate-400 px-1.5 py-0.5 rounded inline-block mt-1 uppercase">${item.status}</div>
                    <div class="text-[10px] text-slate-500 mt-2 truncate">${item.original_filename}</div>
                    <button onclick="openMatchModal(${item.id}, '${item.title.replace(/'/g, "\\'")}')" class="text-[10px] font-bold text-sky-400 mt-3 block hover:underline">MANUAL MATCH</button>
                </div>
            `).join('') : '<div class="col-span-3 text-center text-slate-500 py-20">Queue is empty.</div>';
        }

        async function fetchTracked() {
            const res = await fetch('/api/tracked'); const data = await res.json();
            document.getElementById('tab-tracked').innerHTML = data.map(item => `
                <div class="glass rounded-xl overflow-hidden group border border-slate-800/50">
                    <div class="relative"><img src="${item.poster_path ? `https://image.tmdb.org/t/p/w500${item.poster_path}` : placeholder}" class="h-64 w-full object-cover" onerror="this.src='${placeholder}'">
                    <div class="absolute inset-0 bg-black/60 opacity-0 group-hover:opacity-100 flex items-center justify-center transition-all">
                        <button onclick="deleteTracked(${item.id})" class="bg-rose-600 text-white px-4 py-2 rounded text-xs font-bold">REMOVE</button>
                    </div></div>
                    <div class="p-3"><div class="font-bold text-sm truncate">${item.title}</div>
                    <button onclick="openEpisodeModal(${item.id}, '${item.title.replace(/'/g, "\\'")}')" class="text-xs text-sky-400 mt-1 font-bold uppercase">View Episodes</button></div>
                </div>
            `).join('');
        }

        async function performGlobalSearch() {
            const query = document.getElementById('search-query').value; if(!query) return; showTab('search');
            const res = await fetch('/api/search?q=' + encodeURIComponent(query)); const data = await res.json();
            document.getElementById('tab-search').innerHTML = data.map(item => `
                <div class="glass rounded-xl overflow-hidden p-4 border border-slate-800/50">
                    <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w500${item.poster_path}` : placeholder}" class="h-48 w-full object-cover rounded shadow-lg" onerror="this.src='${placeholder}'">
                    <div class="mt-3 text-sm font-bold truncate">${item.title || item.name}</div>
                    <div class="text-[10px] text-slate-500 mb-3">${item.release_date || item.first_air_date || 'Unknown'}</div>
                    <button onclick="track('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}', '${item.release_date || item.first_air_date}', '${item.media_type}')" class="w-full bg-sky-600/20 text-sky-400 py-2 rounded font-bold text-[10px] hover:bg-sky-600 hover:text-white transition-all">TRACK</button>
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
            
            document.getElementById('modal-results').innerHTML = '<div class="col-span-3 text-center text-slate-500 py-10">Searching matches for "'+query+'"...</div>';
            
            try {
                const res = await fetch('/api/search?q=' + encodeURIComponent(query)); 
                const data = await res.json();
                
                if (data.length === 0) {
                    document.getElementById('modal-results').innerHTML = '<div class="col-span-3 text-center text-rose-400 py-10">No matches found. Try refining your search.</div>';
                    return;
                }

                document.getElementById('modal-results').innerHTML = data.map(item => `
                    <div class="glass p-3 rounded-xl text-center border border-slate-800">
                        <img src="${item.poster_path ? `https://image.tmdb.org/t/p/w200${item.poster_path}` : placeholder}" class="w-full h-32 object-cover rounded shadow-md" onerror="this.src='${placeholder}'">
                        <div class="font-bold text-[10px] mt-2 truncate w-full px-1">${item.title || item.name}</div>
                        <div class="text-[8px] text-slate-500 mb-2">${item.release_date || item.first_air_date || ''}</div>
                        <button onclick="applyMatch('${item.id}', '${(item.title || item.name).replace(/'/g, "\\'")}', '${item.poster_path}')" class="text-sky-400 text-[10px] font-bold hover:underline uppercase">Select</button>
                    </div>
                `).join('');
            } catch (e) {
                document.getElementById('modal-results').innerHTML = '<div class="col-span-3 text-center text-rose-400 py-10">Error searching matches.</div>';
            }
        }

        async function applyMatch(tmdbId, title, poster) {
            const applyToAll = confirm('Smart Match all similar titles in queue?');
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
            document.getElementById('episodes-list').innerHTML = data.length ? data.map(ep => `
                <div class="text-[11px] p-3 bg-slate-900 rounded-xl flex justify-between items-center border border-slate-800/50">
                    <span><b class="text-sky-400 mr-2">S${ep.season}E${ep.episode}</b> ${ep.title}</span>
                    <span class="text-[9px] font-black uppercase text-slate-500">${ep.status}</span>
                </div>
            `).join('') : '<div class="text-center text-slate-500 py-10">Syncing episodes... check back in a minute.</div>';
        }

        async function fetchTorrents() {
            const res = await fetch('/api/torrents'); const data = await res.json();
            document.getElementById('tab-downloads').innerHTML = data.length ? data.map(t => `
                <div class="glass p-4 rounded-xl border border-slate-800">
                    <div class="flex justify-between text-xs font-bold mb-2"><span>${t.name}</span><span class="text-sky-400">${(t.progress*100).toFixed(1)}%</span></div>
                    <div class="w-full h-1.5 bg-slate-800 rounded-full overflow-hidden"><div class="bg-sky-500 h-full transition-all duration-1000" style="width:${t.progress*100}%"></div></div>
                    <div class="flex justify-between text-[9px] text-slate-500 mt-2 font-black"><span>${t.state.toUpperCase()}</span><span>${(t.dlspeed / 1024 / 1024).toFixed(1)} MB/S</span></div>
                </div>
            `).join('') : '<div class="text-center text-slate-500 py-20 font-bold uppercase tracking-widest text-xs">No active downloads in qBittorrent.</div>';
        }

        async function fetchSysInfo() {
            const res = await fetch('/api/sysinfo'); const data = await res.json();
            document.getElementById('sys-cpu').innerText = `CPU: ${data.cpu_usage.toFixed(1)}%`;
            document.getElementById('sys-ram').innerText = `RAM: ${data.memory_used}MB`;
        }

        async function fetchDisks() {
            const res = await fetch('/api/disks'); const data = await res.json();
            document.getElementById('disk-info').innerHTML = data.map(d => `
                <div class="mb-4">
                    <div class="flex justify-between text-xs font-bold mb-1"><span>${d.name}</span><span>${d.available}GB / ${d.total}GB FREE</span></div>
                    <div class="w-full h-1 bg-slate-800 rounded-full overflow-hidden"><div class="bg-sky-500 h-full" style="width:${(1 - d.available/d.total)*100}%"></div></div>
                </div>
            `).join('');
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

        async function track(id, title, poster, date, type) { await fetch('/api/track', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({id:parseInt(id),title,poster_path:poster,release_date:date,media_type:type||'movie'})}); alert('Added '+title+' to Collection'); }
        async function deleteTracked(id) { if(confirm('Permanently stop tracking this show?')) { await fetch('/api/tracked/'+id,{method:'DELETE'}); fetchTracked(); } }
        async function clearQueue() { if(confirm('Clear all processing history?')) { await fetch('/api/media/clear', {method:'DELETE'}); fetchQueue(); } }
        async function scanLibrary() { await fetch('/api/scan-library', {method:'POST'}); alert('Full library scan started!'); }
        async function updateApp() { if(confirm('Download latest version and rebuild?')) { await fetch('/api/update', {method:'POST'}); alert('Updating... Refresh manually in 1 minute.'); } }

        showTab('queue'); setInterval(fetchSysInfo, 3000); setInterval(fetchTorrents, 5000);
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
