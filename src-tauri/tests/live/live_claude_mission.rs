//! Opt-in: `cargo test --test live -- live_claude_mission:: --ignored --nocapture --test-threads 1`
//!
//! Phase B items 1 to 3 against the real machine: a Claude Code mission in the
//! `agentos-sandbox-1` fixture repository (a Hermes project) gets its own worktree and branch, a
//! headless `claude -p` worker with only the repository's tool allowlist (never skipping
//! permissions) commits, pushes, opens a pull request and calls `complete_task` on the real hub,
//! and the scheduler tick moves the mission to review with the PR URL.
//!
//! The worker is started here with the same arguments `bus::spawn_worker` builds, because the
//! Tauri host that the scheduler uses does not exist in a test binary. Everything created is
//! removed: the pull request is closed with its branch deleted, the worktree, bus task, worker log
//! and mission are deleted.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agentos_studio_lib::{
    automation, bus, missions, procs, review_agent, routing, settings, vault::Vault, worktree,
};

const REPO: &str = "agentos-sandbox-1";

#[test]
#[ignore = "runs a real Claude Code worker and opens a real pull request"]
fn claude_code_mission_reaches_review_with_a_pr() {
    let v = Vault::new(settings::vault_path());
    let dir = review_agent::project_dir(REPO).expect("agentos-sandbox-1 is a Hermes project");
    let m = missions::create(
        REPO,
        "Add a multiply(a, b) function to lib.js and a test for it in test.js",
        "lib.js exports multiply; test.js asserts multiply(3, 4) === 12; npm test passes; nothing else changes",
        "implementer",
        "claude-code",
        "claude-subscription",
        "",
        0.0,
    )
    .unwrap();
    println!("mission {}", m.id);

    // The single dispatch path: worktree, bus task with Repository and Worktree lines. No app
    // host exists here, so it logs that no worker started instead of starting one.
    let m = missions::start(&v, &m.id).expect("start");
    let task = m.task_id.clone().expect("task id");
    let (t, _) = v.get_task(&task).unwrap();
    let wt = bus::workdir_from_body(&t.body).expect("worktree line");
    assert_ne!(wt, dir, "the worker runs in its own worktree");
    assert!(wt.starts_with(dir.join(".worktrees")));
    assert_eq!(bus::repo_from_body(&t.body).unwrap(), dir);
    println!("task {task} worktree {}", wt.display());

    // Run the worker exactly as spawn_worker would.
    v.claim_task(&task, "claude-code").unwrap();
    let allowed = routing::claude_allowed_tools(&routing::allowed_tools(&dir));
    println!("allowed tools: {allowed:?}");
    assert!(allowed.contains(&"PowerShell(npm test*)".to_string()));
    let (t, _) = v.get_task(&task).unwrap();
    let prompt = bus::worker_prompt(&task, "claude-code", &t.body);
    let exe = procs::find("claude").expect("claude installed");
    let mut child = Command::new(exe)
        .args(bus::claude_args(&allowed, None))
        .current_dir(&wt)
        // The same cleared, allowlisted environment an unattended worker gets.
        .env_clear()
        .envs(bus::worker_env(
            std::env::vars_os(),
            &t.project,
            &v.root,
            procs::search_path(),
            true,
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn claude");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(prompt.as_bytes())
        .unwrap();
    let started = Instant::now();
    let out = child.wait_with_output().expect("worker output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let denials: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("\"permission_denials\""))
        .collect();
    println!(
        "worker exited {:?} after {:?}; result lines: {}",
        out.status.code(),
        started.elapsed(),
        denials.len()
    );
    for l in stdout.lines().rev().take(1) {
        println!("last event: {}", l.chars().take(1500).collect::<String>());
    }

    // The tick moves it to review and captures the PR from the worker's summary.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut after = missions::get(&m.id).unwrap().unwrap();
    while after.status == "running" && Instant::now() < deadline {
        let r = automation::tick(&v);
        println!("tick: {:?}", r.moved_to_review);
        after = missions::get(&m.id).unwrap().unwrap();
        if after.status == "running" {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    let pr = after.pr_url.clone();
    println!("status={} pr={pr:?}", after.status);

    // Clean up whatever happened, then assert. Close the PR first, then drop worktree and branches.
    if let Some(url) = &pr {
        let _ = Command::new("gh")
            .args(["pr", "close", url])
            .current_dir(&dir)
            .env("PATH", procs::search_path())
            .output();
    }
    let branch = worktree::recorded_branch(&m.id).expect("recorded branch");
    println!("branch {branch}");
    if let Err(e) = worktree::discard(&dir, &m.id) {
        println!("discard: {e:#}");
    }
    let branch_left = worktree::remote_branch_exists(&dir, &branch);
    if let Ok((t, _)) = v.get_task(&task) {
        let _ = std::fs::remove_file(&t.path);
    }
    let _ = std::fs::remove_file(v.worker_logs().join(format!("{task}.log")));
    if after.status == "review" {
        let _ = missions::block(&m.id, "live test cleanup", true);
    }
    let _ = missions::delete(&m.id);

    assert_eq!(after.status, "review", "reached review, not done");
    assert!(
        pr.as_deref()
            .is_some_and(|u| u.contains("agentos-sandbox-1/pull/")),
        "PR URL captured: {pr:?}"
    );
    assert!(!wt.exists(), "worktree removed");
    assert!(!branch_left, "remote branch deleted");
    assert!(missions::get(&m.id).unwrap().is_none(), "mission removed");
}
