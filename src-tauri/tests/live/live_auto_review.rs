//! Live: the automatic reviewer for Claude Code's work runs on another provider (Antigravity)
//! with the diff, the CI checks and the repository's conventions, and records a verdict.
//!
//! Opt-in, one Antigravity turn (flat subscription):
//!   cargo test --test live -- live_auto_review:: --ignored --nocapture --test-threads 1
//!
//! Uses a temporary studio.db, so the running app never sees the mission, and an existing pull
//! request in the private sandbox (agentos-sandbox-1#8, merged then reverted), so nothing on
//! GitHub changes. `learning.done.<id>` is set first so an approval writes no lesson notes into
//! the real vault.

use agentos_studio_lib::{missions, review_agent, store, vault::Vault};

#[test]
#[ignore]
fn claude_codes_work_is_reviewed_on_another_provider_without_a_click() {
    let db = tempfile::tempdir().unwrap();
    std::env::set_var("AGENTOS_STUDIO_DB", db.path().join("studio.db"));
    // The conventions lookup indexes the vault; keep that index out of the real app data.
    std::env::set_var("AGENTOS_STUDIO_MEMORY_DIR", db.path().join("memory"));
    let vdir = tempfile::tempdir().unwrap();
    let v = Vault::new(vdir.path());
    v.scaffold().unwrap();

    let m = missions::create(
        "agentos-sandbox-1",
        "Add subtract(a, b) to lib.js with a test, and document it in README.md",
        "- lib.js exports subtract(a, b) returning a - b\n- a test covers subtract\n- README.md documents subtract",
        "implementer",
        "claude-code",
        "",
        "",
        0.0,
    )
    .unwrap();
    let c = store::open().unwrap();
    store::kv_set(&c, &format!("learning.done.{}", m.id), "1").unwrap();
    missions::set_pr(
        &m.id,
        "https://github.com/konethegreat/agentos-sandbox-1/pull/8",
    )
    .unwrap();
    missions::dispatch(&m.id, "TASK-live-auto-review").unwrap();
    let m = missions::submit_for_review(&m.id, 0.0).unwrap();
    assert!(review_agent::auto_review_due(
        &m,
        review_agent::auto_state(&m.id).as_deref(),
        store::now(),
        true
    ));

    let started = std::time::Instant::now();
    let judged = review_agent::run_auto(&v, &m.id).expect("the automatic review runs");
    println!(
        "{} reviewed in {:.0}s by {:?}: {:?} {:?}",
        m.id,
        started.elapsed().as_secs_f64(),
        judged.reviewer,
        judged.review_verdict,
        judged.review_note
    );
    assert_eq!(
        judged.reviewer.as_deref(),
        Some("antigravity"),
        "Claude Code's work is judged on another provider"
    );
    assert!(matches!(
        judged.review_verdict.as_deref(),
        Some("approve") | Some("reject")
    ));
    assert!(!judged.review_note.unwrap_or_default().trim().is_empty());
    assert_eq!(
        review_agent::auto_state(&m.id).as_deref(),
        Some("done"),
        "one review per round"
    );
    let events = agentos_studio_lib::agent_log::for_mission(&m.id).unwrap();
    assert!(
        events.iter().any(|e| e
            .summary
            .contains("automatic review by antigravity (another provider)")),
        "the timeline shows who reviewed and why"
    );

    let after = missions::get(&m.id).unwrap().unwrap();
    if after.status != "done" && after.status != "failed" {
        missions::block(&m.id, "live test cleanup", true).unwrap();
    }
    missions::delete(&m.id).unwrap();
    std::env::remove_var("AGENTOS_STUDIO_DB");
    std::env::remove_var("AGENTOS_STUDIO_MEMORY_DIR");
}
