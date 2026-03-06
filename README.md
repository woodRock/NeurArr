# NeurArr

[![Rust CI](https://github.com/woodRock/NeurArr/actions/workflows/rust.yml/badge.svg)](https://github.com/woodRock/NeurArr/actions/workflows/rust.yml)

NeurArr is a blazingly fast, privacy-first, offline media management daemon.
 It uses local AI (via Ollama) to parse scene release names and generate spoiler-free metadata for your library.

## Prerequisites

To use the full automation features of NeurArr, you need the following services running:

### 1. Ollama (AI Inference)
- **Install:** [ollama.com](https://ollama.com)
- **Model:** Run `ollama run qwen3.5:0.8b` (NeurArr uses this for parsing and summary rewriting).

### 2. Jackett (Indexer)
- **Install:** [Jackett GitHub](https://github.com/Jackett/Jackett)
- **Setup:** Configure at least one indexer (tracker) in the Jackett dashboard.
- **API Key:** Required for NeurArr to search for new content.

### 3. qBittorrent (Downloader)
- **Install:** [qbittorrent.org](https://www.qbittorrent.org/)
- **Setup:** Enable "Web UI" in settings (Tools -> Options -> Web UI).
- **Default Port:** 8080.

### 4. Plex Media Server (Optional)
- **Setup:** Needed if you want NeurArr to automatically trigger library scans after processing.

---

## Setup & Installation

1.  **Clone the repository.**
2.  **Configure Environment:**
    Create a `.env` file and fill in your keys:
    ```env
    TMDB_API_KEY=your_tmdb_key
    INDEXER_API_KEY=your_jackett_api_key
    INDEXER_URL=http://localhost:9117/api/v2.0/indexers/all/results
    QBITTORRENT_URL=http://localhost:8080
    QBITTORRENT_USER=admin
    QBITTORRENT_PASS=your_password
    PLEX_TOKEN=your_plex_token
    NEURARR_INGEST_DIR=./ingest
    NEURARR_LIBRARY_DIR=./library
    DATABASE_URL=sqlite:neurarr.db
    ```
3.  **Run the application:**
    ```bash
    cargo run
    ```

## Features

- **AI Scene Parsing:** Automatically extracts Title, Season, Episode, and Resolution from complex filenames.
- **Spoiler-Free Summaries:** Uses the local LLM to rewrite TMDB summaries, removing twists and endings.
- **Automated Search:** Periodically checks Jackett for your tracked shows and adds them to qBittorrent.
- **Smart Organization:** Moves finished downloads from `ingest/` to a clean library structure (`Movies/Title (Year)/Title.mkv`).
- **NFO Generation:** Creates XML `.nfo` files with the spoiler-free plot for Plex/Kodi compatibility.
- **Modern Dashboard:** Monitor your queue, search for new content, and track system resources (CPU/RAM) at `http://localhost:3000`.

## Tech Stack

- **Rust (Edition 2024)**
- **Ollama** for local high-speed AI inference.
- **Axum** for the web dashboard and API.
- **SQLx & SQLite** for persistent storage.
- **Notify** for real-time directory watching.
