#[test]
fn test_improved_must_not_contain_filtering() {
    let cases = vec![
        // title, forbidden_tag, should_be_filtered
        ("Sousou no Frieren S01E01 [Tsundere-Raws]", "ts", false), // Should NOT match 'ts' inside a word
        ("Frieren S01E01 CAM", "cam", true), 
        ("Frieren S01E01 [TS]", "ts", true), 
        ("Frieren.S01E01-TS-X264", "ts", true), 
        ("Frieren S01E01 ts ", "ts", true), 
        ("Frieren.S01E01.ts.mkv", "ts", true),
        ("Frieren S01E01 TS-Rip", "ts", true),
        ("Frieren S01E01 (TS)", "ts", true),
    ];

    for (title, tag, expected_filtered) in cases {
        let t = title.to_lowercase();
        let tag = tag.to_lowercase();
        
        let title_parts: Vec<_> = t.split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .collect();
        
        let is_filtered = title_parts.contains(&tag.as_str());
            
        assert_eq!(is_filtered, expected_filtered, "Failed for title: '{}', tag: '{}'", title, tag);
    }
}

#[test]
fn test_title_string_match_logic() {
    let target_title = "Frieren: Beyond Journey's End";
    let torrent_titles = vec![
        "[Arg0] Frieren: Beyond Journey's End (2023) - S01E01 v3",
        "Frieren Beyond Journey's End S01E01",
        "FRIEREN: BEYOND JOURNEY'S END - 01",
    ];
    
    let normalize = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>();
    let target_norm = normalize(target_title);

    for tt in torrent_titles {
        let torrent_norm = normalize(tt);
        assert!(torrent_norm.contains(&target_norm), "Should match: {} (normalized: {}) against (target normalized: {})", tt, torrent_norm, target_norm);
    }
    
    let wrong_torrent = "[Arg0] Different Show S01E01";
    let wrong_norm = normalize(wrong_torrent);
    assert!(!wrong_norm.contains(&target_norm));
}

#[test]
fn test_alternative_title_query_generation() {
    let show_title = "Frieren: Beyond Journey's End";
    let ep_code = "S01E09";
    let alts = vec!["Sousou no Frieren", "葬送のフリーレン"];
    
    let mut queries = vec![format!("{} {}", show_title, ep_code)];
    for alt in alts {
        queries.push(format!("{} {}", alt, ep_code));
    }
    
    assert_eq!(queries.len(), 3);
    assert_eq!(queries[0], "Frieren: Beyond Journey's End S01E09");
    assert_eq!(queries[1], "Sousou no Frieren S01E09");
    assert_eq!(queries[2], "葬送のフリーレン S01E09");
}
