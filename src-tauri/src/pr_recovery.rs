//! Delivery for a headless worker that stopped leaving work on its branch but no pull request.
//!
//! A worker exits, AgentOS Studio finds commits on the attempt's branch that no remote branch
//! carries, so it pushes the branch and opens the pull request itself. Work that exists must reach
//! GitHub: the mission gate reviews it, and a finished attempt never disappears as "no pull
//! request to review". A run that changed nothing is left open for the stuck check instead, so a
//! denied command cannot look like completed work.

use std::path::Path;

use anyhow::{anyhow, bail, Result};

use crate::procs;

/// What a finished attempt left behind. Pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Commits on the branch that the base does not have.
    Commits(u64),
    /// No commits, but the worktree holds uncommitted changes.
    Uncommitted,
    /// Nothing at all: the attempt did not work on the repository.
    Empty,
}

/// Classify an attempt from the commits it added and whether its worktree is dirty. Pure.
pub fn classify(commits: u64, dirty: bool) -> Output {
    if commits > 0 {
        Output::Commits(commits)
    } else if dirty {
        Output::Uncommitted
    } else {
        Output::Empty
    }
}

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let exe = procs::find("git")?;
    let out = procs::command(&exe)
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Commits on the branch that its base does not have: the remote default branch, then `main` or
/// `master` when the repository has no remote. Zero when no base can be found, which keeps the
/// caller on the "leave it open" path rather than guessing.
pub fn ahead(worktree: &Path) -> u64 {
    for base in [
        "origin/HEAD",
        "origin/main",
        "origin/master",
        "main",
        "master",
    ] {
        if git_out(worktree, &["rev-parse", "--verify", "--quiet", base]).is_none() {
            continue;
        }
        let range = format!("{base}..HEAD");
        if let Some(n) = git_out(worktree, &["rev-list", "--count", &range])
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            return n;
        }
    }
    0
}

/// Whether the worktree holds uncommitted changes. False when git cannot answer.
pub fn dirty(worktree: &Path) -> bool {
    git_out(worktree, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty())
}

/// Push the branch and open its pull request, returning the URL. An open pull request for the
/// branch is returned as it is, so a worker that opened one but never reported it is not
/// duplicated. Errors are returned, never swallowed: the caller leaves the task open so the next
/// attempt retries instead of losing the commits.
pub fn deliver(worktree: &Path, branch: &str, title: &str, body: &str) -> Result<String> {
    let branch = branch.trim();
    if branch.is_empty() || branch.starts_with('-') {
        bail!("refusing to push branch '{branch}'");
    }
    let git = procs::find("git").ok_or_else(|| anyhow!("git is not installed"))?;
    let push = procs::command(&git)
        .args(["push", "-u", "origin", branch])
        .current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .output()?;
    if !push.status.success() {
        bail!(
            "git push {branch} failed: {}",
            String::from_utf8_lossy(&push.stderr).trim()
        );
    }
    let gh = procs::find("gh").ok_or_else(|| anyhow!("gh is not installed"))?;
    let existing = procs::command(&gh)
        .args([
            "pr", "list", "--head", branch, "--state", "open", "--json", "url", "--jq", ".[0].url",
        ])
        .current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .output()?;
    if existing.status.success() {
        let url = String::from_utf8_lossy(&existing.stdout).trim().to_string();
        if url.starts_with("https://") {
            return Ok(url);
        }
    }
    let create = procs::command(&gh)
        .args([
            "pr", "create", "--head", branch, "--title", title, "--body", body,
        ])
        .current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .output()?;
    if !create.status.success() {
        bail!(
            "gh pr create failed: {}",
            String::from_utf8_lossy(&create.stderr).trim()
        );
    }
    String::from_utf8_lossy(&create.stdout)
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("https://"))
        .map(str::to_string)
        .ok_or_else(|| anyhow!("gh pr create printed no pull request URL"))
}

/// The last auto-denied action in a worker log tail, for the note that explains why a run changed
/// nothing. Pure.
pub fn denied_hint(tail: &str) -> String {
    tail.lines()
        .rev()
        .find(|l| l.contains("denied:"))
        .map(|l| {
            let t: String = l.trim().chars().take(220).collect();
            format!(" (last denied action: {t})")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) -> String {
        let exe = procs::find("git").expect("git on PATH for this test");
        let out = Command::new(exe)
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(dir: &Path, name: &str) {
        fs::write(dir.join(name), "x").unwrap();
        run(dir, &["add", name]);
        run(
            dir,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                name,
            ],
        );
    }

    #[test]
    fn an_attempt_is_classified_by_what_it_left() {
        assert_eq!(classify(2, true), Output::Commits(2));
        assert_eq!(classify(1, false), Output::Commits(1));
        assert_eq!(classify(0, true), Output::Uncommitted);
        assert_eq!(classify(0, false), Output::Empty);
    }

    #[test]
    fn commits_ahead_of_the_local_default_branch_are_counted() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        run(dir, &["init", "-b", "main"]);
        commit(dir, "base.txt");
        run(dir, &["checkout", "-b", "agentos/m-001-1"]);
        commit(dir, "work.txt");
        assert_eq!(ahead(dir), 1);
        assert!(!dirty(dir));
        fs::write(dir.join("scratch.txt"), "x").unwrap();
        assert!(dirty(dir));
    }

    #[test]
    fn a_clean_branch_has_nothing_ahead() {
        let d = tempfile::tempdir().unwrap();
        let dir: PathBuf = d.path().to_path_buf();
        run(&dir, &["init", "-b", "main"]);
        commit(&dir, "base.txt");
        assert_eq!(ahead(&dir), 0);
        assert_eq!(classify(ahead(&dir), dirty(&dir)), Output::Empty);
    }

    #[test]
    fn the_denied_hint_names_the_last_refused_action() {
        let tail = "step one\ndenied: [{\"action\":\"command\"}]\nstep two";
        assert_eq!(
            denied_hint(tail),
            " (last denied action: denied: [{\"action\":\"command\"}])"
        );
        assert_eq!(denied_hint("all clean"), "");
    }

    #[test]
    fn a_branch_with_no_name_or_a_flag_is_refused() {
        let d = tempfile::tempdir().unwrap();
        assert!(deliver(d.path(), " ", "t", "b").is_err());
        assert!(deliver(d.path(), "-x", "t", "b").is_err());
    }
}
