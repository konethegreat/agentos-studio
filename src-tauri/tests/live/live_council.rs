//! Opt-in: `cargo test --test live -- live_council:: --ignored --nocapture --test-threads 1`
//!
//! Phase C item 4 against the real machine: a design decision mission in `agentos-sandbox-1` is
//! started through `missions::start`, which seats a council (agents on different providers, Hermes
//! as synthesizer), collects each position, writes the ADR in the mission worktree, pushes it and
//! opens a pull request. The tick then moves the mission to review. Afterwards the pull request is
//! closed with its branch, and the worktree, mission, exchange rows and the Hermes sessions the
//! council used are deleted.

use std::process::Command;
use std::time::{Duration, Instant};

use agentos_studio_lib::{
    agent_log, automation, council, missions, procs, review_agent, vault::Vault, worktree,
};

const REPO: &str = "agentos-sandbox-1";

#[test]
#[ignore = "runs real agents and opens a real pull request"]
fn a_council_writes_an_adr_pull_request_with_every_position() {
    let d = tempfile::tempdir().unwrap();
    let v = Vault::new(d.path().join("vault"));
    v.scaffold().unwrap();
    let dir = review_agent::project_dir(REPO).expect("sandbox project");
    let m = missions::create(
        REPO,
        "Should this sandbox keep its hand-rolled test.js runner or move to node:test?",
        "The ADR states one decision, each agent's position, and the consequences for npm test",
        "planner",
        "hermes",
        "",
        "",
        0.0,
    )
    .unwrap();
    missions::set_kind(&m.id, missions::KIND_DECISION).unwrap();
    let started = missions::start(&v, &m.id).expect("council starts");
    assert!(council::is_council(started.task_id.as_deref()));
    let deadline = Instant::now() + Duration::from_secs(20 * 60);
    let mut after = missions::get(&m.id).unwrap().unwrap();
    while after.status == "running" && Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(10));
        automation::tick(&v);
        after = missions::get(&m.id).unwrap().unwrap();
        println!("state {:?} status {}", council::state(&m.id), after.status);
    }
    let events = agent_log::for_mission(&m.id).unwrap();
    let wt = worktree::recorded(&m.id);
    let adr = wt.as_ref().and_then(|w| {
        std::fs::read_dir(w.join("docs").join("adr"))
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "md"))
            .and_then(|p| std::fs::read_to_string(p).ok())
    });
    let note = v.list_memory_notes().into_iter().find(|n| {
        v.read_note(&n.relative_path)
            .is_ok_and(|t| t.contains("Council decision for mission") && t.contains(&m.id))
    });
    for e in &events {
        println!(
            "{} {}: {}",
            e.kind,
            e.from_agent,
            e.summary.chars().take(140).collect::<String>()
        );
    }
    println!("pr {:?}", after.pr_url);
    println!(
        "{}",
        adr.clone()
            .unwrap_or_default()
            .chars()
            .take(1500)
            .collect::<String>()
    );

    // Clean up.
    if let Some(url) = &after.pr_url {
        let _ = Command::new("gh")
            .args(["pr", "close", url, "--delete-branch"])
            .current_dir(&dir)
            .env("PATH", procs::search_path())
            .output();
    }
    let mut sessions = Vec::new();
    for e in &events {
        if let Some((_, sid)) = e.detail.rsplit_once("Hermes session: ") {
            let sid = sid.trim().to_string();
            let ok = Command::new(procs::find("hermes").unwrap())
                .args(["sessions", "delete", &sid, "--yes"])
                .env("PATH", procs::search_path())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            sessions.push((sid, ok));
        }
    }
    let branch = worktree::recorded_branch(&m.id);
    let _ = worktree::discard(&dir, &m.id);
    if matches!(after.status.as_str(), "review" | "running") {
        let _ = missions::block(&m.id, "live test cleanup", true);
    }
    let _ = missions::delete(&m.id);
    let _ = agent_log::delete_for_mission(&m.id);
    council::clear_state(&m.id);
    println!("sessions deleted {sessions:?}");

    assert_eq!(after.status, "review", "the tick sends the ADR to review");
    assert!(after
        .pr_url
        .as_deref()
        .is_some_and(|u| u.contains("/pull/")));
    let positions: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "council" && e.summary.starts_with("position:"))
        .collect();
    let who: std::collections::BTreeSet<&str> =
        positions.iter().map(|e| e.from_agent.as_str()).collect();
    assert!(who.len() >= 2, "positions from two agents at least");
    assert!(events.iter().any(|e| e.summary.starts_with("synthesis:")));
    let adr = adr.expect("ADR file in the worktree");
    assert!(adr.contains("## Positions") && adr.contains("## Decision"));
    assert!(!adr.contains('\u{2014}') && !adr.contains('\u{2013}'));
    assert!(note.is_some(), "decision note in the vault");
    assert!(sessions.iter().all(|(_, ok)| *ok));
    if let Some(b) = branch {
        assert!(!worktree::remote_branch_exists(&dir, &b), "branch deleted");
    }
}
