use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use regex::Regex;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MediaMetadata {
    pub title: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub resolution: Option<String>,
    pub source: Option<String>,
}

pub struct Parser;

impl Parser {
    pub fn parse_llm_json(json_str: &str) -> Result<MediaMetadata> {
        // Try to find JSON within brackets first
        let json_part = if let (Some(start), Some(end)) = (json_str.find('{'), json_str.rfind('}')) {
            &json_str[start..=end]
        } else {
            json_str.trim()
        };
        
        let metadata: MediaMetadata = serde_json::from_str(json_part)
            .context(format!("Failed to parse model output as MediaMetadata JSON. Raw output: {}", json_str))?;
            
        Ok(metadata)
    }

    pub fn parse_regex(filename: &str) -> MediaMetadata {
        let mut title = filename.to_string();
        let mut resolution = None;
        let mut season = None;
        let mut episode = None;

        // 1. Extract resolution
        let res_re = Regex::new(r"(2160p|1080p|720p|480p)").unwrap();
        if let Some(caps) = res_re.captures(filename) {
            resolution = Some(caps[1].to_string());
        }

        // 2. Extract S01E01 style
        let tv_re = Regex::new(r"(?i)S(\d+)E(\d+)").unwrap();
        if let Some(caps) = tv_re.captures(filename) {
            season = caps[1].parse().ok();
            episode = caps[2].parse().ok();
        }

        // 3. Clean title (take everything before the year or resolution)
        let clean_re = Regex::new(r"(?i)(^.*?)[\s\.](19|20)\d{2}|(2160p|1080p|720p|480p)|S\d+E\d+").unwrap();
        if let Some(caps) = clean_re.captures(filename) {
            if let Some(mat) = caps.get(1) {
                title = mat.as_str().replace('.', " ").trim().to_string();
            }
        } else {
            // Fallback: just remove dots and extension
            title = title.replace('.', " ");
            if let Some(idx) = title.rfind(' ') {
                title.truncate(idx);
            }
        }

        MediaMetadata {
            title,
            season,
            episode,
            resolution,
            source: None,
        }
    }
}
