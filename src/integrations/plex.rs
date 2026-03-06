use anyhow::{Context, Result};
use reqwest::Client;
use std::env;
use tracing::info;

#[allow(dead_code)]
pub struct PlexClient {
    client: Client,
    base_url: String,
    token: String,
}

impl PlexClient {
    #[allow(dead_code)]
    pub fn new() -> Result<Self> {
        let base_url = env::var("PLEX_BASE_URL").unwrap_or_else(|_| "http://localhost:32400".to_string());
        let token = env::var("PLEX_TOKEN").context("PLEX_TOKEN not set")?;
        
        Ok(Self {
            client: Client::new(),
            base_url,
            token,
        })
    }

    #[allow(dead_code)]
    pub async fn refresh_library(&self) -> Result<()> {
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
