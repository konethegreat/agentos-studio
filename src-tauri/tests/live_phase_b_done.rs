//! Opt-in, needs AgentOS Studio running (release build):
//! `cargo test --test live_phase_b_done -- --ignored --nocapture --test-threads 1`
//!
//! Phase B "Done when", hands off. This test only writes three queued missions into `studio.db`
//! and turns Automations on; everything after that is the running app's own scheduler, bus
//! watcher and workers:
//!
//! - a Hermes mission in `agentos-sandbox-1` (Hermes kanban, gateway dispatcher),
//! - a Claude Code mission in `agentos-sandbox-2` (Studio worktree, headless worker),
//! - a Qoder mission in `agentos-sandbox-1` (IDE only here): offered to Qoder, then reassigned to
//!   a headless agent after the IDE pickup wait (set to 1 minute for the run).
//!
//! Passes when the Hermes and Claude Code missions reach review with a PR on their own and the
//! Qoder mission is picked up or reassigned. Then it removes everything it caused: PRs closed,
//! branches and worktrees deleted, kanban tasks and bus tasks removed, missions deleted, and the
//! settings it changed put back.

use std::process::Command;
use std::time::{Duration, Instant};

use agentos_studio_lib::{
    automation, kanban, missions, procs, review_agent, settings, store, vault::Vault, worktree,
};

fn kv(key: &str) -> Option<String> {
    let c = store::open().unwrap();
    store::kv_get(&c, key).unwrap()
}

fn restore_kv(key: &str, v: &Option<String>) {
    let c = store::open().unwrap();
    match v {
        Some(v) => store::kv_set(&c, key, v).unwrap(),
        None => store::kv_delete(&c, key).unwrap(),
    }
}

fn run(dir: &std::path::Path, exe: &str, args: &[&str]) -> String {
    let out = Command::new(procs::find(exe).expect(exe))
        .args(args)
        .current_dir(dir)
        .env("PATH", procs::search_path())
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
#[ignore = "drives the running app through a real three-agent run"]
fn phase_b_done_when() {
    assert!(
        std::net::TcpStream::connect("127.0.0.1:47823").is_ok(),
        "start AgentOS Studio first: its scheduler does the work"
    );
    let v = Vault::new(settings::vault_path());
    let keys = [
        automation::KV_ENABLED,
        automation::KV_MAX,
        automation::KV_IDE_WAIT,
        kanban::KV_PARALLEL,
    ];
    let saved: Vec<Option<String>> = keys.iter().map(|k| kv(k)).collect();

    let a = missions::create(
        "agentos-sandbox-1",
        "Add a multiply(a, b) function to lib.js and a test for it in test.js",
        "lib.js exports multiply; test.js asserts multiply(3, 4) === 12; npm test passes; nothing else changes",
        "implementer",
        "hermes",
        "",
        "",
        0.0,
    )
    .unwrap();
    let b = missions::create(
        "agentos-sandbox-2",
        "Add a subtract(a, b) function to lib.js and a test for it in test.js",
        "lib.js exports subtract; test.js asserts subtract(9, 4) === 5; npm test passes; nothing else changes",
        "implementer",
        "claude-code",
        "",
        "",
        0.0,
    )
    .unwrap();
    let c = missions::create(
        "agentos-sandbox-1",
        "Add a negate(a) function to lib.js and a test for it in test.js",
        "lib.js exports negate; test.js asserts negate(5) === -5; npm test passes; nothing else changes",
        "implementer",
        "qoder",
        "",
        "",
        0.0,
    )
    .unwrap();
    println!(
        "missions: {} hermes, {} claude-code, {} qoder",
        a.id, b.id, c.id
    );
    // Two missions share sandbox-1, each in its own worktree; the wait is short for the run.
    kanban::set_parallel_per_repo(2).unwrap();
    automation::set_max_concurrent(4);
    automation::set_ide_wait_minutes(1);
    automation::set_enabled(true);
    let started = Instant::now();

    let deadline = started + Duration::from_secs(40 * 60);
    let mut last = String::new();
    let (mut ra, mut rb, mut rc);
    loop {
        ra = missions::get(&a.id).unwrap().unwrap();
        rb = missions::get(&b.id).unwrap().unwrap();
        rc = missions::get(&c.id).unwrap().unwrap();
        let line = format!(
            "{}={}({}) {}={}({}) {}={}({}) agent={}",
            ra.id,
            ra.status,
            ra.pr_url.is_some(),
            rb.id,
            rb.status,
            rb.pr_url.is_some(),
            rc.id,
            rc.status,
            rc.pr_url.is_some(),
            rc.agent
        );
        if line != last {
            println!("[{:>4}s] {line}", started.elapsed().as_secs());
            last = line;
        }
        let ab = ra.status == "review" && rb.status == "review";
        let c_moved = rc.agent != "qoder" && rc.status != "queued";
        let settled =
            |m: &missions::Mission| matches!(m.status.as_str(), "review" | "blocked" | "failed");
        if (ab && c_moved && settled(&rc)) || Instant::now() > deadline {
            break;
        }
        if [&ra, &rb]
            .iter()
            .any(|m| m.status == "blocked" || m.status == "failed")
        {
            break;
        }
        std::thread::sleep(Duration::from_secs(15));
    }
    let note_c = kv(&format!("mission.note.{}", c.id));
    println!("reassignment note: {note_c:?}");
    for m in [&ra, &rb, &rc] {
        println!(
            "{} {} agent={} status={} pr={:?} note={:?}",
            m.id, m.repo, m.agent, m.status, m.pr_url, m.review_note
        );
    }

    // Stop new work first, then remove what the run created.
    automation::set_enabled(false);
    for m in [&ra, &rb, &rc] {
        let m = missions::get(&m.id).unwrap().unwrap();
        let dir = review_agent::project_dir(&m.repo).unwrap();
        if let Some(url) = &m.pr_url {
            let head = run(
                &dir,
                "gh",
                &[
                    "pr",
                    "view",
                    url,
                    "--json",
                    "headRefName",
                    "-q",
                    ".headRefName",
                ],
            );
            println!(
                "close {url}: {}",
                run(&dir, "gh", &["pr", "close", url]).trim()
            );
            let head = head.trim();
            if !head.is_empty() {
                println!(
                    "delete branch {head}: {}",
                    run(&dir, "git", &["push", "origin", "--delete", head]).trim()
                );
            }
        }
        if let Some(task) = m.task_id.as_deref() {
            if let Some(id) = task.strip_prefix(kanban::PREFIX) {
                if let Ok(view) = kanban::show(task) {
                    if let Some(wt) = view.workspace_path.as_deref() {
                        let wtp = std::path::Path::new(wt);
                        if wtp.starts_with(dir.join(".worktrees")) {
                            let br = run(wtp, "git", &["branch", "--show-current"]);
                            run(&dir, "git", &["worktree", "remove", "--force", wt]);
                            run(&dir, "git", &["branch", "-D", br.trim()]);
                        }
                    }
                }
                run(
                    &dir,
                    "hermes",
                    &["kanban", "--board", kanban::BOARD, "archive", id],
                );
                println!(
                    "kanban {id}: {}",
                    run(
                        &dir,
                        "hermes",
                        &["kanban", "--board", kanban::BOARD, "archive", "--rm", id]
                    )
                    .trim()
                );
            } else {
                agentos_studio_lib::bus::stop_task_worker(task);
                if let Ok((t, _)) = v.get_task(task) {
                    let _ = std::fs::remove_file(&t.path);
                }
                for ext in ["log", "prompt.md", "agy-settings.json"] {
                    let _ = std::fs::remove_file(v.worker_logs().join(format!("{task}.{ext}")));
                }
            }
        }
        if worktree::recorded(&m.id).is_some() {
            let _ = worktree::discard(&dir, &m.id);
        }
        run(&dir, "git", &["worktree", "prune"]);
        if matches!(m.status.as_str(), "running" | "review") {
            let _ = missions::block(&m.id, "live test cleanup", true);
        }
        missions::delete(&m.id).unwrap();
        let c = store::open().unwrap();
        for k in [
            "mission.note.",
            "mission.pin.",
            "mission.retries.",
            "mission.beat.",
        ] {
            let _ = store::kv_delete(&c, &format!("{k}{}", m.id));
        }
    }
    for (k, s) in keys.iter().zip(&saved) {
        restore_kv(k, s);
    }

    assert_eq!(
        ra.status, "review",
        "Hermes mission reached review on its own"
    );
    assert!(ra.pr_url.is_some(), "Hermes mission has a PR");
    assert_eq!(
        rb.status, "review",
        "Claude Code mission reached review on its own"
    );
    assert!(rb.pr_url.is_some(), "Claude Code mission has a PR");
    assert_ne!(rc.agent, "qoder", "the IDE-only mission was reassigned");
    assert!(note_c.unwrap_or_default().contains("reassigned to"));
}
