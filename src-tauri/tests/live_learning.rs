//! Live learning-loop check: one real Hermes turn turns a finished sandbox mission into vault
//! notes, two matching rejects become one convention note, and a rerun changes nothing.
//! `cargo test --test live_learning -- --ignored --nocapture --test-threads 1`
//!
//! Run only when the owner has allowed Hermes use. Hold the live-machine lock first
//! (`04_Agent_Engine/locks` in the vault) and delete it after. The turn runs on a throwaway
//! profile built from the active profile's settings, and the profile is deleted afterwards.

use std::path::{Path, PathBuf};
use std::process::Command;

use agentos_studio_lib::{learning, missions, store, vault::Vault};

fn home() -> PathBuf {
    agentos_studio_lib::procs::hermes_home()
}

fn hermes(args: &[&str]) -> String {
    let out = Command::new("hermes")
        .args(args)
        .output()
        .expect("hermes is installed");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "hermes {} failed: {text}",
        args.join(" ")
    );
    text
}

struct ThrowawayProfile(String);

impl Drop for ThrowawayProfile {
    fn drop(&mut self) {
        // The host gateway keeps the profile's state.db open while it serves it. Unserve first,
        // then retry the delete: the gateway rescans within 30 seconds.
        let _ = Command::new("hermes")
            .args(["-p", &self.0, "gateway", "stop"])
            .output();
        for _ in 0..6 {
            let _ = Command::new("hermes")
                .args(["profile", "delete", &self.0, "-y"])
                .output();
            if !home().join("profiles").join(&self.0).exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
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

/// A throwaway profile with the active profile's provider settings. `hermes profile create
/// --clone` copies plugin directories too and fails on an unreadable one (`antigravity-oauth` on
/// this machine), so the settings and readable plugins are copied here instead.
fn throwaway_profile(name: &str) -> ThrowawayProfile {
    hermes(&["profile", "create", name, "--no-alias"]);
    let profile = ThrowawayProfile(name.to_string());
    let home = home();
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

fn memory_bytes() -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let dir = home().join("memories");
    (
        std::fs::read(dir.join("MEMORY.md")).ok(),
        std::fs::read(dir.join("USER.md")).ok(),
    )
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=studio@test", "-c", "user.name=studio"])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git is installed")
        .success();
    assert!(ok, "git {args:?}");
}

/// Vault notes written by the learning loop.
fn learned_notes(v: &Vault) -> Vec<(String, String)> {
    v.list_memory_notes()
        .into_iter()
        .filter_map(|n| {
            let text = v.read_note(&n.relative_path).ok()?;
            text.contains("agent: agentos-learning")
                .then_some((n.relative_path, text))
        })
        .collect()
}

fn seed_reject(repo: &str, note: &str) -> String {
    let m = missions::create(
        repo,
        "Fix the parser",
        "Handles empty input",
        "implementer",
        "hermes",
        "",
        "",
        0.0,
    )
    .unwrap();
    let c = store::open().unwrap();
    c.execute(
        "UPDATE missions SET status = 'queued', reviewer = 'qoder', review_verdict = 'reject',
            review_note = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![note, chrono::Utc::now().timestamp(), m.id],
    )
    .unwrap();
    m.id
}

#[test]
#[ignore = "creates and deletes a real Hermes profile and runs one paid model turn"]
fn a_finished_mission_becomes_notes_and_a_rerun_changes_nothing() {
    let before_memory = memory_bytes();
    let tmp = std::env::temp_dir().join(format!("learning-live-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    std::env::set_var("AGENTOS_STUDIO_DB", tmp.join("studio.db"));
    std::env::set_var("AGENTOS_STUDIO_MEMORY_DIR", tmp.join("memory-index"));

    let repo = tmp.join("sandbox");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        repo.join("README.md"),
        "# Sandbox\n\nA scratch repository for the learning-loop live test.\n",
    )
    .unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "sandbox"]);

    let v = Vault::new(tmp.join("vault"));
    v.scaffold().unwrap();

    let name = format!(
        "e2-learning-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    assert!(
        !home().join("profiles").join(&name).exists(),
        "refusing to overwrite an existing profile"
    );
    let profile = throwaway_profile(&name);
    {
        let c = store::open().unwrap();
        store::kv_set(&c, "hermes.profile.reviewer", &name).unwrap();
    }

    let repo_arg = repo.display().to_string();
    let m = missions::create(
        &repo_arg,
        "Add a retry helper for transient failures",
        "The helper retries exactly once and tests cover it",
        "implementer",
        "hermes",
        "",
        "",
        0.0,
    )
    .unwrap();
    {
        let c = store::open().unwrap();
        c.execute(
            "UPDATE missions SET status = 'done', reviewer = 'qoder', review_verdict = 'approve',
                review_note = 'the retry covers the transient case', updated_at = ?1 WHERE id = ?2",
            rusqlite::params![chrono::Utc::now().timestamp(), m.id],
        )
        .unwrap();
    }

    let first = learning::extract_lessons(&v, &m.id).expect("the learning turn should run");
    println!("learning report: {first:?}");
    assert!(!first.skipped);
    assert!(
        first.dropped.is_none(),
        "the model answer was dropped: {:?}",
        first.dropped
    );
    assert!(
        !first.notes.is_empty(),
        "the turn produced no lessons: {first:?}"
    );
    let session = first
        .session
        .clone()
        .expect("hermes should report the session id it created");
    let written = learned_notes(&v);
    assert!(
        !written.is_empty(),
        "no lesson note carries the learning agent"
    );
    for (path, text) in &written {
        assert!(text.contains("repo:"), "{path} should name its repository");
        assert!(
            text.contains(&format!("source_session: {session}")),
            "{path} should name the session that wrote it"
        );
    }

    let after_first = learned_notes(&v);
    let again = learning::extract_lessons(&v, &m.id).expect("the skipped run should succeed");
    assert!(again.skipped, "{again:?}");
    assert_eq!(learned_notes(&v), after_first, "a rerun changes nothing");

    // Two rejects with the same blockers become one convention note, and only one.
    seed_reject(
        &repo_arg,
        "Two blockers:\n- Tests do not cover the transient case.\n- The helper is not documented.",
    );
    seed_reject(
        &repo_arg,
        "1. The helper is not documented\n2. tests DO NOT cover the transient case",
    );
    let conventions = learning::repeated_rejects(&v, &repo_arg).expect("the reject pass");
    println!("conventions: {conventions:?}");
    assert_eq!(conventions.len(), 1, "{conventions:?}");
    let convention_note = v
        .list_memory_notes()
        .into_iter()
        .find(|n| n.relative_path.contains("Review_blockers_"))
        .expect("the convention note exists");
    let before = v.read_note(&convention_note.relative_path).unwrap();
    assert!(before.contains("Missions:"), "{before}");
    let repeat = learning::repeated_rejects(&v, &repo_arg).expect("the rerun");
    assert!(repeat.is_empty(), "a rerun writes nothing: {repeat:?}");
    assert_eq!(
        v.read_note(&convention_note.relative_path).unwrap(),
        before,
        "the convention note is unchanged"
    );

    drop(profile);
    assert!(
        !home().join("profiles").join(&name).exists(),
        "the throwaway profile should be gone"
    );
    assert_eq!(
        memory_bytes(),
        before_memory,
        "the default memory files must be untouched"
    );
    std::fs::remove_dir_all(&tmp).ok();
}
