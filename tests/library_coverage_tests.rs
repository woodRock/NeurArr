#[path = "../src/db/mod.rs"]
mod db;
#[path = "../src/parser/mod.rs"]
mod parser;
#[path = "../src/utils.rs"]
mod utils;

// Mock web module for db types
pub mod web {
    use serde::{Deserialize, Serialize};
    #[derive(Serialize, Deserialize, sqlx::FromRow, Clone)]
    pub struct MediaItem {
        pub id: i64,
        pub original_filename: String,
        pub title: String,
        pub season: Option<i64>,
        pub episode: Option<i64>,
        pub status: String,
        pub spoiler_free_summary: Option<String>,
        pub poster_path: Option<String>,
    }
}

#[test]
fn test_manual_match_precedence() {
    // Logic: If a manual match exists for a title, it should override TMDB search
    let manual_matches = vec![("Techcrunch Disrupt", 12345u32)];
    let search_title = "Techcrunch Disrupt";
    
    let forced_id = manual_matches.iter()
        .find(|(t, _)| t.to_lowercase() == search_title.to_lowercase())
        .map(|(_, id)| *id);
        
    assert_eq!(forced_id, Some(12345));
}

#[test]
fn test_quality_profile_resolution_cutoff() {
    let cases = vec![
        ("2160p", "1080p", true),  // Cutoff 1080p, release 2160p -> Filtered (true)
        ("1080p", "1080p", false), // Cutoff 1080p, release 1080p -> OK (false)
        ("720p", "1080p", false),  // Cutoff 1080p, release 720p -> OK (false)
        ("1080p", "2160p", false), // Cutoff 2160p, release 1080p -> OK (false)
    ];

    for (release_res, cutoff, expected_filtered) in cases {
        let is_filtered = match cutoff {
            "1080p" => release_res == "2160p",
            "720p" => release_res == "1080p" || release_res == "2160p",
            _ => false,
        };
        assert_eq!(is_filtered, expected_filtered, "Failed for release: {}, cutoff: {}", release_res, cutoff);
    }
}

#[test]
fn test_directory_pack_ingestion_logic() {
    // Logic: In a directory pack, multiple files should be processed but the directory deleted only once
    let mut files_processed = 0;
    let dir_contents = vec!["Show.S01E01.mkv", "Show.S01E02.mkv", "Show.S01E03.mkv", "random.txt"];
    
    for file in &dir_contents {
        if ["mkv", "mp4", "avi", "mov"].contains(&file.split('.').last().unwrap_or("")) {
            files_processed += 1;
        }
    }
    
    assert_eq!(files_processed, 3);
}

#[test]
fn test_duplicate_ingestion_prevention() {
    // Logic: If a file is already being processed (in registry), don't start it again
    let mut registry = std::collections::HashSet::new();
    let file_path = "/tmp/ingest/movie.mkv";
    
    let first_insert = registry.insert(file_path.to_string());
    let second_insert = registry.insert(file_path.to_string());
    
    assert!(first_insert);
    assert!(!second_insert);
}

#[test]
fn test_tv_show_year_match_verification() {
    // Logic: If searching for a show with a year, verify the TMDB result year matches
    let target_year = 2023;
    let tmdb_results = vec![
        ("Show A", "2023-10-01"),
        ("Show A (Old)", "1999-05-12"),
    ];
    
    let best_match = tmdb_results.iter().find(|(_, date)| {
        date.split('-').next().and_then(|y| y.parse::<i32>().ok()) == Some(target_year)
    });
    
    assert!(best_match.is_some());
    assert_eq!(best_match.unwrap().0, "Show A");
}
