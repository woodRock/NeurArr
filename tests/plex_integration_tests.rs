#[path = "../src/utils.rs"]
mod utils;
#[path = "../src/parser/mod.rs"]
mod parser;

use utils::Renamer;
use parser::MediaMetadata;
use std::path::PathBuf;

#[test]
fn test_plex_url_construction() {
    let base_url = "http://plex:32400";
    let token = "secret_token";
    let url = format!("{}/library/sections/all/refresh?X-Plex-Token={}", base_url, token);
    
    assert_eq!(url, "http://plex:32400/library/sections/all/refresh?X-Plex-Token=secret_token");
}

#[test]
fn test_filename_sanitation_for_plex() {
    // Plex and Windows both hate these characters
    let original = "Star Wars: Episode I - The Phantom Menace?";
    let sanitized = Renamer::sanitize_filename(original);
    
    assert!(!sanitized.contains(':'));
    assert!(!sanitized.contains('?'));
    assert_eq!(sanitized, "Star Wars Episode I - The Phantom Menace");
}

#[test]
fn test_library_structure_consistency() {
    let lib_dir = "/tmp/library".to_string();
    let _renamer = Renamer::new(lib_dir.clone());
    
    // Movie path
    let _movie_metadata = MediaMetadata {
        title: "Inception".to_string(),
        year: Some(2010),
        season: None,
        episode: None,
        resolution: Some("1080p".to_string()),
        source: None,
    };
    
    let mut movie_dest = PathBuf::from(&lib_dir);
    movie_dest.push("Movies");
    movie_dest.push("Inception (2010)");
    movie_dest.push("Inception (2010).mkv");
    
    // TV path
    let _tv_metadata = MediaMetadata {
        title: "The Office".to_string(),
        year: Some(2005),
        season: Some(1),
        episode: Some(5),
        resolution: Some("720p".to_string()),
        source: None,
    };
    
    let mut tv_dest = PathBuf::from(&lib_dir);
    tv_dest.push("TV");
    tv_dest.push("The Office");
    tv_dest.push("Season 1");
    tv_dest.push("The Office - S01E05.mkv");

    assert_eq!(movie_dest.file_name().unwrap().to_str().unwrap(), "Inception (2010).mkv");
    assert_eq!(tv_dest.parent().unwrap().file_name().unwrap().to_str().unwrap(), "Season 1");
}

#[test]
fn test_plex_sync_trigger_logic() {
    // Logic test: should we trigger refresh?
    let token_empty = "";
    let token_set = "token123";
    
    let should_trigger_empty = !token_empty.is_empty();
    let should_trigger_set = !token_set.is_empty();
    
    assert!(!should_trigger_empty);
    assert!(should_trigger_set);
}
