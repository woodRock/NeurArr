use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;
use std::env;
use tracing::{info, error};

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct TorznabItem {
    pub title: String,
    pub link: String,
    pub size: u64,
    pub seeders: u32,
    pub indexer: String,
}

#[derive(Deserialize, Debug)]
struct JackettResponse {
    #[serde(rename = "Results")]
    results: Option<Vec<JackettResult>>,
}

#[derive(Deserialize, Debug)]
struct JackettResult {
    #[serde(rename = "Title")]
    title: String,
    #[serde(rename = "Link")]
    link: Option<String>,
    #[serde(rename = "Guid")]
    guid: Option<String>,
    #[serde(rename = "Size")]
    size: u64,
    #[serde(rename = "Seeders")]
    seeders: Option<u32>,
    #[serde(rename = "Tracker")]
    tracker: Option<String>,
}

pub struct IndexerClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl IndexerClient {
    pub fn new() -> Result<Self> {
        let base_url = env::var("INDEXER_URL").unwrap_or_else(|_| "http://localhost:9117/api/v2.0/indexers/all/results".to_string());
        let api_key = env::var("INDEXER_API_KEY").unwrap_or_else(|_| "".to_string());
        
        Ok(Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            base_url,
            api_key,
        })
    }

    pub async fn search(&self, query: &str) -> Result<Vec<TorznabItem>> {
        if self.api_key.is_empty() {
            info!("Indexer API key not set, skipping search for: {}", query);
            return Ok(vec![]);
        }

        // Use Torznab-compatible parameters (t=search, q=query)
        // This is much more reliable for automation tools than the native UI API
        let url = if self.base_url.contains("torznab") {
            format!("{}?apikey={}&t=search&q={}&format=json", self.base_url, self.api_key, urlencoding::encode(query))
        } else {
            format!("{}?apikey={}&Query={}&t=search&format=json", self.base_url, self.api_key, urlencoding::encode(query))
        };

        info!("Requesting indexer search for: {}", query);
        
        let response = self.client.get(&url).send().await?;
        
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            error!("Indexer search failed with status {}: {}", status, text);
            return Ok(vec![]);
        }

        let jackett_res = response.json::<JackettResponse>().await?;
        let raw_results = jackett_res.results.unwrap_or_default();
        info!("Indexer found {} raw results", raw_results.len());
        
        let items: Vec<_> = raw_results.into_iter()
            .map(|r| TorznabItem {
                title: r.title,
                link: r.link.or(r.guid).unwrap_or_default(),
                size: r.size,
                seeders: r.seeders.unwrap_or(0),
                indexer: r.tracker.unwrap_or_else(|| "Unknown".to_string()),
            })
            .collect();

        let with_seeders: Vec<_> = items.into_iter().filter(|item| item.seeders > 0).collect();
        info!("Indexer has {} results after seeder filter", with_seeders.len());
            
        Ok(with_seeders)
    }
}
