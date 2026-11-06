//! Opt-in live checks for v0.3 against the real machine:
//! `cargo test --test live_v3 -- --ignored --nocapture --test-threads 1`

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agentos_studio_lib::{github, pty, updates};

type Exits = Arc<Mutex<Vec<serde_json::Value>>>;

fn wait_for(buf: &Arc<Mutex<String>>, needle: &str, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if buf.lock().unwrap().contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn sink_into(out: Arc<Mutex<String>>, exits: Exits) -> pty::Sink {
    Arc::new(move |ev, payload| {
        if ev == pty::EV_PTY_DATA {
            out.lock()
                .unwrap()
                .push_str(payload["data"].as_str().unwrap_or(""));
        } else {
            exits.lock().unwrap().push(payload);
        }
    })
}

/// ConPTY asks for the cursor position (ESC[6n) at start and waits for the answer. xterm.js
/// answers in the app; here we answer like a terminal would.
fn answer_cursor_queries(state: &pty::PtyState, id: &str, out: &Arc<Mutex<String>>, secs: u64) {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut answered = 0;
    while Instant::now() < end {
        let asked = out.lock().unwrap().matches("\x1b[6n").count();
        while answered < asked {
            pty::write(state, id, "\x1b[1;1R").unwrap();
            answered += 1;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("answered {answered} cursor position queries");
}

fn req(profile: &str) -> pty::SpawnRequest {
    pty::SpawnRequest {
        profile: profile.into(),
        session_id: None,
        cwd: None,
        cols: 120,
        rows: 36,
    }
}

#[test]
#[ignore]
fn shell_pty_round_trip() {
    let state = pty::PtyState::default();
    let out = Arc::new(Mutex::new(String::new()));
    let exits: Exits = Arc::new(Mutex::new(Vec::new()));
    let info = pty::spawn_with(sink_into(out.clone(), exits.clone()), &state, req("shell"))
        .expect("spawn shell");
    println!("spawned {} pid {:?} in {}", info.title, info.pid, info.cwd);
    answer_cursor_queries(&state, &info.id, &out, 4);
    pty::resize(&state, &info.id, 132, 40).expect("resize");
    let cmd = if cfg!(windows) {
        "Write-Output (\"studio-\" + \"pty-ok-\" + (6*7))\r"
    } else {
        "echo studio-pty-ok-$((6*7))\r"
    };
    pty::write(&state, &info.id, cmd).unwrap();
    assert!(
        wait_for(&out, "studio-pty-ok-42", 30),
        "no echo; got:\n{}",
        out.lock().unwrap()
    );
    let snap = pty::snapshot(&state, &info.id).unwrap();
    assert!(snap.data.contains("studio-pty-ok-42") && snap.seq > 0);
    pty::write(&state, &info.id, "exit\r").unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    while exits.lock().unwrap().is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
    let ex = exits.lock().unwrap().clone();
    println!("exit events: {ex:?}");
    assert_eq!(ex.len(), 1);
    assert!(pty::list(&state).is_empty());
}

#[test]
#[ignore]
fn hermes_interactive_starts_in_a_pty() {
    let state = pty::PtyState::default();
    let out = Arc::new(Mutex::new(String::new()));
    let exits: Exits = Arc::new(Mutex::new(Vec::new()));
    let info = pty::spawn_with(sink_into(out.clone(), exits), &state, req("hermes"))
        .expect("spawn hermes");
    answer_cursor_queries(&state, &info.id, &out, 4);
    // The classic REPL draws a banner and a prompt; substantial output proves the TTY works.
    let end = Instant::now() + Duration::from_secs(60);
    while out.lock().unwrap().len() < 200 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_secs(2));
    let text = out.lock().unwrap().clone();
    let tail: String = text
        .chars()
        .rev()
        .take(800)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    println!("hermes printed {} chars; tail:\n{tail}", text.len());
    pty::kill(&state, &info.id).unwrap();
    assert!(text.len() >= 200, "hermes printed too little in a PTY");
}

#[test]
#[ignore]
fn update_checks_return_real_versions() {
    let r = updates::check_all();
    for p in &r.products {
        println!(
            "{:<15} state={:<12} current={:?} latest={:?} detail={}",
            p.label, p.state, p.current, p.latest, p.detail
        );
        if let Some(c) = &p.changelog {
            println!("   versions {:?}", c.versions);
            for h in c.highlights.iter().take(4) {
                println!("   * {h}");
            }
            for g in &c.groups {
                println!("   [{}] {} items", g.title, g.items.len());
            }
        }
    }
    assert_eq!(r.products.len(), 3);
    let studio = r.products.iter().find(|p| p.product == "studio").unwrap();
    assert_ne!(
        studio.state, "unknown",
        "GitHub releases unreachable: {}",
        studio.detail
    );
    let claude = r
        .products
        .iter()
        .find(|p| p.product == "claude-code")
        .unwrap();
    if claude.state == "available" {
        assert!(claude
            .changelog
            .as_ref()
            .is_some_and(|c| !c.versions.is_empty()));
    }
}

#[test]
#[ignore]
fn github_reads_work_through_gh() {
    let auth = github::auth_status().unwrap();
    println!("auth: {auth}");
    assert_eq!(auth["logged_in"], true);
    let repo = "konethegreat/agentos-studio";
    let prs = github::prs(repo, "all").unwrap();
    let list = prs.as_array().cloned().unwrap_or_default();
    println!("prs: {}", list.len());
    assert!(!list.is_empty());
    let first = list.last().unwrap()["number"].as_u64().unwrap();
    let detail = github::pr_detail(repo, first).unwrap();
    println!(
        "PR #{first}: {} files",
        detail["files"].as_array().map(|a| a.len()).unwrap_or(0)
    );
    let diff = github::pr_diff(repo, first).unwrap();
    assert!(diff.contains("diff --git"));
    let branches = github::branches(repo).unwrap();
    assert!(branches
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["name"] == "main"));
    let commits = github::commits(repo, Some("main")).unwrap();
    assert!(!commits.as_array().unwrap().is_empty());
    let runs = github::runs(repo, None).unwrap();
    let runs = runs.as_array().cloned().unwrap_or_default();
    println!("runs: {}", runs.len());
    if let Some(run) = runs.first() {
        let id = run["databaseId"].as_u64().unwrap();
        let d = github::run_detail(repo, id).unwrap();
        println!(
            "run {id}: {} jobs, {}",
            d["jobs"].as_array().map(|a| a.len()).unwrap_or(0),
            d["conclusion"]
        );
    }
    let issues = github::issues(repo, "all").unwrap();
    assert!(issues.is_array());
    let local = github::repo_for_dir(env!("CARGO_MANIFEST_DIR")).unwrap();
    assert_eq!(local["nameWithOwner"], repo);
}
