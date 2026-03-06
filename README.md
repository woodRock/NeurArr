# 🧠 NeurArr Pro

[![Rust CI](https://github.com/woodRock/NeurArr/actions/workflows/rust.yml/badge.svg)](https://github.com/woodRock/NeurArr/actions/workflows/rust.yml)

**NeurArr** is a blazingly fast, privacy-first media management and discovery suite. It combines the automation of the "Arr" stack with the intelligence of local AI to provide a modern, engaging experience for cinephiles.

---

## 🚀 Key Features

*   **AI-Powered Ingestion**: Uses local LLMs (via Ollama) to parse complex scene releases and generate spoiler-free metadata.
*   **Smart Discovery ("For You")**: A personalized recommendation engine that learns from your collection and star ratings.
*   **Genre-Based Exploration**: Interactive genre chips that perform semantic discovery across the entire TMDB database.
*   **Unified Activity Feed**: Monitor your media lifecycle from **Tracked** → **Downloading** → **Ingested** in one real-time view.
*   **Automated Quality Upgrades**: Set your desired quality cutoff (e.g., 1080p), and NeurArr will automatically hunt for better versions until that goal is met.
*   **Interactive Search**: Manually browse indexer results and choose the exact release you want.
*   **Subtitle Automation**: Built-in OpenSubtitles integration to automatically fetch `.srt` files for every import.
*   **Security First**: Full API and dashboard authentication with secure session management.

---

## 🛠 Prerequisites

Before running NeurArr, ensure the following services are installed and reachable:

1.  **[Ollama](https://ollama.com/) (AI Inference)**:
    *   Install Ollama and pull the default model: `ollama run qwen3.5:0.8b`.
2.  **[Rust](https://www.rust-lang.org/tools/install) (Toolchain)**:
    *   Required to compile the binary (Edition 2024).
3.  **[Jackett](https://github.com/Jackett/Jackett) or [Prowlarr](https://prowlarr.com/) (Indexer)**:
    *   Provides the Torznab API for searching trackers.
4.  **[qBittorrent](https://www.qbittorrent.org/) (Downloader)**:
    *   Must have **Web UI** enabled in settings.
5.  **[TMDB API Key](https://www.themoviedb.org/documentation/api) (Metadata)**:
    *   Required for movie/show data and discovery features.
6.  **[OpenSubtitles API Key](https://www.opensubtitles.com/en/consumers) (Optional)**:
    *   Required for automated subtitle downloads.

---

## 💻 Installation

### 1. Clone the Repository
```bash
git clone https://github.com/woodRock/NeurArr.git
cd NeurArr
```

### 2. Platform-Specific Setup

#### **Windows**
*   Install [Build Tools for Visual Studio 2022](https://visualstudio.microsoft.com/visual-cpp-build-tools/).
*   Install [SQLite](https://www.sqlite.org/download.html) (or use the bundled `sqlx` features).
*   Create your `.env` file (see Configuration below).
*   Run: `cargo run -- Run`

#### **macOS**
*   Install Homebrew: `/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"`
*   Install dependencies: `brew install sqlite openssl`
*   Run: `cargo run -- Run`

#### **Linux (Ubuntu/Debian)**
*   `sudo apt update && sudo apt install build-essential libssl-dev pkg-config sqlite3`
*   Run: `cargo run -- Run`

---

## ⚙️ Configuration

Create a `.env` file in the root directory:

```env
# Core API Keys
TMDB_API_KEY=your_tmdb_key
OPENSUBTITLES_API_KEY=your_key_here  # Optional

# Indexer Settings (Jackett/Prowlarr)
INDEXER_URL=http://localhost:9117/api/v2.0/indexers/all/results
INDEXER_API_KEY=your_jackett_key

# Download Settings (qBittorrent)
QBITTORRENT_URL=http://localhost:8080
QBITTORRENT_USER=admin
QBITTORRENT_PASS=adminadmin

# LLM Settings
OLLAMA_BASE_URL=http://localhost:11434
OLLAMA_MODEL=qwen3.5:0.8b

# Paths & Database
NEURARR_INGEST_DIR=./ingest
NEURARR_LIBRARY_DIR=./library
DATABASE_URL=sqlite:neurarr.db
```

---

## 🏃 Running the App

NeurArr uses a multi-command CLI:

*   **Setup**: Create the initial admin user.
    ```bash
    cargo run -- Setup
    ```
*   **Run**: Start the daemon (API, Web UI, and Scheduler).
    ```bash
    cargo run -- Run
    ```
*   **Scan**: Force a manual scan of the library.
    ```bash
    cargo run -- Scan
    ```

Access the dashboard at **`http://localhost:3000`**.

---

## 🧪 Testing

Run the comprehensive test suite (including AI logic and search de-duplication tests):
```bash
cargo test
```

---

## 🛡 Security Note
NeurArr implements a session-based authentication system. Ensure your `auth` cookie is protected and never share your `.env` file as it contains cleartext credentials for your entire media stack.
