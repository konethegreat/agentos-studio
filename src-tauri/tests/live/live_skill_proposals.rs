//! Live skill-proposal check: three seeded approved missions produce one proposal, and approving it
//! on a throwaway Hermes profile creates the skill there. The profile is deleted afterwards.
//! `cargo test --test live -- live_skill_proposals:: --ignored --nocapture --test-threads 1`
//!
//! Run only when the owner has allowed Hermes use. Hold the live-machine lock first
//! (`04_Agent_Engine/locks` in the vault) and delete it after. The test writes only to a temporary
//! database and a throwaway profile; the owner's skills are never touched.

use std::path::{Path, PathBuf};
use std::process::Command;

use agentos_studio_lib::{missions, skill_proposals, store, vault::Vault};

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

fn seed_approved(repo: &str, goal: &str, agent: &str) -> String {
    let m = missions::create(
        repo,
        goal,
        "The change is on main",
        "implementer",
        agent,
        "",
        "",
        0.0,
    )
    .unwrap();
    let c = store::open().unwrap();
    c.execute(
        "UPDATE missions SET status = 'done', reviewer = 'qoder', review_verdict = 'approve',
            updated_at = ?1 WHERE id = ?2",
        rusqlite::params![chrono::Utc::now().timestamp(), m.id],
    )
    .unwrap();
    m.id
}

#[test]
#[ignore = "creates and deletes a real Hermes profile and runs one paid model turn"]
fn three_approved_missions_propose_one_skill_and_approval_creates_it() {
    let tmp = std::env::temp_dir().join(format!("skill-proposals-live-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    std::env::set_var("AGENTOS_STUDIO_DB", tmp.join("studio.db"));
    std::env::set_var("AGENTOS_STUDIO_MEMORY_DIR", tmp.join("memory-index"));

    let v = Vault::new(tmp.join("vault"));
    v.scaffold().unwrap();

    let ids = vec![
        seed_approved("agentos-studio", "Add the retry helper", "hermes"),
        seed_approved("agentos-studio", "add a Retry Helper", "qoder"),
        seed_approved("agentos-studio", "Add retry helper", "claude-code"),
    ];
    println!("seeded approved missions: {ids:?}");

    let proposals = skill_proposals::proposals(&v).expect("proposals should be readable");
    let p = proposals
        .iter()
        .find(|p| p.task_class == "add retry helper")
        .unwrap_or_else(|| panic!("no proposal for the seeded class: {proposals:?}"));
    assert_eq!(p.evidence.len(), 3, "{p:?}");
    assert!(p.description.starts_with("Use when"));

    let name = format!(
        "e4-skills-{}",
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

    let created =
        skill_proposals::approve(p, Some(&name)).expect("the skill turn should create it");
    println!("created skill: {created}");
    let root = skill_proposals::skill_root(Some(&name)).unwrap();
    let installed = agentos_studio_lib::skills::scan(&root);
    assert!(
        installed.iter().any(|s| s.name == p.name),
        "the profile does not list {}: {installed:?}",
        p.name
    );

    // The throwaway profile carries the skill away when it is deleted.
    drop(profile);
    assert!(
        !home().join("profiles").join(&name).exists(),
        "the throwaway profile should be gone"
    );
    for f in [format!("{}.md", p.name), format!("{}.prompt.md", p.name)] {
        let _ = std::fs::remove_file(
            dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("agentos-studio")
                .join("skill-proposals")
                .join(f),
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
