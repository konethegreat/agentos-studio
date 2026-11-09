//! Opt-in, Phase C "Done when": `cargo test --test live_phase_c_done -- --ignored --nocapture --test-threads 1`
//!
//! A live goal is finished by two different agents, with one `ask_agent` and one `handoff`, and the
//! run timeline shows the whole path, with nobody copying text between tools. Needs the RUNNING
//! release build of this version (its scheduler and bus watcher start the workers, and the agents'
//! agentos-hub entries point at this version's hub, which has `ask_agent` and `handoff`).
//!
//! The test only queues one mission for Claude Code in `agentos-sandbox-1` and turns Automations
//! on. The goal tells the team how to work: Claude Code asks Hermes for a CLI fact, implements,
//! then hands the rest to Hermes, which documents it and opens the pull request. When the app moves
//! the mission to review, the test plays the owner: the reviewer runs (a different agent from the
//! final implementer), and the gatekeeper merges with the repository's Allow merge switch on. Then
//! the timeline is read and checked.
//!
//! Cleanup: the merge is reverted on the sandbox's main, the Hermes sessions the run created are
//! deleted, kv is restored. With `PHASE_C_KEEP=1` the mission, its exchange log and bus files stay
//! so the timeline can be shown in the app; `cleanup_kept_mission` (with `PHASE_C_MISSION`) removes
//! them afterwards.

use std::process::Command;
use std::time::{Duration, Instant};

use agentos_studio_lib::{
    agent_log, missions, procs, review_agent, router, settings, store, timeline, vault::Vault,
    worktree,
};

const REPO: &str = "agentos-sandbox-1";
const KV: [&str; 3] = [
    "automation.enabled",
    "automation.max_concurrent",
    "missions.parallel_per_repo",
];

fn kv_get(k: &str) -> Option<String> {
    store::kv_get(&store::open().unwrap(), k).unwrap()
}

fn kv_put(k: &str, v: Option<&str>) {
    let c = store::open().unwrap();
    match v {
        Some(v) => store::kv_set(&c, k, v).unwrap(),
        None => store::kv_delete(&c, k).unwrap(),
    }
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let o = Command::new(procs::find("git").unwrap())
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn delete_sessions(v: &Vault, id: &str) -> Vec<(String, bool)> {
    let mut sids: Vec<String> = agent_log::for_mission(id)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            e.detail
                .rsplit_once("Hermes session: ")
                .map(|(_, s)| s.trim().to_string())
        })
        .collect();
    if let Ok(tl) = timeline::mission(v, id) {
        for s in &tl.steps {
            if let Some(sid) = s.title.strip_prefix("Hermes session ") {
                sids.push(sid.trim().to_string());
            }
        }
    }
    sids.sort();
    sids.dedup();
    sids.into_iter()
        .map(|sid| {
            let ok = Command::new(procs::find("hermes").unwrap())
                .args(["sessions", "delete", &sid, "--yes"])
                .env("PATH", procs::search_path())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            (sid, ok)
        })
        .collect()
}

fn remove_mission(v: &Vault, id: &str) {
    if let Ok(tl) = timeline::mission(v, id) {
        let _ = tl;
    }
    for s in agentos_studio_lib::vault::State::ALL {
        if let Ok(rd) = std::fs::read_dir(v.state_dir(s)) {
            for e in rd.flatten() {
                let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                if text.contains(&format!("Mission: {id}.")) {
                    let name = e.file_name().to_string_lossy().to_string();
                    let task = name.split('_').next().unwrap_or("").to_string();
                    let _ = std::fs::remove_file(e.path());
                    for ext in ["log", "prompt.md", "agy-settings.json"] {
                        let _ = std::fs::remove_file(v.worker_logs().join(format!("{task}.{ext}")));
                    }
                }
            }
        }
    }
    if let Some(m) = missions::get(id).unwrap() {
        if matches!(m.status.as_str(), "running" | "review" | "queued") {
            let _ = missions::block(id, "live test cleanup", true);
        }
    }
    let _ = missions::delete(id);
    let _ = agent_log::delete_for_mission(id);
    let c = store::open().unwrap();
    for k in ["note", "pin", "worktree", "urgent", "defer"] {
        let _ = store::kv_delete(&c, &format!("mission.{k}.{id}"));
    }
}

#[test]
#[ignore = "needs the running release build; real agents, a real merged pull request"]
fn a_goal_is_finished_by_two_agents_with_a_handoff_and_an_ask() {
    assert!(
        std::net::TcpStream::connect_timeout(
            &"127.0.0.1:47823".parse().unwrap(),
            Duration::from_secs(2)
        )
        .is_ok(),
        "AgentOS Studio must be running"
    );
    let cfg = std::fs::read_to_string(procs::hermes_config()).unwrap();
    assert!(
        cfg.contains(&format!("AgentOS Hub\\{}\\", env!("CARGO_PKG_VERSION"))),
        "agents must run the {} hub",
        env!("CARGO_PKG_VERSION")
    );
    let v = Vault::new(settings::vault_path());
    let dir = review_agent::project_dir(REPO).expect("sandbox project");
    let saved: Vec<(String, Option<String>)> =
        KV.iter().map(|k| (k.to_string(), kv_get(k))).collect();

    let m = router::create_mission(
        REPO,
        "Add a subtract(a, b) function to lib.js with a test, and document it in README.md. Work as a team through the agentos-hub tools, never through the owner: \
(1) Before writing code, call ask_agent with to=hermes, your mission id, and ask Hermes to run `hermes project list` and tell you the display name of the agentos-sandbox-1 project. \
(2) Implement subtract in lib.js and a test in test.js asserting subtract(5, 3) === 2, run npm test, and commit on your branch. \
(3) Then call handoff with to=hermes, your mission id, a summary of what you did, files lib.js and test.js, and next_step: add a README.md section titled Subtract that names the project display name Hermes gave you, run npm test, commit, push with git push -u origin HEAD, open the pull request with gh pr create, and put its URL in complete_task. Do not open the pull request yourself and do not call complete_task after the handoff.",
        "lib.js exports subtract; test.js asserts subtract(5, 3) === 2; npm test passes; README.md has a Subtract section naming the project display name AgentOS Sandbox 1; exactly one pull request",
        "implementer",
        "claude-code",
        "claude-subscription",
        "",
        0.0,
    )
    .unwrap();
    println!("mission {}", m.id);
    kv_put("missions.parallel_per_repo", Some("1"));
    kv_put("automation.max_concurrent", Some("3"));
    kv_put("automation.enabled", Some("1"));

    // From here the app does the work: scheduler, workers, watcher, tick.
    let started = Instant::now();
    let deadline = started + Duration::from_secs(40 * 60);
    let mut last = String::new();
    let mut now = missions::get(&m.id).unwrap().unwrap();
    while !matches!(
        now.status.as_str(),
        "review" | "blocked" | "failed" | "done"
    ) && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_secs(15));
        now = missions::get(&m.id).unwrap().unwrap();
        let line = format!("{} {} {:?}", now.status, now.agent, now.task_id);
        if line != last {
            println!("[{:>4}s] {line}", started.elapsed().as_secs());
            last = line;
        }
    }
    // Automations off again before the owner's part, so nothing else starts.
    for (k, val) in &saved {
        kv_put(k, val.as_deref());
    }

    // The owner's part: run the reviewer, then the gate with the Allow merge switch on.
    let mut merged = None;
    let mut reviewed = None;
    if now.status == "review" {
        reviewed = Some(review_agent::review(&v, &m.id, "claude-code"));
        println!(
            "review: {:?}",
            reviewed
                .as_ref()
                .map(|r| r.as_ref().map(|m| (&m.status, &m.review_note)))
        );
        if missions::get(&m.id).unwrap().unwrap().status == "done" {
            // CI (GitGuardian) must have reported on the pull request.
            for _ in 0..20 {
                let r = review_agent::merge(&m.id, true);
                println!("merge: {r:?}");
                let retry = matches!(&r, Err(e) if e.to_string().contains("CI is not green"));
                merged = Some(r);
                if !retry {
                    break;
                }
                std::thread::sleep(Duration::from_secs(15));
            }
        }
    }
    let after = missions::get(&m.id).unwrap().unwrap();
    let tl = timeline::mission(&v, &m.id).unwrap();
    println!(
        "timeline: {} steps, agents {:?}, {} handoffs, {} asks, {}s, metered ${:.2}",
        tl.steps.len(),
        tl.agents,
        tl.handoffs,
        tl.asks,
        tl.duration_s,
        tl.total_cost_usd
    );
    for s in tl.steps.iter().filter(|s| s.kind != "tool") {
        println!(
            "  {} {:8} {:12} {}",
            s.at,
            s.kind,
            s.agent,
            s.title.chars().take(110).collect::<String>()
        );
    }
    let report = std::env::var("PHASE_C_REPORT").ok();
    if let Some(p) = &report {
        let _ = std::fs::write(p, serde_json::to_string_pretty(&tl).unwrap());
    }

    // Undo the merge on the sandbox so the fixture stays as it was.
    if merged.as_ref().is_some_and(|r| r.is_ok()) {
        let _ = git(&dir, &["checkout", "main"]);
        let _ = git(&dir, &["pull", "--ff-only", "origin", "main"]);
        println!("{}", git(&dir, &["revert", "--no-edit", "HEAD"]));
        println!("{}", git(&dir, &["push", "origin", "main"]));
    }
    let _ = worktree::discard(&dir, &m.id);
    let keep = std::env::var("PHASE_C_KEEP").is_ok_and(|v| v == "1");
    if !keep {
        println!("sessions deleted: {:?}", delete_sessions(&v, &m.id));
        remove_mission(&v, &m.id);
    } else {
        println!("kept {} for the timeline screenshot", m.id);
    }

    assert!(
        matches!(reviewed, Some(Ok(_))),
        "the mission reached review and was judged"
    );
    assert_eq!(after.status, "done", "approved by the reviewer");
    assert!(
        merged.as_ref().is_some_and(|r| r.is_ok()),
        "merged by the gate: {merged:?}"
    );
    assert!(tl.agents.iter().any(|a| a == "claude-code"));
    assert!(tl.agents.iter().any(|a| a == "hermes"));
    assert!(tl.handoffs >= 1, "a handoff happened");
    assert!(tl.asks >= 1, "an ask_agent happened");
    assert!(tl.steps.iter().any(|s| s.kind == "answer"));
    assert!(tl.steps.iter().any(|s| s.kind == "pr"));
    assert!(tl.steps.iter().any(|s| s.kind == "review"));
}

#[test]
#[ignore = "removes a mission kept with PHASE_C_KEEP=1"]
fn cleanup_kept_mission() {
    let id = std::env::var("PHASE_C_MISSION").expect("PHASE_C_MISSION");
    let v = Vault::new(settings::vault_path());
    println!("sessions deleted: {:?}", delete_sessions(&v, &id));
    remove_mission(&v, &id);
    assert!(missions::get(&id).unwrap().is_none());
}
