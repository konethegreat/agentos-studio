//! Opt-in, needs the running release build of this version:
//! `cargo test --test live_phase_e_done -- --ignored --nocapture --test-threads 1`
//!
//! Phase E "Done when", items 2, 3 and 6, against the running release build and the two sandbox
//! repositories. Hold `04_Agent_Engine/locks/live-machine.lock` first and delete it after.
//!
//! 2. Two rejected missions in one repository with the same blocker make one convention note, and
//!    the next mission started there carries that convention in its instructions.
//! 3. A finished sandbox mission produces lesson notes through `remember` with no human step; the
//!    review path triggers the extraction, the test only records a verdict.
//! 6. A mission merged by Studio carries the Mission, Agent, Model, Reviewer and Cost trailers and
//!    `provenance::why` resolves a line the change touched back to that mission.
//!
//! Item 2 needs the E2 hook (`conventions_for` in the mission instructions). Item 3 needs the E2
//! hook (the review path calls `learning::on_mission_reviewed`) and one real Hermes lesson turn on a
//! throwaway profile, so it runs only when the owner has allowed Hermes use. Item 6 needs the E5
//! hook (the merge appends `provenance::trailers`). Each test names the missing hook when it fails.
//!
//! Cleanup: missions, bus tasks, worktrees, PRs, branches, vault notes, kv rows, Hermes sessions and
//! the throwaway profile are removed, the settings the test changed are restored, and item 3 checks
//! that the owner's default `MEMORY.md` and `USER.md` are byte identical before and after.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agentos_studio_lib::{
    github, learning, missions, procs, provenance, review_agent, settings, store,
    vault::{State, Vault},
    worktree,
};

const SANDBOX: &str = "agentos-sandbox-2";

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn stamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn kv(key: &str) -> Option<String> {
    store::kv_get(&store::open().unwrap(), key).unwrap()
}

fn kv_set(key: &str, value: Option<&str>) {
    let c = store::open().unwrap();
    match value {
        Some(v) => store::kv_set(&c, key, v).unwrap(),
        None => store::kv_delete(&c, key).unwrap(),
    }
}

fn run(dir: &Path, exe: &str, args: &[&str]) -> (bool, String) {
    let bin = procs::find(exe).unwrap_or_else(|| PathBuf::from(exe));
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("PATH", procs::search_path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap_or_else(|e| panic!("{exe} could not run: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn must(dir: &Path, exe: &str, args: &[&str]) -> String {
    let (ok, text) = run(dir, exe, args);
    assert!(ok, "{exe} {} failed: {text}", args.join(" "));
    text
}

fn sandbox_dir() -> PathBuf {
    review_agent::project_dir(SANDBOX).expect("the sandbox must be a known project with a folder")
}

/// The app must be running and the agents must already point at this version's hub.
fn preflight() {
    assert!(
        std::net::TcpStream::connect_timeout(
            &"127.0.0.1:47823".parse().unwrap(),
            Duration::from_secs(2)
        )
        .is_ok(),
        "start AgentOS Studio first"
    );
    let cfg = std::fs::read_to_string(procs::hermes_config()).unwrap_or_default();
    let hub = format!("AgentOS Hub\\{}\\", env!("CARGO_PKG_VERSION"));
    assert!(
        cfg.contains(&hub),
        "the agents must run the {} hub before the done check: {hub} is not in {}",
        env!("CARGO_PKG_VERSION"),
        procs::hermes_config().display()
    );
}

fn notes(v: &Vault) -> Vec<PathBuf> {
    v.list_memory_notes()
        .into_iter()
        .map(|n| v.root.join(&n.relative_path))
        .collect()
}

fn memory_bytes() -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let dir = procs::hermes_home().join("memories");
    (
        std::fs::read(dir.join("MEMORY.md")).ok(),
        std::fs::read(dir.join("USER.md")).ok(),
    )
}

fn reject_keys() -> Vec<String> {
    let c = store::open().unwrap();
    let mut st = c
        .prepare("SELECT key FROM kv WHERE key LIKE 'learning.rejects.%'")
        .unwrap();
    let rows = st.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.flatten().collect()
}

/// Everything the tests created, removed on drop, including after a failed assertion.
struct Cleanup {
    vault: Vault,
    missions: Vec<String>,
    tasks: Vec<String>,
    notes_before: Vec<(PathBuf, Option<Vec<u8>>)>,
    notes_written: Vec<PathBuf>,
    kv_delete: Vec<String>,
    kv_restore: Vec<(String, Option<String>)>,
    sessions: Vec<String>,
    profile: Option<String>,
    dirs: Vec<PathBuf>,
}

impl Cleanup {
    fn new(v: &Vault) -> Cleanup {
        Cleanup {
            vault: v.clone(),
            missions: Vec::new(),
            tasks: Vec::new(),
            notes_before: Vec::new(),
            notes_written: Vec::new(),
            kv_delete: Vec::new(),
            kv_restore: Vec::new(),
            sessions: Vec::new(),
            profile: None,
            dirs: Vec::new(),
        }
    }

    fn remember_kv(&mut self, key: &str) {
        if !self.kv_restore.iter().any(|(k, _)| k == key) {
            self.kv_restore.push((key.to_string(), kv(key)));
        }
    }

    /// Record every note and its bytes before the run, so a note the run overwrites is put back.
    fn snapshot_notes(&mut self, v: &Vault) {
        self.notes_before = notes(v)
            .into_iter()
            .map(|p| {
                let bytes = std::fs::read(&p).ok();
                (p, bytes)
            })
            .collect();
    }

    fn watch_note(&mut self, path: &Path) {
        self.notes_written.push(path.to_path_buf());
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for id in &self.missions {
            if let Some(m) = missions::get(id).ok().flatten() {
                if matches!(m.status.as_str(), "running" | "review") {
                    let _ = missions::block(id, "Phase E done check cleanup", false);
                }
                if let Some(dir) = review_agent::project_dir(&m.repo) {
                    let _ = worktree::discard(&dir, id);
                }
            }
            let _ = missions::delete(id);
            let c = store::open().unwrap();
            for k in [
                "note", "pin", "worktree", "urgent", "defer", "beat", "retries",
            ] {
                let _ = store::kv_delete(&c, &format!("mission.{k}.{id}"));
            }
            let _ = store::kv_delete(&c, &format!("learning.done.{id}"));
        }
        for id in &self.missions {
            remove_bus_files(&self.vault, id);
        }
        for id in &self.tasks {
            if let Ok((task, _)) = self.vault.get_task(id) {
                let _ = std::fs::remove_file(&task.path);
            }
        }
        for path in &self.notes_written {
            match self.notes_before.iter().find(|(p, _)| p == path) {
                Some((_, Some(bytes))) => {
                    let _ = std::fs::write(path, bytes);
                }
                _ => {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        let c = store::open().unwrap();
        for k in &self.kv_delete {
            let _ = store::kv_delete(&c, k);
        }
        for (k, v) in &self.kv_restore {
            match v {
                Some(v) => {
                    let _ = store::kv_set(&c, k, v);
                }
                None => {
                    let _ = store::kv_delete(&c, k);
                }
            }
        }
        if let Some(hermes) = procs::find("hermes") {
            for sid in &self.sessions {
                let mut args: Vec<&str> = Vec::new();
                if let Some(p) = &self.profile {
                    args.extend(["-p", p.as_str()]);
                }
                args.extend(["sessions", "delete", sid.as_str(), "--yes"]);
                let _ = Command::new(&hermes)
                    .args(&args)
                    .env("PATH", procs::search_path())
                    .output();
            }
        }
        for path in &self.dirs {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// Bus task files are named `<id>_<slug>.md` and carry `Mission: <id>.`; remove any that do.
fn remove_bus_files(v: &Vault, mission_id: &str) {
    let marker = format!("Mission: {mission_id}.");
    for state in State::ALL {
        if let Ok(rd) = std::fs::read_dir(v.state_dir(state)) {
            for e in rd.flatten() {
                let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                if text.contains(&marker) {
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
}

fn session_in(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("source_session:"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A branch checked out in a throwaway worktree of the sandbox, with a pull request once opened.
struct PrBranch {
    repo: PathBuf,
    worktree: PathBuf,
    branch: String,
    url: Option<String>,
}

impl PrBranch {
    fn open(&mut self, title: &str, body: &str) {
        must(&self.worktree, "git", &["add", "-A"]);
        must(
            &self.worktree,
            "git",
            &[
                "-c",
                "user.name=Qoder",
                "-c",
                "user.email=qoder@example.invalid",
                "commit",
                "-q",
                "-m",
                title,
            ],
        );
        must(
            &self.worktree,
            "git",
            &["push", "-q", "-u", "origin", &self.branch],
        );
        let branch = self.branch.clone();
        let out = must(
            &self.repo,
            "gh",
            &[
                "pr", "create", "--base", "main", "--head", &branch, "--title", title, "--body",
                body,
            ],
        );
        let url = out
            .lines()
            .rev()
            .find(|l| l.trim().starts_with("https://"))
            .map(|l| l.trim().to_string())
            .unwrap_or_else(|| panic!("gh pr create printed no URL: {out}"));
        self.url = Some(url);
    }

    fn number(&self) -> u64 {
        self.url
            .as_deref()
            .and_then(|u| u.trim_end_matches('/').rsplit('/').next())
            .and_then(|n| n.parse().ok())
            .expect("the pull request URL ends in a number")
    }
}

impl Drop for PrBranch {
    fn drop(&mut self) {
        if let Some(url) = &self.url {
            let _ = run(&self.repo, "gh", &["pr", "close", url]);
        }
        let wt = self.worktree.display().to_string();
        let _ = run(&self.repo, "git", &["worktree", "remove", "--force", &wt]);
        let _ = run(&self.repo, "git", &["branch", "-D", &self.branch]);
        let _ = run(
            &self.repo,
            "git",
            &["push", "origin", "--delete", &self.branch],
        );
        let _ = run(&self.repo, "git", &["worktree", "prune"]);
    }
}

fn new_pr_branch(repo: &Path, branch: &str) -> PrBranch {
    let _ = run(repo, "git", &["fetch", "-q", "origin", "main"]);
    let worktree = repo.join(".worktrees").join(branch.replace('/', "-"));
    let _ = run(
        repo,
        "git",
        &[
            "worktree",
            "remove",
            "--force",
            &worktree.display().to_string(),
        ],
    );
    must(
        repo,
        "git",
        &[
            "worktree",
            "add",
            "-q",
            &worktree.display().to_string(),
            "-b",
            branch,
            "origin/main",
        ],
    );
    PrBranch {
        repo: repo.to_path_buf(),
        worktree,
        branch: branch.to_string(),
        url: None,
    }
}

fn write(dir: &Path, file: &str, text: &str) {
    let path = dir.join(file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

#[test]
#[ignore = "needs the running release build; writes and removes one convention note in the real vault"]
fn two_matching_rejects_make_one_convention_and_the_next_mission_carries_it() {
    preflight();
    let v = Vault::new(settings::vault_path());
    let mut cleanup = Cleanup::new(&v);

    // No mission starts while the fixture is seeded.
    cleanup.remember_kv("automation.enabled");
    kv_set("automation.enabled", Some("0"));
    let keys_before = reject_keys();
    cleanup.snapshot_notes(&v);
    let marker = format!("phase-e-done-{}", stamp());

    // Two blocked missions rejected for the same blocker, worded differently.
    let mut reject = |note: String| {
        let m = missions::create(
            SANDBOX,
            "Seed a rejection for the learning loop",
            "The seed exists only to feed the reject matcher",
            "implementer",
            "claude-code",
            "claude-subscription",
            "",
            0.0,
        )
        .unwrap();
        let c = store::open().unwrap();
        c.execute(
            "UPDATE missions SET status = 'blocked', reviewer = 'qoder', review_verdict = 'reject',
                review_note = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![note, now(), m.id],
        )
        .unwrap();
        cleanup.missions.push(m.id.clone());
        m.id
    };
    let first_id = reject(format!("1. The {marker} helper has no test."));
    let second_id = reject(format!(
        "Two blockers:\n- the {marker} HELPER has no test\n- the readme is thin"
    ));

    let written = learning::repeated_rejects(&v, SANDBOX).expect("the reject pass");
    println!("convention notes: {written:?}");
    for path in &written {
        cleanup.watch_note(Path::new(path));
    }
    let matching: Vec<&String> = written
        .iter()
        .filter(|p| {
            std::fs::read_to_string(p)
                .map(|t| t.contains(&marker))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "two rejects with the same blocker must make exactly one convention note: {written:?}"
    );
    let note = PathBuf::from(matching[0]);
    let text = std::fs::read_to_string(&note).expect("the convention note exists");
    assert!(
        text.contains(&marker),
        "the note should name the shared blocker: {text}"
    );
    assert!(
        text.contains(&first_id) && text.contains(&second_id),
        "the note should link both rejected missions: {text}"
    );
    let again = learning::repeated_rejects(&v, SANDBOX).expect("the rerun");
    assert!(again.is_empty(), "a rerun writes nothing: {again:?}");

    // The next mission started in the repository must carry the convention in its instructions.
    let next = missions::create(
        SANDBOX,
        &format!("Prove the convention reaches the next mission ({marker})"),
        "Read the repository conventions before starting",
        "implementer",
        "qoder",
        "",
        "",
        0.0,
    )
    .unwrap();
    cleanup.missions.push(next.id.clone());
    // qoder is IDE only here, so starting it creates a bus task and no worker.
    let queued = missions::get(&next.id).unwrap().unwrap();
    let started = if queued.status == "queued" {
        missions::start(&v, &next.id).expect("the next mission starts")
    } else {
        queued
    };
    let task = started
        .task_id
        .clone()
        .expect("an IDE mission gets a bus task");
    cleanup.tasks.push(task.clone());
    let (_, body) = v.get_task(&task).expect("the bus task exists");
    println!(
        "instructions carry the convention: {}",
        body.contains(&marker)
    );
    assert!(
        body.contains(&marker),
        "the mission instructions must carry the repository convention. If this is the failing \
         clause, the E2 hook (conventions_for added to the mission instructions) is not merged; \
         see the coordination note."
    );

    // Keys this run wrote are new, so remove them.
    for k in reject_keys() {
        if !keys_before.contains(&k) {
            cleanup.kv_delete.push(k);
        }
    }
}

#[test]
#[ignore = "needs a real Hermes lesson turn on a throwaway profile and the E2 review hook; owner must allow Hermes use"]
fn a_finished_mission_writes_lessons_without_a_human_step() {
    preflight();
    let v = Vault::new(settings::vault_path());

    // A throwaway profile with the active profile's settings, used only for the lesson turn. It is
    // created before Cleanup so Cleanup drops first and can delete the profile's sessions.
    let name = format!("e3-done-{}", stamp());
    assert!(
        !procs::hermes_home().join("profiles").join(&name).exists(),
        "refusing to overwrite an existing profile"
    );
    let _profile = throwaway_profile(&name);
    let mut cleanup = Cleanup::new(&v);
    cleanup.profile = Some(name.clone());
    let before_memory = memory_bytes();
    cleanup.snapshot_notes(&v);
    let known: Vec<PathBuf> = cleanup
        .notes_before
        .iter()
        .map(|(p, _)| p.clone())
        .collect();
    let dir = sandbox_dir();

    cleanup.remember_kv("hermes.profile.reviewer");
    kv_set("hermes.profile.reviewer", Some(&name));

    // A real pull request in the sandbox, so the lesson prompt reads a real diff.
    let branch = format!("phase-e/lessons-{name}");
    let mut pr = new_pr_branch(&dir, &branch);
    // The sandbox exports with CommonJS. The change is trivial and matches the acceptance.
    let lib = std::fs::read_to_string(pr.worktree.join("lib.js")).unwrap();
    write(
        &pr.worktree,
        "lib.js",
        &format!(
            "{lib}\nfunction retryOnce(fn) {{\n  try {{\n    return fn();\n  }} catch (err) {{\n    return fn();\n  }}\n}}\nmodule.exports = {{ add, retryOnce }};\n"
        ),
    );
    write(
        &pr.worktree,
        "test.js",
        "const assert = require(\"assert\");\nconst { add, retryOnce } = require(\"./lib\");\nassert.strictEqual(add(2, 3), 5);\nlet attempts = 0;\nconst value = retryOnce(() => {\n  attempts += 1;\n  if (attempts === 1) throw new Error(\"transient\");\n  return 7;\n});\nassert.strictEqual(value, 7);\nassert.strictEqual(attempts, 2);\nconsole.log(\"ok\");\n",
    );
    let (ok, out) = run(&pr.worktree, "node", &["test.js"]);
    assert!(
        ok,
        "the pull request must pass node test.js before it is opened: {out}"
    );
    pr.open(
        "Add retryOnce with a test",
        "Adds `retryOnce(fn)`, which retries one transient failure exactly once, and covers it in test.js. Acceptance: lib.js exports retryOnce, node test.js passes, nothing else changes.",
    );
    let number = pr.number();
    wait_for_checks(&pr.repo, number);

    // The mission fixture points at the real pull request; the verdict is recorded below.
    let m = missions::create(
        SANDBOX,
        "Add retryOnce with a test",
        "lib.js exports retryOnce; test.js asserts retryOnce returns 7 after one transient failure; node test.js passes; nothing else changes",
        "implementer",
        "claude-code",
        "claude-subscription",
        "",
        0.0,
    )
    .unwrap();
    cleanup.missions.push(m.id.clone());
    {
        let c = store::open().unwrap();
        c.execute(
            "UPDATE missions SET status = 'review', pr_url = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![pr.url.as_deref().unwrap(), now(), m.id],
        )
        .unwrap();
    }
    // The review path moves the mission to done and, with the E2 hook, extracts lessons itself.
    // The verdict is recorded directly (the reviewer's prose is not the subject of this check).
    missions::review(
        &m.id,
        "qoder",
        "approve",
        "The change matches the acceptance criteria.",
    )
    .expect("recording the approving verdict");
    let after = missions::get(&m.id).unwrap().unwrap();
    assert_eq!(
        after.status, "done",
        "the mission is finished: {:?}",
        after.status
    );

    let deadline = Instant::now() + Duration::from_secs(300);
    let mut fresh: Vec<PathBuf> = Vec::new();
    while Instant::now() < deadline {
        fresh = notes(&v)
            .into_iter()
            .filter(|p| !known.contains(p))
            .collect();
        let learned: Vec<&PathBuf> = fresh
            .iter()
            .filter(|p| {
                std::fs::read_to_string(p)
                    .map(|t| t.contains("agent: agentos-learning"))
                    .unwrap_or(false)
            })
            .collect();
        if !learned.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    for path in &fresh {
        cleanup.watch_note(path);
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(sid) = session_in(&text) {
                cleanup.sessions.push(sid);
            }
        }
    }
    let learned: Vec<PathBuf> = fresh
        .into_iter()
        .filter(|p| {
            std::fs::read_to_string(p)
                .map(|t| t.contains("agent: agentos-learning"))
                .unwrap_or(false)
        })
        .collect();
    println!("lesson notes: {learned:?}");
    assert!(
        !learned.is_empty(),
        "the finished mission must produce lesson notes through remember with no human step. If \
         this is the failing clause, the E2 hook (the review path calls \
         learning::on_mission_reviewed) is not merged; see the coordination note."
    );
    let text = std::fs::read_to_string(&learned[0]).unwrap();
    assert!(
        text.contains(&format!("repo: {SANDBOX}")),
        "a lesson note should name the repository: {text}"
    );
    assert!(
        text.contains("source_session:"),
        "a lesson note should name the Hermes session that wrote it: {text}"
    );

    assert_eq!(
        memory_bytes(),
        before_memory,
        "the owner's default MEMORY.md and USER.md must be untouched"
    );
}

#[test]
#[ignore = "needs a real pull request merged with gh in the sandbox and the E5 merge hook"]
fn a_mission_merged_by_studio_carries_the_provenance_trailers() {
    preflight();
    let v = Vault::new(settings::vault_path());
    let mut cleanup = Cleanup::new(&v);
    let dir = sandbox_dir();

    let branch = format!("phase-e/trailers-{}", stamp());
    let mut pr = new_pr_branch(&dir, &branch);
    let lib = std::fs::read_to_string(pr.worktree.join("lib.js")).unwrap();
    write(
        &pr.worktree,
        "lib.js",
        &format!(
            "{lib}\nfunction multiply(a, b) {{\n  return a * b;\n}}\nmodule.exports = {{ add, multiply }};\n"
        ),
    );
    write(
        &pr.worktree,
        "test.js",
        "const assert = require(\"assert\");\nconst { add, multiply } = require(\"./lib\");\nassert.strictEqual(add(2, 3), 5);\nassert.strictEqual(multiply(3, 4), 12);\nconsole.log(\"ok\");\n",
    );
    let (ok, out) = run(&pr.worktree, "node", &["test.js"]);
    assert!(
        ok,
        "the pull request must pass node test.js before it is opened: {out}"
    );
    pr.open(
        "Add multiply with a test",
        "Adds `multiply(a, b)` and covers it in test.js. Acceptance: lib.js exports multiply, node test.js passes, nothing else changes.",
    );
    let number = pr.number();
    wait_for_checks(&dir, number);

    // A finished, approved mission for the pull request; Studio's gate does the merge.
    let m = missions::create(
        SANDBOX,
        "Add multiply with a test",
        "lib.js exports multiply; test.js asserts multiply(3, 4) === 12; node test.js passes; nothing else changes",
        "implementer",
        "claude-code",
        "claude-subscription",
        "claude-sonnet-4-5",
        0.0,
    )
    .unwrap();
    cleanup.missions.push(m.id.clone());
    {
        let c = store::open().unwrap();
        c.execute(
            "UPDATE missions SET status = 'done', pr_url = ?1, reviewer = 'qoder',
                review_verdict = 'approve', review_note = 'The change matches the acceptance criteria.',
                updated_at = ?2 WHERE id = ?3",
            rusqlite::params![pr.url.as_deref().unwrap(), now(), m.id],
        )
        .unwrap();
    }

    let tree_before = must(&dir, "git", &["rev-parse", "origin/main^{tree}"])
        .trim()
        .to_string();
    let mut merged = None;
    for _ in 0..12 {
        let r = review_agent::merge(&m.id, true);
        println!("merge: {r:?}");
        let retry = matches!(&r, Err(e) if e.to_string().contains("CI is not green"));
        merged = Some(r);
        if !retry {
            break;
        }
        std::thread::sleep(Duration::from_secs(15));
    }
    assert!(
        merged.as_ref().is_some_and(|r| r.is_ok()),
        "the gate merged the pull request: {merged:?}"
    );
    // From here on the sandbox main carries the merge, so every later assertion runs under a guard
    // that reverts it, including when the missing E5 hook makes the trailer check fail.
    let mut undo = UndoMerge::new(dir.clone());

    // Read the squash commit from origin, then blame the added line in a detached worktree.
    let _ = run(&dir, "git", &["fetch", "-q", "origin", "main"]);
    let message = must(&dir, "git", &["log", "-1", "--format=%B", "origin/main"]);
    println!("squash commit:\n{message}");
    let trailers = provenance::parse_trailers(&message);
    assert_eq!(
        trailers.mission.as_deref(),
        Some(m.id.as_str()),
        "the squash commit must carry Mission: {}. If this is the failing clause, the E5 hook \
         (the merge appends provenance::trailers) is not merged; see the coordination note. \
         Commit message:\n{message}",
        m.id
    );
    assert_eq!(
        trailers.agent.as_deref(),
        Some("claude-code"),
        "{trailers:?}"
    );
    assert_eq!(
        trailers.model.as_deref(),
        Some("claude-sonnet-4-5"),
        "{trailers:?}"
    );
    assert_eq!(trailers.reviewer.as_deref(), Some("qoder"), "{trailers:?}");
    assert_eq!(trailers.cost.as_deref(), Some("flat"), "{trailers:?}");

    let blame_wt = dir.join(".worktrees").join(format!("blame-{}", stamp()));
    let blame_wt_s = blame_wt.display().to_string();
    must(
        &dir,
        "git",
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &blame_wt_s,
            "origin/main",
        ],
    );
    cleanup.dirs.push(blame_wt.clone());
    let lib_at_origin = std::fs::read_to_string(blame_wt.join("lib.js")).unwrap();
    let line = lib_at_origin
        .lines()
        .position(|l| l.contains("function multiply"))
        .map(|i| i as u64 + 1)
        .expect("multiply is in lib.js on origin/main");
    let got = provenance::why(&blame_wt_s, "lib.js", line).expect("blame the added line");
    println!(
        "why: source={} mission={:?}",
        got.source,
        got.mission.as_ref().map(|x| &x.id)
    );
    assert_eq!(got.source, "trailers", "{got:?}");
    let found = got
        .mission
        .expect("the blamed line resolves to the mission");
    assert_eq!(
        found.id, m.id,
        "Why is this here? names the mission behind the line"
    );
    assert_eq!(found.agent, "claude-code");
    assert_eq!(found.cost, "flat");

    let _ = run(&dir, "git", &["worktree", "remove", "--force", &blame_wt_s]);
    // Undo the merge on the sandbox so the fixture stays as it was.
    undo.undo();
    let mut tree_after = String::new();
    for _ in 0..10 {
        let _ = run(&dir, "git", &["fetch", "-q", "origin", "main"]);
        tree_after = run(&dir, "git", &["rev-parse", "origin/main^{tree}"])
            .1
            .trim()
            .to_string();
        if tree_after == tree_before {
            break;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    assert_eq!(
        tree_after, tree_before,
        "the revert must return the sandbox main tree to its state before the merge"
    );
}

/// Reverts the squash merge on the sandbox main. `undo` is idempotent and runs from `Drop`, so a
/// failed assertion after the merge never leaves the fixture changed.
struct UndoMerge {
    repo: PathBuf,
    active: bool,
}

impl UndoMerge {
    fn new(repo: PathBuf) -> UndoMerge {
        UndoMerge { repo, active: true }
    }

    fn undo(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let wt = self
            .repo
            .join(".worktrees")
            .join(format!("revert-{}", stamp()));
        let wt_s = wt.display().to_string();
        let _ = run(&self.repo, "git", &["fetch", "-q", "origin", "main"]);
        let added = run(
            &self.repo,
            "git",
            &["worktree", "add", "-q", "--detach", &wt_s, "origin/main"],
        )
        .0;
        if added {
            let reverted = run(&wt, "git", &["revert", "--no-edit", "HEAD"]).0;
            if reverted {
                let _ = run(&wt, "git", &["push", "-q", "origin", "HEAD:main"]);
            }
            let _ = run(&self.repo, "git", &["worktree", "remove", "--force", &wt_s]);
        }
        let _ = run(&self.repo, "git", &["worktree", "prune"]);
    }
}

impl Drop for UndoMerge {
    fn drop(&mut self) {
        self.undo();
    }
}

fn wait_for_checks(repo: &Path, number: u64) {
    let slug = github::name_with_owner(SANDBOX).expect("the sandbox has a GitHub remote");
    let deadline = Instant::now() + Duration::from_secs(10 * 60);
    let mut last = String::new();
    while Instant::now() < deadline {
        match github::pr_checks_green(&slug, number) {
            Ok(true) => {
                println!("checks green on #{number}");
                return;
            }
            Ok(false) => {}
            Err(e) => {
                let line = format!("checks: {e:#}");
                if line != last {
                    println!("{line}");
                    last = line;
                }
            }
        }
        std::thread::sleep(Duration::from_secs(15));
    }
    let (_, text) = run(repo, "gh", &["pr", "checks", &number.to_string()]);
    panic!("checks were not green on #{number} within 10 minutes: {text}");
}

/// A throwaway Hermes profile with the active profile's provider settings. `hermes profile create
/// --clone` copies plugin directories and fails on an unreadable one on this machine, so the
/// settings and the readable plugins are copied instead. Removed on drop, retrying while the
/// gateway lets go of it.
struct ThrowawayProfile(String);

impl Drop for ThrowawayProfile {
    fn drop(&mut self) {
        let Some(hermes) = procs::find("hermes") else {
            return;
        };
        let _ = Command::new(&hermes)
            .args(["-p", &self.0, "gateway", "stop"])
            .env("PATH", procs::search_path())
            .output();
        for _ in 0..6 {
            let _ = Command::new(&hermes)
                .args(["profile", "delete", &self.0, "-y"])
                .env("PATH", procs::search_path())
                .output();
            if !procs::hermes_home().join("profiles").join(&self.0).exists() {
                return;
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    if std::fs::create_dir_all(dst).is_err() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(src) else {
        return;
    };
    for entry in entries.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else if std::fs::copy(&from, &to).is_err() {
            eprintln!("skipping unreadable {}", from.display());
        }
    }
}

fn throwaway_profile(name: &str) -> ThrowawayProfile {
    let hermes = procs::find("hermes").expect("hermes is installed");
    let made = Command::new(&hermes)
        .args(["profile", "create", name, "--no-alias"])
        .env("PATH", procs::search_path())
        .output()
        .expect("hermes is runnable");
    assert!(
        made.status.success(),
        "hermes profile create failed: {}",
        String::from_utf8_lossy(&made.stderr)
    );
    let profile = ThrowawayProfile(name.to_string());
    let home = procs::hermes_home();
    let dst = home.join("profiles").join(name);
    for file in ["config.yaml", ".env", "SOUL.md"] {
        let src = home.join(file);
        if src.is_file() {
            std::fs::copy(&src, dst.join(file)).expect("copy the active profile's settings");
        }
    }
    copy_tree(&home.join("plugins"), &dst.join("plugins"));
    profile
}
