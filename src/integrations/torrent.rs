use anyhow::Result;
use reqwest::{Client, cookie::Jar};
use serde::Serialize;
use std::env;
use std::sync::Arc;
use tracing::info;

#[derive(Serialize)]
#[allow(dead_code)]
struct LoginRequest {
    username: String,
    password: String,
}

#[allow(dead_code)]
pub struct QBittorrentClient {
    client: Client,
    base_url: String,
}

impl QBittorrentClient {
    #[allow(dead_code)]
    pub fn new() -> Result<Self> {
        let base_url = env::var("QBITTORRENT_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
        let jar = Arc::new(Jar::default());
        
        let client = Client::builder()
            .cookie_provider(jar)
            .build()?;

        Ok(Self { client, base_url })
    }

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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
}
