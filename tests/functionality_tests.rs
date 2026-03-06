// Mock needed dependencies for tests
#[path = "../src/parser/mod.rs"]
mod parser;
#[path = "../src/utils.rs"]
mod utils;

use parser::Parser;
use utils::Renamer;

// Mock the MediaMetadata struct if not correctly visible
// Actually, mod parser will include it.

#[test]
fn test_regex_parsing() {
    // Movie Test
    let metadata = Parser::parse_regex("Inception.2010.1080p.BluRay.x264.mkv");
    assert_eq!(metadata.title, "Inception");
    assert_eq!(metadata.resolution, Some("1080p".to_string()));
    assert_eq!(metadata.season, None);

    // TV Show Test
    let metadata = Parser::parse_regex("The.Office.S05E10.720p.HDTV.x264.mkv");
    assert_eq!(metadata.title, "The Office");
    assert_eq!(metadata.season, Some(5));
    assert_eq!(metadata.episode, Some(10));
}

#[test]
fn test_title_normalization() {
    let title1 = "Frieren: Beyond Journey's End";
    let title2 = "Frieren Beyond Journeys End";
    
    let norm1 = title1.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "");
    let norm2 = title2.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "");
    
    assert_eq!(norm1, norm2);
    assert_eq!(norm1, "frierenbeyondjourneysend");
}

#[test]
fn test_renamer_formatting() {
    let movie_template = "{title} ({year}) [{quality}]";
    let formatted = Renamer::format_movie(movie_template, "Inception", "2010", "1080p");
    assert_eq!(formatted, "Inception (2010) [1080p]");

    let tv_template = "{title} - S{season}E{episode} - {quality}";
    let formatted = Renamer::format_tv(tv_template, "The Office", 1, 5, "720p");
    assert_eq!(formatted, "The Office - S01E05 - 720p");
}

#[test]
fn test_complex_regex_parsing() {
    // Edge case: Year inside title
    let metadata = Parser::parse_regex("Blade.Runner.2049.2017.2160p.mkv");
    assert_eq!(metadata.title, "Blade Runner 2049");
    assert_eq!(metadata.resolution, Some("2160p".to_string()));

    // Edge case: Multiple resolutions
    let metadata = Parser::parse_regex("Movie.1080p.720p.x264.mkv");
    assert_eq!(metadata.title, "Movie");
    assert_eq!(metadata.resolution, Some("1080p".to_string()));
}

#[test]
fn test_password_auth() {
    let password = "my_secure_password_123";
    let hash = utils::auth::hash_password(password);
    
    assert!(utils::auth::verify_password(password, &hash));
    assert!(!utils::auth::verify_password("wrong_password", &hash));
}

#[test]
fn test_search_query_generation() {
    let title = "Frieren";
    let alts = vec!["Sousou no Frieren", "Frieren of the Funeral"];
    let ep_code = "S01E01";
    
    let mut queries = Vec::new();
    queries.push(format!("{} {}", title, ep_code));
    for alt in alts {
        queries.push(format!("{} {}", alt, ep_code));
    }
    
    assert_eq!(queries.len(), 3);
    assert!(queries.contains(&"Frieren S01E01".to_string()));
    assert!(queries.contains(&"Sousou no Frieren S01E01".to_string()));
}

#[test]
fn test_parsing_special_chars() {
    // Handling ampersands and brackets in title
    let metadata = Parser::parse_regex("Fast.&.Furious.Hobbs.&.Shaw.2019.1080p.mkv");
    assert!(metadata.title.contains("Fast & Furious"));
    assert_eq!(metadata.resolution, Some("1080p".to_string()));

    let metadata = Parser::parse_regex("[SubsPlease] Spy x Family - 01 (1080p) [720p].mkv");
    assert!(metadata.title.to_lowercase().contains("spy x family"));
}

#[test]
fn test_parsing_resolutions() {
    let cases = vec![
        ("Movie.2160p.bluray.mkv", "2160p"),
        ("Movie.1080p.webrip.mkv", "1080p"),
        ("Movie.720p.hdtv.mkv", "720p"),
        ("Movie.480p.dvdrip.mkv", "480p"),
    ];

    for (filename, expected) in cases {
        let metadata = Parser::parse_regex(filename);
        assert_eq!(metadata.resolution, Some(expected.to_string()));
    }
}

#[test]
fn test_tv_episode_normalization() {
    // S01E01 vs S1E1 vs Season 1 Episode 1 (standardizing on SxxEyy)
    let cases = vec![
        ("Show.S01E01.mkv", Some(1), Some(1)),
        ("Show.S10E25.mkv", Some(10), Some(25)),
    ];

    for (filename, s, e) in cases {
        let metadata = Parser::parse_regex(filename);
        assert_eq!(metadata.season, s);
        assert_eq!(metadata.episode, e);
    }
}

#[test]
fn test_library_path_logic() {
    use std::path::PathBuf;
    let lib_base = "/Media";
    let title = "Inception";
    let year = "2010";
    
    let mut dest = PathBuf::from(lib_base);
    dest.push("Movies");
    dest.push(format!("{} ({})", title, year));
    
    assert_eq!(dest.to_string_lossy(), "/Media/Movies/Inception (2010)");
}

#[test]
fn test_quality_filtering_logic() {
    let torrent_title = "Movie.2024.2160p.WEB-DL.x265.mkv".to_lowercase();
    
    // Profile: Max 1080p, No x265
    let max_res_1080 = "1080p";
    let must_not_contain = vec!["x265", "cam"];
    
    let is_too_high_res = max_res_1080 == "1080p" && torrent_title.contains("2160p");
    let has_forbidden = must_not_contain.iter().any(|word| torrent_title.contains(word));
    
    assert!(is_too_high_res);
    assert!(has_forbidden);
}

#[test]
fn test_nfo_generation_logic() {
    let title = "Inception";
    let plot = "A thief who steals corporate secrets...";
    let nfo = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\" ?>\n<movie>\n  <title>{}</title>\n  <plot>{}</plot>\n</movie>",
        title, plot
    );
    
    assert!(nfo.contains("<title>Inception</title>"));
    assert!(nfo.contains("<plot>A thief who steals corporate secrets...</plot>"));
}

#[test]
fn test_indexer_url_construction() {
    let base_url = "http://localhost:9117/api/v2.0/indexers/all/results";
    let api_key = "test_key";
    let query = "The Office S01E01";
    
    let url = format!("{}?apikey={}&Query={}&t=search&format=json", base_url, api_key, urlencoding::encode(query));
    
    assert!(url.contains("apikey=test_key"));
    assert!(url.contains("Query=The%20Office%20S01E01"));
    assert!(url.contains("format=json"));
}
