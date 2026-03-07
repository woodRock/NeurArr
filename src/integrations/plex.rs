use anyhow::{Context, Result};
use reqwest::Client;
use std::env;
use tracing::info;

pub struct PlexClient {
    client: Client,
    base_url: String,
    token: String,
}

impl PlexClient {
    pub fn new() -> Result<Self> {
        let base_url = env::var("PLEX_BASE_URL").unwrap_or_else(|_| "http://localhost:32400".to_string());
        let token = env::var("PLEX_TOKEN").unwrap_or_else(|_| "".to_string());
        
        Ok(Self {
            client: Client::new(),
            base_url,
            token,
        })
    }

    pub async fn refresh_library(&self) -> Result<()> {
        if self.token.is_empty() {
            return Ok(());
        }
        info!("Triggering Plex library refresh...");
        let url = format!("{}/library/sections/all/refresh?X-Plex-Token={}", self.base_url, self.token);
        let response = self.client.get(&url).send().await?;
        
        if response.status().is_success() {
            info!("Successfully triggered Plex library refresh");
        } else {
            anyhow::bail!("Failed to trigger Plex library refresh: {}", response.status());
        }
        
        Ok(())
    }
}
