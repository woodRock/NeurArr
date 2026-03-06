use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::env;
use tracing::info;

#[derive(Serialize, Deserialize)]
struct OllamaChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
#[allow(dead_code)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaChatMessage>,
    stream: bool,
    options: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    message: OllamaChatMessage,
}

pub struct OllamaClient {
    client: Client,
    base_url: String,
    model: String,
}

impl OllamaClient {
    pub fn new() -> Result<Self> {
        let base_url = env::var("OLLAMA_BASE_URL").unwrap_or_else(|_| "http://localhost:11434".to_string());
        let model = env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen3.5:0.8b".to_string());
        
        Ok(Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()?,
            base_url,
            model,
        })
    }

    pub async fn chat(&self, system: &str, user: &str, is_json: bool) -> Result<String> {
        info!("Sending chat request to Ollama ({})", self.model);
        
        let url = format!("{}/api/chat", self.base_url);
        let request = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user }
            ],
            "stream": false,
            "format": if is_json { "json" } else { "" },
            "options": {
                "temperature": 0.1,
                "num_predict": 512
            }
        });

        let response = self.client
            .post(&url)
            .json(&request)
            .send()
            .await?;
            
        if !response.status().is_success() {
            let err_text = response.text().await?;
            anyhow::bail!("Ollama API error: {}", err_text);
        }

        let chat_res = response.json::<OllamaChatResponse>().await?;
        let content = chat_res.message.content.trim().to_string();
        
        Ok(content)
    }

    pub async fn parse_scene_release(&self, filename: &str) -> Result<String> {
        let system = "You are a media parser. Return a JSON object with these fields: title, season, episode, resolution, source. Return ONLY the JSON block.";
        let user = format!("Parse: {}", filename);
        self.chat(system, &user, true).await
    }

    pub async fn rewrite_summary(&self, summary: &str) -> Result<String> {
        let system = "Rewrite this movie summary to be spoiler-free. Keep the setup, remove twists and endings. Return only the rewritten text.";
        let user = format!("Rewrite: {}", summary);
        self.chat(system, &user, false).await
    }

    pub async fn verify_torrent_match(&self, target_title: &str, torrent_title: &str) -> Result<bool> {
        let prompt = format!(
            "Task: Does the torrent filename match the EXACT movie/show title?
Target Title: '{}'
Torrent Filename: '{}'

Rules:
1. Return 'true' ONLY if it is an exact match or a verified alternative title.
2. If the torrent filename contains a year (e.g. 2024, 2005), it MUST match the correct release year for the target media. If it is a different year, return 'false'.
3. Return 'false' if it is a 'Live', 'Theatre', 'Stage', 'Musical', 'Documentary', or 'Behind the Scenes' version of the show.
4. Return 'false' if it is a different show with a similar name.
5. Answer ONLY with 'true' or 'false'.",
            target_title, torrent_title
        );
        
        let response = self.chat("You are a strict media matching expert. Answer ONLY 'true' or 'false'.", &prompt, false).await?;
        let res_lower = response.to_lowercase();
        info!("LLM raw response for match: '{}'", res_lower);
        
        if res_lower.is_empty() {
            return Ok(false);
        }
        
        Ok(res_lower.contains("true") || (res_lower.contains("yes") && !res_lower.contains("no")))
    }

    pub async fn semantic_search_translate(&self, prompt: &str) -> Result<String> {
        let system = "You are a movie and TV recommendation expert. Based on the user's description, identify 5 specific movies or TV shows that perfectly match the request. Return ONLY the titles as a comma-separated list. No explanations, no numbering, no extra text.";
        self.chat(system, prompt, false).await
    }
}
