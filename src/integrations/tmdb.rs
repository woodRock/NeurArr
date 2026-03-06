use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::env;

#[derive(Debug, Deserialize)]
pub struct TmdbSearchResult {
    pub results: Vec<TmdbMedia>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[allow(dead_code)]
pub struct TmdbMedia {
    pub id: u32,
    pub title: Option<String>,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub release_date: Option<String>,
    pub first_air_date: Option<String>,
    pub media_type: Option<String>,
    pub poster_path: Option<String>,
}

#[derive(Clone)]
pub struct TmdbClient {
    client: Client,
    api_key: String,
}

impl TmdbClient {
    pub fn new() -> Result<Self> {
        let api_key = env::var("TMDB_API_KEY").context("TMDB_API_KEY not set")?;
        Ok(Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?,
            api_key,
        })
    }

    pub async fn search_movie(&self, query: &str) -> Result<Vec<TmdbMedia>> {
        let url = format!(
            "https://api.themoviedb.org/3/search/movie?api_key={}&query={}",
            self.api_key,
            urlencoding::encode(query)
        );
        let response = self.client.get(&url).send().await?.json::<TmdbSearchResult>().await?;
        Ok(response.results)
    }

    pub async fn search_tv(&self, query: &str) -> Result<Vec<TmdbMedia>> {
        let url = format!(
            "https://api.themoviedb.org/3/search/tv?api_key={}&query={}",
            self.api_key,
            urlencoding::encode(query)
        );
        let response = self.client.get(&url).send().await?.json::<TmdbSearchResult>().await?;
        Ok(response.results)
    }

    #[allow(dead_code)]
    pub async fn get_movie_details(&self, id: u32) -> Result<TmdbMedia> {
        let url = format!(
            "https://api.themoviedb.org/3/movie/{}?api_key={}",
            id, self.api_key
        );
        let response = self.client.get(&url).send().await?.json::<TmdbMedia>().await?;
        Ok(response)
    }

    pub async fn get_tv_details(&self, id: u32) -> Result<TmdbMedia> {
        let url = format!(
            "https://api.themoviedb.org/3/tv/{}?api_key={}",
            id, self.api_key
        );
        let response = self.client.get(&url).send().await?.json::<TmdbMedia>().await?;
        Ok(response)
    }

    pub async fn get_upcoming_movies(&self) -> Result<Vec<TmdbMedia>> {
        let url = format!(
            "https://api.themoviedb.org/3/movie/upcoming?api_key={}",
            self.api_key
        );
        let response = self.client.get(&url).send().await?.json::<TmdbSearchResult>().await?;
        Ok(response.results)
    }

    pub async fn get_trending_tv(&self) -> Result<Vec<TmdbMedia>> {
        let url = format!(
            "https://api.themoviedb.org/3/trending/tv/week?api_key={}",
            self.api_key
        );
        let response = self.client.get(&url).send().await?.json::<TmdbSearchResult>().await?;
        Ok(response.results)
    }

    pub async fn get_alternative_titles(&self, id: u32, is_tv: bool) -> Result<Vec<String>> {
        let media_type = if is_tv { "tv" } else { "movie" };
        let url = format!(
            "https://api.themoviedb.org/3/{}/{}/alternative_titles?api_key={}",
            media_type, id, self.api_key
        );
        
        let response = self.client.get(&url).send().await?.json::<serde_json::Value>().await?;
        
        let mut titles = Vec::new();
        if let Some(results) = response.get("results").or(response.get("titles")) {
            if let Some(arr) = results.as_array() {
                for item in arr {
                    if let Some(title) = item.get("title").or(item.get("name")) {
                        if let Some(s) = title.as_str() {
                            titles.push(s.to_string());
                        }
                    }
                }
            }
        }
        Ok(titles)
    }
}
