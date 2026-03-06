use anyhow::Result;
use reqwest::{Client, cookie::Jar};
use serde::{Deserialize, Serialize};
use std::env;
use std::sync::Arc;
use tracing::info;

#[derive(Serialize)]
#[allow(dead_code)]
struct LoginRequest {
    username: String,
    password: String,
}

pub struct QBittorrentClient {
    client: Client,
    base_url: String,
}

impl QBittorrentClient {
    pub fn new() -> Result<Self> {
        let base_url = env::var("QBITTORRENT_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
        let jar = Arc::new(Jar::default());
        
        let client = Client::builder()
            .cookie_provider(jar)
            .build()?;

        Ok(Self { client, base_url })
    }

    pub async fn login(&self) -> Result<()> {
        let username = env::var("QBITTORRENT_USER").unwrap_or_else(|_| "admin".to_string());
        let password = env::var("QBITTORRENT_PASS").unwrap_or_else(|_| "adminadmin".to_string());

        let url = format!("{}/api/v2/auth/login", self.base_url);
        let params = [("username", username), ("password", password)];
        
        let response = self.client
            .post(&url)
            .form(&params)
            .send()
            .await?;

        if response.status().is_success() {
            info!("Successfully logged into qBittorrent");
            Ok(())
        } else {
            anyhow::bail!("Failed to login to qBittorrent: {}", response.status());
        }
    }

    pub async fn add_torrent_url(&self, magnet_url: &str, save_path: Option<&str>) -> Result<()> {
        let url = format!("{}/api/v2/torrents/add", self.base_url);
        let mut params = vec![("urls", magnet_url.to_string())];
        
        if let Some(path) = save_path {
            params.push(("savepath", path.to_string()));
        }

        let response = self.client
            .post(&url)
            .form(&params)
            .send()
            .await?;

        if response.status().is_success() {
            info!("Successfully added torrent to qBittorrent");
            Ok(())
        } else {
            anyhow::bail!("Failed to add torrent: {}", response.status());
        }
    }

    pub async fn get_torrents(&self) -> Result<Vec<TorrentInfo>> {
        let url = format!("{}/api/v2/torrents/info", self.base_url);
        let response = self.client.get(&url).send().await?.json::<Vec<TorrentInfo>>().await?;
        Ok(response)
    }

    pub async fn delete_torrent(&self, hash: &str, delete_files: bool) -> Result<()> {
        let url = format!("{}/api/v2/torrents/delete", self.base_url);
        let params = [
            ("hashes", hash.to_string()),
            ("deleteFiles", delete_files.to_string()),
        ];
        
        let response = self.client
            .post(&url)
            .form(&params)
            .send()
            .await?;

        if response.status().is_success() {
            info!("Successfully deleted torrent: {}", hash);
            Ok(())
        } else {
            anyhow::bail!("Failed to delete torrent: {}", response.status());
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
pub struct TorrentInfo {
    pub name: String,
    pub hash: String,
    pub progress: f32,
    pub state: String,
    pub eta: u64,
    pub dlspeed: u64,
    pub size: u64,
}
