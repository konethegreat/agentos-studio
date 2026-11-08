//! Live eval check: replay one finished sandbox mission on another agent or model, and leave
//! nothing behind except one note. The note goes to a throwaway vault, never the owner's.
//! `cargo test --test live_eval -- --ignored --nocapture --test-threads 1`
//!
//! Address the mission and the agent, for example:
//! `AGENTOS_EVAL_MISSION=M-123 AGENTOS_EVAL_AGENT=claude-code AGENTOS_EVAL_MODEL=claude-opus-5-5`
//!
//! Run only when the owner has allowed the agent to be invoked. Hold the live-machine lock first
//! (`04_Agent_Engine/locks` in the vault) and delete it after. The replay spends agent time.

use std::path::{Path, PathBuf};
use std::process::Command;

use agentos_studio_lib::vault::{Doc, Vault};
use agentos_studio_lib::{eval, learning, missions};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git is installed");
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn worktrees(dir: &Path) -> Vec<String> {
    git(dir, &["worktree", "list", "--porcelain"])
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .map(String::from)
        .collect()
}

fn branches(dir: &Path) -> Vec<String> {
    git(
        dir,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .lines()
    .map(String::from)
    .collect()
}

#[test]
#[ignore]
fn replays_a_finished_mission_and_leaves_nothing_behind() {
    let Ok(id) = std::env::var("AGENTOS_EVAL_MISSION") else {
        println!("AGENTOS_EVAL_MISSION is not set; nothing to replay");
        return;
    };
    let agent = std::env::var("AGENTOS_EVAL_AGENT").unwrap_or_else(|_| "claude-code".into());
    let model = std::env::var("AGENTOS_EVAL_MODEL").ok();
    let m = missions::get(&id)
        .expect("missions")
        .expect("the mission exists");
    assert!(
        m.status == "done" || m.status == "failed",
        "{} is {}; only a finished mission can be replayed",
        m.id,
        m.status
    );
    let dir: PathBuf = learning::workspace_for(&m.repo).expect("the repository has a checkout");
    let worktrees_before = worktrees(&dir);
    let branches_before = branches(&dir);
    let policy = dir.join(".agentos/routing.json");
    let policy_before = std::fs::read(&policy).ok();

    let vault_dir = tempfile::tempdir().expect("a temp vault");
    let v = Vault::new(vault_dir.path());
    let r = eval::run(&v, &id, &agent, model.as_deref()).expect("the replay runs");

    assert!(r.score <= 100, "score {} is out of range", r.score);
    assert!(matches!(r.verdict.as_str(), "approve" | "reject"));
    assert!(
        Path::new(&r.note_path).is_file(),
        "no note at {}",
        r.note_path
    );
    let note = std::fs::read_to_string(&r.note_path).expect("the note is readable");
    let d = Doc::parse(&note);
    assert_eq!(d.get("type"), Some("eval"));
    assert_eq!(d.get("mission"), Some(m.id.as_str()));
    assert_eq!(d.get("agent"), Some(r.agent.as_str()));
    assert_eq!(eval::list(&v).expect("list").len(), 1);

    // No worktree, no branch: the replay is detached and its sandbox removes itself.
    assert_eq!(
        worktrees(&dir),
        worktrees_before,
        "the replay left a worktree behind"
    );
    assert_eq!(
        branches(&dir),
        branches_before,
        "the replay left a local branch behind"
    );
    // Nothing about routing changed.
    assert_eq!(std::fs::read(&policy).ok(), policy_before);
    println!(
        "{} on {}: {} vs original {} ({}), {}s, note {}",
        r.mission,
        r.agent,
        r.score,
        r.original_score,
        r.comparison,
        r.duration_s,
        r.note_path
    );
}
