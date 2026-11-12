//! Opt-in: `cargo test --test live -- live_mission_loop:: --ignored --nocapture`
//! Exercises create -> dispatch -> worker completion -> scheduler -> reviewer against real state.

use agentos_studio_lib::{automation, missions, procs, review_agent, settings, vault::Vault};

#[test]
#[ignore = "writes a real mission, task and review"]
fn mission_loop_end_to_end() {
    let pr = std::env::var("MISSION_PR")
        .unwrap_or_else(|_| "https://github.com/konethegreat/agentos-studio/pull/22".into());
    let v = Vault::new(settings::vault_path());
    if let Ok(existing) = std::env::var("MISSION_ID") {
        let reviewer = if procs::find("claude").is_some() {
            "claude-code"
        } else {
            "hermes"
        };
        let done = review_agent::review(&v, &existing, reviewer).expect("review");
        println!(
            "reviewed by {reviewer}: verdict={:?} note={:?} status={}",
            done.review_verdict, done.review_note, done.status
        );
        return;
    }

    let m = missions::create(
        "agentos-studio",
        "Add a probe line to the updater docs",
        "docs/UPDATER.md contains one added probe line and nothing else changed",
        "implementer",
        "hermes",
        "claude-subscription",
        "",
        0.0,
    )
    .unwrap();
    let m = missions::start(&v, &m.id).unwrap();
    println!("mission {} dispatched as {:?}", m.id, m.task_id);

    let task = m.task_id.clone().expect("task id");
    v.claim_task(&task, "hermes").unwrap();
    v.complete_task(&task, "hermes", &format!("Added the probe line. PR: {pr}"))
        .unwrap();

    let r = automation::tick(&v);
    println!(
        "tick moved={:?} dispatched={:?}",
        r.moved_to_review, r.dispatched
    );
    let after = missions::get(&m.id).unwrap().unwrap();
    println!(
        "status={} pr={:?} verdict={:?}",
        after.status, after.pr_url, after.review_verdict
    );
    assert_eq!(
        after.status, "review",
        "a completed task must reach review, never done"
    );

    let reviewer = if procs::find("claude").is_some() {
        "claude-code"
    } else {
        "hermes"
    };
    match review_agent::review(&v, &m.id, reviewer) {
        Ok(done) => println!(
            "reviewed by {reviewer}: verdict={:?} note={:?} status={}",
            done.review_verdict, done.review_note, done.status
        ),
        Err(e) => println!("review failed: {e:#}"),
    }
}
