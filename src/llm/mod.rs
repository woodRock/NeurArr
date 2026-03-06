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
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaChatMessage>,
    stream: bool,
    // Note: We are removing the 'format' field to avoid the JSON mode hang
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

    pub async fn chat(&self, system: &str, user: &str) -> Result<String> {
        info!("Sending chat request to Ollama ({})", self.model);
        
        let url = format!("{}/api/chat", self.base_url);
        let request = OllamaChatRequest {
            model: self.model.clone(),
            messages: vec![
                OllamaChatMessage { role: "system".to_string(), content: system.to_string() },
                OllamaChatMessage { role: "user".to_string(), content: user.to_string() },
            ],
            stream: false,
            options: Some(serde_json::json!({
                "temperature": 0.1, // Slightly above 0 to avoid loops
                "num_predict": 512,
            })),
        };

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
        self.chat(system, &user).await
    }

    pub async fn rewrite_summary(&self, summary: &str) -> Result<String> {
        let system = "Rewrite this movie summary to be spoiler-free. Keep the setup, remove twists and endings. Return only the rewritten text.";
        let user = format!("Rewrite: {}", summary);
        self.chat(system, &user).await
    }
}
