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
    #[allow(dead_code)]
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
        let mut resolution = None;
        let mut season = None;
        let mut episode = None;

        // 1. Extract resolution
        let res_re = Regex::new(r"(?i)(2160p|1080p|720p|480p)").unwrap();
        if let Some(caps) = res_re.captures(filename) {
            resolution = Some(caps[1].to_lowercase());
        }

        // 2. Extract S01E01 style
        let tv_re = Regex::new(r"(?i)S(\d+)E(\d+)").unwrap();
        if let Some(caps) = tv_re.captures(filename) {
            season = caps[1].parse().ok();
            episode = caps[2].parse().ok();
        }

        // 3. Clean title: find the split point
        let mut split_point = filename.len();
        
        // Find all tags: years, resolutions, or TV tags
        let all_tags_re = Regex::new(r"(?i)[\s\.]((19|20)\d{2}|2160p|1080p|720p|480p|S\d+E\d+)").unwrap();
        let all_matches: Vec<_> = all_tags_re.find_iter(filename).collect();
        
        if !all_matches.is_empty() {
            // Logic: A year is only a "split point" if it's the LAST year in the filename.
            // Other tags (resolution, TV) are always split points.
            
            for mat in &all_matches {
                let tag_text = mat.as_str().to_lowercase();
                let is_year = Regex::new(r"(19|20)\d{2}").unwrap().is_match(&tag_text);
                
                if is_year {
                    // Check if there's another year after this one
                    let remaining_text = &filename[mat.end()..];
                    let has_later_year = Regex::new(r"(19|20)\d{2}").unwrap().is_match(remaining_text);
                    
                    if !has_later_year {
                        split_point = mat.start();
                        break;
                    }
                } else {
                    // Not a year (resolution or TV tag), split here immediately
                    split_point = mat.start();
                    break;
                }
            }
        } else {
            // Fallback: split at extension
            split_point = filename.rfind('.').unwrap_or(filename.len());
        }

        let title = filename[..split_point].replace('.', " ").trim().to_string();

        MediaMetadata {
            title,
            season,
            episode,
            resolution,
            source: None,
        }
    }
}
