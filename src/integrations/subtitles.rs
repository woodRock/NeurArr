use anyhow::{Result, Context};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;
use tracing::{info, error};

pub struct SubtitleClient {
    client: Client,
    api_key: String,
}

#[derive(Deserialize, Debug)]
struct OpenSubtitlesSearchResponse {
    data: Vec<SubtitleData>,
}

#[derive(Deserialize, Debug)]
struct SubtitleData {
    id: String,
    attributes: SubtitleAttributes,
}

#[derive(Deserialize, Debug)]
struct SubtitleAttributes {
    release: String,
    language: String,
    files: Vec<SubtitleFile>,
}

#[derive(Deserialize, Debug)]
struct SubtitleFile {
    file_id: u64,
    file_name: String,
}

#[derive(Deserialize, Debug)]
struct DownloadResponse {
    link: String,
}

impl SubtitleClient {
    pub fn new() -> Result<Self> {
        let api_key = env::var("OPENSUBTITLES_API_KEY").unwrap_or_default();
        Ok(Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent("NeurArr v0.1.0")
                .build()?,
            api_key,
        })
    }

    pub async fn download_subtitles(&self, filename: &str, dest_path: &PathBuf) -> Result<()> {
        if self.api_key.is_empty() { return Ok(()); }

        info!("Searching subtitles for: {}", filename);
        let url = format!("https://api.opensubtitles.com/api/v1/subtitles?query={}", urlencoding::encode(filename));
        
        let res = self.client.get(&url)
            .header("Api-Key", &self.api_key)
            .send().await?
            .json::<OpenSubtitlesSearchResponse>().await?;

        if let Some(sub) = res.data.first() {
            if let Some(file) = sub.attributes.files.first() {
                info!("Found subtitle: {}. Downloading...", file.file_name);
                
                let dl_url = "https://api.opensubtitles.com/api/v1/download";
                let body = serde_json::json!({ "file_id": file.file_id });
                
                let dl_res = self.client.post(dl_url)
                    .header("Api-Key", &self.api_key)
                    .json(&body)
                    .send().await?
                    .json::<DownloadResponse>().await?;

                let content = self.client.get(&dl_res.link).send().await?.bytes().await?;
                let sub_path = dest_path.with_extension("srt");
                tokio::fs::write(&sub_path, content).await?;
                info!("Subtitles saved to: {:?}", sub_path);
            }
        }

        Ok(())
    }
}
