//! Live Phase D tests, in one file so they share one test binary (#107).
//!
//! 1. `a_goal_is_planned_into_missions_without_touching_a_hermes_board`: Hermes' own decomposer
//!    plans a goal into missions and nothing reaches a Hermes board where the gateway's
//!    dispatcher would run it. One auxiliary model call; a temporary studio.db and vault.
//!    cargo test --test live_planner a_goal_is_planned -- --ignored --nocapture --test-threads 1
//!
//! 2. `phase_d_done_a_goal_goes_from_one_sentence_to_merged_verified_and_reported`: the Phase D
//!    "Done when". Needs the RUNNING release build of this version (its scheduler starts the
//!    workers, reviews, merges and verifies). The test only does what the Goals screen does when
//!    the owner submits one sentence (create the goal at Ask on risk, plan it in the background)
//!    and then watches: no review, merge or answer is made by the test. The sandbox's Allow merge
//!    switch is on for the run, since Ask on risk merges only on the allow list.
//!    cargo test --test live_planner phase_d_done_a_goal -- --ignored --nocapture --test-threads 1
//!
//! 3. `phase_d_done_an_escalation_reaches_the_phone_and_the_answer_is_honoured`: a second goal at
//!    Ask on risk with Allow merge off, so its merge becomes an escalation sent to the paired
//!    phone. The owner replies `E-<n> <code> merge` from the phone within 30 minutes and the test
//!    checks Studio merged because of that reply. Needs a messaging target (Providers: Hermes).
//!    cargo test --test live_planner phase_d_done_an_escalation -- --ignored --nocapture --test-threads 1
//!
//! Cleanup for 2 and 3: every merge is reverted on the sandbox's main, the routing file, the
//! project note and kv are restored byte for byte, the outcome note, missions, goal and the
//! Hermes sessions the run created are deleted.

use std::process::Command;
use std::time::{Duration, Instant};

use agentos_studio_lib::{
    agent_log, escalation, goal_loop, goals, missions, planner, procs, review_agent, settings,
    store, timeline, vault::Vault, worktree,
};
fn board_task_counts() -> String {
    let out = std::process::Command::new("hermes")
        .args(["kanban", "boards", "list"])
        .output()
        .expect("hermes kanban boards list");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
#[ignore]
fn a_goal_is_planned_into_missions_without_touching_a_hermes_board() {
    let db = tempfile::tempdir().unwrap();
    std::env::set_var("AGENTOS_STUDIO_DB", db.path().join("studio.db"));
    let vdir = tempfile::tempdir().unwrap();
    let v = Vault::new(vdir.path());
    v.scaffold().unwrap();

    let boards_before = board_task_counts();
    let g = goals::create(&goals::NewGoal {
        outcome: "Add a GET /probe endpoint that returns {\"ok\":true}, with a unit test, and document it in README.md".into(),
        repos: vec!["agentos-sandbox-1".into()],
        checks: vec![
            goals::Check {
                kind: "command".into(),
                target: "npm test".into(),
                repo: None,
            },
            goals::Check {
                kind: "file".into(),
                target: "README.md".into(),
                repo: None,
            },
        ],
        budget_usd: 0.0,
        deadline: None,
        autonomy: Some("ask".into()),
    })
    .unwrap();

    let started = std::time::Instant::now();
    let planned = planner::plan(&v, &g.id).expect("the planner plans the goal");
    println!(
        "{} planned in {:.1}s: status {}, note {:?}",
        g.id,
        started.elapsed().as_secs_f64(),
        planned.status,
        planned.note
    );
    let ms = goals::missions_of(&g.id).unwrap();
    for m in &ms {
        println!(
            "  {} repo={} agent={} after={:?}\n    title: {}\n    acceptance: {}",
            m.id,
            m.repo,
            m.agent,
            planner::depends_on(&m.id),
            m.goal.lines().next().unwrap_or(""),
            m.acceptance.lines().next().unwrap_or("")
        );
    }

    assert_eq!(planned.status, "running", "Ask on risk starts at once");
    assert!(
        !ms.is_empty() && ms.len() <= 8,
        "a small plan: {}",
        ms.len()
    );
    for m in &ms {
        assert_eq!(m.status, "queued");
        assert_eq!(m.repo, "agentos-sandbox-1");
        assert_eq!(m.agent, "auto", "assign.rs picks the agent at start");
        assert!(!m.acceptance.trim().is_empty());
        assert!(m.goal.contains(&format!("Part of goal {}", g.id)));
        for d in planner::depends_on(&m.id) {
            assert!(
                ms.iter().any(|x| x.id == d),
                "{} depends inside the plan",
                m.id
            );
        }
    }
    assert!(
        !planner::plan_db(&g.id).exists(),
        "the private kanban database is deleted"
    );
    assert_eq!(
        board_task_counts(),
        boards_before,
        "no Hermes board gained a task"
    );
    let note = std::fs::read_to_string(vdir.path().join(goals::note_rel(&g.id))).unwrap();
    assert!(note.contains("status: running"));

    goals::delete(&g.id).unwrap();
    assert!(goals::get(&g.id).unwrap().is_none());
    std::env::remove_var("AGENTOS_STUDIO_DB");
}

// ---------------------------------------------------------------------------------------------
// Phase D done check
// ---------------------------------------------------------------------------------------------

const REPO: &str = "agentos-sandbox-1";
const KV: [&str; 4] = [
    "automation.enabled",
    "missions.parallel_per_repo",
    "fleet.paused",
    "escalation.timeout_minutes",
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

fn preflight() {
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
}

/// What the run must put back exactly as it found it.
struct Saved {
    kv: Vec<(String, Option<String>)>,
    routing: Option<String>,
    project_note: Option<String>,
    main_before: String,
    untracked: Vec<String>,
}

fn routing_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(".agentos").join("routing.json")
}

fn save(v: &Vault, dir: &std::path::Path) -> Saved {
    let _ = git(dir, &["fetch", "-q", "origin"]);
    Saved {
        kv: KV.iter().map(|k| (k.to_string(), kv_get(k))).collect(),
        routing: std::fs::read_to_string(routing_path(dir)).ok(),
        project_note: std::fs::read_to_string(v.root.join(format!("01_Projects/{REPO}.md"))).ok(),
        main_before: git(dir, &["rev-parse", "origin/main"]).trim().to_string(),
        untracked: git(dir, &["status", "--porcelain", "--untracked-files=all"])
            .lines()
            .filter_map(|l| l.strip_prefix("?? ").map(str::to_string))
            .collect(),
    }
}

fn set_allow_merge(dir: &std::path::Path, on: bool) {
    let p = routing_path(dir);
    let mut j: serde_json::Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    j["allow_merge"] = serde_json::Value::Bool(on);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, serde_json::to_string_pretty(&j).unwrap() + "\n").unwrap();
}

fn delete_sessions(v: &Vault, id: &str) -> usize {
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
    sids.iter()
        .filter(|sid| {
            Command::new(procs::find("hermes").unwrap())
                .args(["sessions", "delete", sid, "--yes"])
                .env("PATH", procs::search_path())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .count()
}

/// Undo everything the goal did; prints what it did so a failure is visible.
fn restore(v: &Vault, dir: &std::path::Path, goal_id: &str, saved: &Saved) {
    for (k, val) in &saved.kv {
        kv_put(k, val.as_deref());
    }
    match &saved.routing {
        Some(s) => std::fs::write(routing_path(dir), s).unwrap(),
        None => {
            let _ = std::fs::remove_file(routing_path(dir));
        }
    }
    // Revert the goal's merges on the sandbox's main, newest first, in one push.
    let _ = git(dir, &["fetch", "-q", "origin"]);
    let after = git(dir, &["rev-parse", "origin/main"]).trim().to_string();
    if after != saved.main_before {
        let _ = git(dir, &["checkout", "-q", "main"]);
        let _ = git(dir, &["pull", "-q", "--ff-only", "origin", "main"]);
        let range = format!("{}..HEAD", saved.main_before);
        println!(
            "revert: {}",
            git(dir, &["revert", "--no-edit", &range]).trim()
        );
        println!(
            "push: {}",
            git(dir, &["push", "-q", "origin", "main"]).trim()
        );
    }
    // Untracked files the run left in the sandbox checkout (an npm lock file so far).
    for line in git(dir, &["status", "--porcelain", "--untracked-files=all"]).lines() {
        if let Some(f) = line.strip_prefix("?? ") {
            if !saved.untracked.iter().any(|u| u == f) {
                println!("removing untracked {f} from the sandbox checkout");
                let _ = std::fs::remove_file(dir.join(f));
            }
        }
    }
    let ms = goals::missions_of(goal_id).unwrap_or_default();
    let mut sessions = 0;
    for m in &ms {
        let _ = worktree::discard(dir, &m.id);
        sessions += delete_sessions(v, &m.id);
        if matches!(m.status.as_str(), "running" | "review" | "queued") {
            let _ = missions::block(&m.id, "live test cleanup", true);
        }
        let _ = agent_log::delete_for_mission(&m.id);
        let c = store::open().unwrap();
        for k in [
            "note", "pin", "worktree", "urgent", "defer", "after", "merged", "rejects",
        ] {
            let _ = store::kv_delete(&c, &format!("mission.{k}.{}", m.id));
        }
        for k in ["learning.done", "review.auto", "ci.heal"] {
            let _ = store::kv_delete(&c, &format!("{k}.{}", m.id));
        }
    }
    println!("sessions deleted: {sessions}");
    // Outcome note and the project note's outcome line.
    if let Ok(rd) = std::fs::read_dir(v.memory()) {
        for e in rd.flatten() {
            if is_outcome_note(&e.file_name().to_string_lossy(), goal_id) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let note = v.root.join(format!("01_Projects/{REPO}.md"));
    match &saved.project_note {
        Some(s) => std::fs::write(&note, s).unwrap(),
        None => {
            let _ = std::fs::remove_file(&note);
        }
    }
    let _ = std::fs::remove_file(v.root.join(goals::note_rel(goal_id)));
    // The goal's Obsidian views: mission and decision notes and the canvas.
    let _ = std::fs::remove_dir_all(v.root.join(agentos_studio_lib::goal_views::folder(goal_id)));
    match goals::delete(goal_id) {
        Ok(()) => println!("goal {goal_id} deleted"),
        Err(e) => println!("goal {goal_id} NOT deleted: {e:#}"),
    }
}

/// Creates the goal the way the Goals screen does and watches until it finishes.
fn run_goal(
    v: &Vault,
    outcome: &str,
    checks: Vec<goals::Check>,
    limit: Duration,
    mut each: impl FnMut(&goals::Goal),
) -> (goals::Goal, Duration) {
    let g = goals::create(&goals::NewGoal {
        outcome: outcome.into(),
        repos: vec![REPO.into()],
        checks,
        budget_usd: 0.0,
        deadline: None,
        autonomy: Some("ask".into()),
    })
    .unwrap();
    println!("goal {} created at Ask on risk", g.id);
    planner::plan_in_background(v.root.clone(), g.id.clone());
    let started = Instant::now();
    let mut last = String::new();
    let mut now = g.clone();
    while Instant::now() < started + limit {
        std::thread::sleep(Duration::from_secs(15));
        now = goals::get(&g.id).unwrap().unwrap();
        let ms = goals::missions_of(&g.id).unwrap_or_default();
        let line = format!(
            "{} [{}] {}",
            now.status,
            ms.iter()
                .map(|m| format!("{}:{}:{}", m.id, m.status, m.agent))
                .collect::<Vec<_>>()
                .join(" "),
            now.note.chars().take(120).collect::<String>()
        );
        if line != last {
            println!("[{:>5}s] {line}", started.elapsed().as_secs());
            last = line;
        }
        // An approval runs the learning hook, which writes lesson notes into the real vault; mark
        // each mission learned as soon as it exists so a test goal leaves no lessons behind.
        let c = store::open().unwrap();
        for m in &ms {
            let _ = store::kv_set(&c, &format!("learning.done.{}", m.id), "1");
        }
        each(&now);
        if matches!(now.status.as_str(), "done" | "failed" | "paused") {
            break;
        }
    }
    (now, started.elapsed())
}

/// The outcome note's file name is a slug of "Goal G-001 outcome" (`Goal_G_001_outcome.md`).
fn is_outcome_note(name: &str, goal_id: &str) -> bool {
    let norm = |s: &str| s.to_lowercase().replace(['-', ' '], "_");
    norm(name).contains(&norm(goal_id))
}

fn probe_checks() -> Vec<goals::Check> {
    vec![
        goals::Check {
            kind: "command".into(),
            target: "npm test".into(),
            repo: None,
        },
        goals::Check {
            kind: "command".into(),
            target: "node -e \"const l=require('./lib.js'); if (typeof l.probe !== 'function' || l.probe().ok !== true) process.exit(1)\"".into(),
            repo: None,
        },
        goals::Check {
            kind: "file".into(),
            target: "README.md".into(),
            repo: None,
        },
    ]
}

#[test]
#[ignore = "needs the running release build; real agents and real merges in the sandbox"]
fn phase_d_done_a_goal_goes_from_one_sentence_to_merged_verified_and_reported() {
    preflight();
    let v = Vault::new(settings::vault_path());
    let dir = review_agent::project_dir(REPO).expect("sandbox project");
    let saved = save(&v, &dir);
    kv_put("fleet.paused", None);
    kv_put("automation.enabled", Some("0"));
    kv_put("missions.parallel_per_repo", Some("1"));
    set_allow_merge(&dir, true);

    let (g, took) = run_goal(
        &v,
        "Add a probe() function to lib.js that returns { ok: true }, with a test in test.js, and document it in README.md",
        probe_checks(),
        Duration::from_secs(75 * 60),
        |_| {},
    );
    let ms = goals::missions_of(&g.id).unwrap_or_default();
    let escs: Vec<_> = escalation::list(500)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.goal_id.as_deref() == Some(g.id.as_str()))
        .collect();
    println!("\n{} {} after {}s", g.id, g.status, took.as_secs());
    println!("report:\n{}", g.report.clone().unwrap_or_default());
    for m in &ms {
        println!(
            "  {} {} by {} reviewed by {:?} ({:?}) merge {:?} pr {:?}",
            m.id,
            m.status,
            m.agent,
            m.reviewer,
            m.review_verdict,
            goal_loop::merge_outcome(&m.id),
            m.pr_url
        );
    }
    for e in &escs {
        println!(
            "  escalation {} {} {} {:?} by {:?}",
            e.id, e.kind, e.status, e.answer, e.answered_by
        );
    }
    let outcome_note = std::fs::read_dir(v.memory())
        .map(|rd| {
            rd.flatten()
                .any(|e| is_outcome_note(&e.file_name().to_string_lossy(), &g.id))
        })
        .unwrap_or(false);
    let project_line = std::fs::read_to_string(v.root.join(format!("01_Projects/{REPO}.md")))
        .unwrap_or_default()
        .contains(&format!("goal {} done", g.id));
    // Everything asserted is captured now: the cleanup below deletes the merge records.
    let merges: Vec<Option<String>> = ms.iter().map(|m| goal_loop::merge_outcome(&m.id)).collect();
    let keep = std::env::var("PHASE_D_KEEP").is_ok_and(|v| v == "1");
    if keep {
        println!(
            "kept {} for the screenshots; run cleanup_kept_goal with PHASE_D_GOAL",
            g.id
        );
        for (k, val) in &saved.kv {
            kv_put(k, val.as_deref());
        }
    } else {
        restore(&v, &dir, &g.id, &saved);
    }

    assert_eq!(g.status, "done", "the goal finished: {}", g.note);
    assert!(!ms.is_empty());
    for (m, merge) in ms.iter().zip(&merges) {
        assert_eq!(m.status, "done", "{} is done", m.id);
        assert!(
            merge.as_deref().is_some_and(|o| o.starts_with("merged")),
            "{} merged by Studio",
            m.id
        );
        assert!(m.reviewer.is_some(), "{} was reviewed", m.id);
        assert_ne!(
            m.reviewer.as_deref(),
            Some(m.agent.as_str()),
            "{} reviewed by another agent",
            m.id
        );
    }
    assert!(
        escs.iter()
            .all(|e| e.answered_by.as_deref() != Some("studio")),
        "zero clicks: no escalation was answered in Studio by the owner"
    );
    let report = g.report.unwrap_or_default();
    assert!(report.contains("Definition of done:"));
    assert_eq!(
        report.matches("- passed ").count(),
        3,
        "all three checks passed"
    );
    assert!(outcome_note, "an outcome note was remembered");
    assert!(project_line, "the project note has the outcome line");
}

#[test]
#[ignore = "removes a goal kept with PHASE_D_KEEP=1"]
fn cleanup_kept_goal() {
    let id = std::env::var("PHASE_D_GOAL").expect("PHASE_D_GOAL");
    let v = Vault::new(settings::vault_path());
    let dir = review_agent::project_dir(REPO).expect("sandbox project");
    let saved = Saved {
        kv: Vec::new(),
        routing: std::env::var("PHASE_D_ROUTING")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok()),
        project_note: std::env::var("PHASE_D_PROJECT_NOTE")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok()),
        main_before: std::env::var("PHASE_D_MAIN_BEFORE").expect("PHASE_D_MAIN_BEFORE"),
        untracked: Vec::new(),
    };
    restore(&v, &dir, &id, &saved);
    assert!(goals::get(&id).unwrap().is_none());
}

#[test]
#[ignore = "needs the running release build, a paired phone and the owner's reply"]
fn phase_d_done_an_escalation_reaches_the_phone_and_the_answer_is_honoured() {
    preflight();
    let target = agentos_studio_lib::hermes_surfaces::notify_target()
        .expect("set a messaging target in Providers: Hermes and pair the phone first");
    let v = Vault::new(settings::vault_path());
    let dir = review_agent::project_dir(REPO).expect("sandbox project");
    let saved = save(&v, &dir);
    kv_put("fleet.paused", None);
    kv_put("automation.enabled", Some("0"));
    kv_put("missions.parallel_per_repo", Some("1"));
    // Thirty minutes for the owner to answer from the phone; the safe answer is skip.
    kv_put("escalation.timeout_minutes", Some("30"));
    set_allow_merge(&dir, false);
    println!("escalations go to {target}; answer the merge question from the phone with `merge`");

    let mut asked: Option<String> = None;
    let (g, took) = run_goal(
        &v,
        "Add a ping() function to lib.js that returns the string pong, with a test in test.js, and document it in README.md",
        vec![goals::Check {
            kind: "command".into(),
            target: "npm test".into(),
            repo: None,
        }],
        Duration::from_secs(110 * 60),
        |g| {
            for e in escalation::list(50).unwrap_or_default() {
                if e.goal_id.as_deref() == Some(g.id.as_str()) && e.kind == "merge" && asked.is_none() {
                    println!(">>> {} sent to the phone: {}", e.id, e.question);
                    asked = Some(e.id.clone());
                }
            }
        },
    );
    let e = asked
        .as_deref()
        .and_then(|id| escalation::get(id).ok().flatten());
    println!(
        "{} {} after {}s; escalation {:?}",
        g.id,
        g.status,
        took.as_secs(),
        e
    );
    let ms = goals::missions_of(&g.id).unwrap_or_default();
    let merged: Vec<_> = ms
        .iter()
        .map(|m| (m.id.clone(), goal_loop::merge_outcome(&m.id)))
        .collect();
    println!("merges: {merged:?}");
    restore(&v, &dir, &g.id, &saved);

    let e = e.expect("a merge escalation was raised");
    assert_eq!(
        e.answered_by.as_deref(),
        Some("phone"),
        "answered from the phone"
    );
    assert_eq!(e.answer.as_deref(), Some("merge"));
    assert!(
        merged
            .iter()
            .any(|(_, o)| o.as_deref().is_some_and(|o| o.starts_with("merged"))),
        "the phone's answer was honoured: Studio merged"
    );
}
