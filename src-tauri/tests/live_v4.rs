//! Opt-in live checks for v0.4 against the real machine:
//! `cargo test --test live_v4 -- --ignored --nocapture --test-threads 1`

use agentos_studio_lib::{projects, skills};

#[test]
#[ignore = "needs a local Hermes install"]
fn skills_tree_reads_real_skills() {
    let all = skills::list();
    println!("installed skills: {}", all.len());
    for s in all.iter().take(5) {
        println!(
            "  {:<28} {:<24} {}",
            s.name,
            s.category,
            &s.description.chars().take(60).collect::<String>()
        );
    }
    assert!(all.len() > 5, "expected the bundled skills to be visible");
    let known = all
        .iter()
        .find(|s| s.name == "agentos-studio-dev")
        .expect("our own skill is installed");
    assert!(known.description.to_lowercase().contains("agentos studio"));
    let body = skills::read("agentos-studio-dev").unwrap();
    assert!(body.contains("Release procedure") || body.contains("Gates"));
    println!("read {} bytes of SKILL.md", body.len());
    let missing = skills::read("definitely-not-a-skill");
    assert!(missing.is_err());
}

#[test]
#[ignore = "needs a local Hermes install"]
fn projects_list_matches_hermes() {
    let all = projects::list().unwrap();
    println!("projects: {}", all.len());
    for p in &all {
        println!(
            "  {:<24} slug={:<20} folders={} sessions={} archived={}",
            p.name,
            p.slug,
            p.folders.len(),
            p.session_count,
            p.archived
        );
    }
    // The rule under test is the path matcher, not the number of projects.
    assert!(projects::under(
        "C:/Users/Konet/agentos-studio/src",
        "c:/users/konet/agentos-studio"
    ));
    assert!(!projects::under(
        "C:/Users/Konet/agentos-studio-old",
        "C:/Users/Konet/agentos-studio"
    ));
}

#[test]
#[ignore = "needs network access to the skill registries"]
fn skill_search_returns_results() {
    let hits = skills::search_hub("excel", 3).unwrap();
    println!("hub hits: {}", hits.len());
    for h in &hits {
        println!("  {:<28} {}", h.name, h.identifier);
    }
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| !h.identifier.is_empty()));
}
