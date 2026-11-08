//! One git worktree per mission for agents Studio starts itself (Claude Code, Antigravity) and
//! for IDE agents that pick a mission up. Hermes missions get theirs from `hermes kanban`.
//!
//! Layout: `<repo>/.worktrees/<mission>-<attempt>` on branch `agentos/<mission>-<attempt>`, cut from
//! the remote default branch when there is one. `.worktrees/` goes into the repository's local
//! `.git/info/exclude`, so no tracked file changes. The path is kept in `studio.db` kv
//! `mission.worktree.<id>` and the worktree is removed when the mission ends (done or failed).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::missions::Mission;
use crate::procs;

pub const DIR: &str = ".worktrees";
const KV_PREFIX: &str = "mission.worktree.";

/// `M-007` attempt 1712345678 -> `m-007-1712345678`. Pure.
pub fn name_for(m: &Mission) -> String {
    format!("{}-{}", m.id.to_lowercase(), m.updated_at)
}

/// The branch a mission attempt works on. Pure.
pub fn branch_for(m: &Mission) -> String {
    format!("agentos/{}", name_for(m))
}

/// Where a mission attempt's worktree lives. Pure.
pub fn path_for(repo_dir: &Path, m: &Mission) -> PathBuf {
    repo_dir.join(DIR).join(name_for(m))
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let exe = procs::find("git").ok_or_else(|| anyhow!("git is not installed"))?;
    let out = procs::command(&exe)
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
    }
}

/// Add `.worktrees/` to the repository's local exclude file once. Pure apart from the file.
pub fn ensure_excluded(common_git_dir: &Path) -> Result<()> {
    let info = common_git_dir.join("info");
    std::fs::create_dir_all(&info)?;
    let f = info.join("exclude");
    let text = std::fs::read_to_string(&f).unwrap_or_default();
    let line = format!("/{DIR}/");
    if text
        .lines()
        .any(|l| l.trim() == line || l.trim() == format!("{DIR}/"))
    {
        return Ok(());
    }
    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&line);
    out.push('\n');
    std::fs::write(&f, out)?;
    Ok(())
}

/// The commit a new worktree starts from: the remote default branch after a fetch, else HEAD.
fn base(repo_dir: &Path) -> String {
    let _ = git(repo_dir, &["fetch", "--quiet", "origin"]);
    git(
        repo_dir,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok()
    .filter(|s| !s.is_empty())
    .or_else(|| {
        ["origin/main", "origin/master"]
            .into_iter()
            .find(|b| git(repo_dir, &["rev-parse", "--verify", "--quiet", b]).is_ok())
            .map(String::from)
    })
    .unwrap_or_else(|| "HEAD".into())
}

/// Create the worktree for a mission attempt and remember it. Returns its folder.
pub fn create(repo_dir: &Path, m: &Mission) -> Result<PathBuf> {
    let common = git(repo_dir, &["rev-parse", "--git-common-dir"])?;
    let common = {
        let p = PathBuf::from(&common);
        if p.is_absolute() {
            p
        } else {
            repo_dir.join(p)
        }
    };
    ensure_excluded(&common)?;
    let path = path_for(repo_dir, m);
    if !path.is_dir() {
        let base = base(repo_dir);
        let p = path.display().to_string();
        git(
            repo_dir,
            &["worktree", "add", "-b", &branch_for(m), &p, &base],
        )?;
    }
    remember(&m.id, &path);
    Ok(path)
}

fn remember(id: &str, path: &Path) {
    if let Ok(c) = crate::store::open() {
        let _ = crate::store::kv_set(&c, &format!("{KV_PREFIX}{id}"), &path.display().to_string());
    }
}

/// The worktree recorded for a mission, if any.
pub fn recorded(id: &str) -> Option<PathBuf> {
    let c = crate::store::open().ok()?;
    crate::store::kv_get(&c, &format!("{KV_PREFIX}{id}"))
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
}

/// Remove a mission's worktree (the branch stays: it is the pull request). Clears the record.
pub fn remove(repo_dir: &Path, id: &str) -> Result<()> {
    if let Some(path) = recorded(id) {
        if path.is_dir() {
            let p = path.display().to_string();
            git(repo_dir, &["worktree", "remove", "--force", &p])?;
        }
        let _ = git(repo_dir, &["worktree", "prune"]);
        if let Ok(c) = crate::store::open() {
            let _ = crate::store::kv_delete(&c, &format!("{KV_PREFIX}{id}"));
        }
    }
    Ok(())
}

/// Throw a mission attempt away entirely: worktree, local branch and remote branch. For live
/// tests and abandoned attempts; a mission that produced a pull request keeps its branch.
pub fn discard(repo_dir: &Path, id: &str) -> Result<()> {
    // The attempt name is fixed when the worktree is made; `updated_at` moves on afterwards, so
    // the branch is read from the recorded folder, never recomputed from the mission.
    let Some(branch) = recorded_branch(id) else {
        return Ok(());
    };
    // A worktree folder that is briefly held (a worker still exiting) must not leak the branches:
    // record the failure and still delete the local and remote branch.
    let mut errors = Vec::new();
    if let Err(e) = remove(repo_dir, id) {
        errors.push(e.to_string());
    }
    if git(repo_dir, &["rev-parse", "--verify", "--quiet", &branch]).is_ok() {
        if let Err(e) = git(repo_dir, &["branch", "-D", &branch]) {
            errors.push(e.to_string());
        }
    }
    let remote = git(repo_dir, &["ls-remote", "--heads", "origin", &branch]).unwrap_or_default();
    if !remote.is_empty() {
        if let Err(e) = git(
            repo_dir,
            &["push", "--quiet", "origin", "--delete", &branch],
        ) {
            errors.push(e.to_string());
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("{}", errors.join("; "))
    }
}

/// Whether a branch still exists on the remote.
pub fn remote_branch_exists(repo_dir: &Path, branch: &str) -> bool {
    git(repo_dir, &["ls-remote", "--heads", "origin", branch])
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// The branch of the mission's recorded worktree (`agentos/<folder name>`).
pub fn recorded_branch(id: &str) -> Option<String> {
    let p = recorded(id)?;
    let name = p.file_name()?.to_string_lossy().to_string();
    Some(format!("agentos/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission(id: &str, at: i64) -> Mission {
        Mission {
            id: id.into(),
            repo: "r".into(),
            goal: "g".into(),
            acceptance: "a".into(),
            role: "implementer".into(),
            agent: "claude-code".into(),
            status: "queued".into(),
            provider: String::new(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: None,
            reviewer: None,
            review_verdict: None,
            pr_url: None,
            review_note: None,
            created_at: 0,
            updated_at: at,
            verify: None,
        }
    }

    #[test]
    fn each_attempt_gets_its_own_branch_and_folder() {
        let a = mission("M-007", 100);
        let b = mission("M-007", 200);
        assert_eq!(branch_for(&a), "agentos/m-007-100");
        assert_ne!(branch_for(&a), branch_for(&b), "a rework is a new branch");
        assert_eq!(
            path_for(Path::new("C:/r"), &a),
            Path::new("C:/r").join(".worktrees").join("m-007-100")
        );
    }

    #[test]
    fn the_exclude_line_is_added_once() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("info")).unwrap();
        std::fs::write(d.path().join("info/exclude"), "# git ls-files --others").unwrap();
        ensure_excluded(d.path()).unwrap();
        ensure_excluded(d.path()).unwrap();
        let t = std::fs::read_to_string(d.path().join("info/exclude")).unwrap();
        assert_eq!(t.matches("/.worktrees/").count(), 1, "{t}");
        assert!(t.starts_with("# git ls-files"));
    }

    #[test]
    fn a_real_worktree_is_created_and_removed() {
        if procs::find("git").is_none() {
            return;
        }
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let repo = d.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]).unwrap();
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "init",
            ],
        )
        .unwrap();
        let m = mission("M-001", 5);
        let wt = create(&repo, &m).unwrap();
        assert!(wt.join("a.txt").is_file());
        assert_eq!(recorded("M-001").as_deref(), Some(wt.as_path()));
        assert_eq!(recorded_branch("M-001").as_deref(), Some("agentos/m-001-5"));
        assert_eq!(
            git(&wt, &["branch", "--show-current"]).unwrap(),
            "agentos/m-001-5"
        );
        // Untracked in the main checkout: the exclude file covers it.
        assert_eq!(git(&repo, &["status", "--porcelain"]).unwrap(), "");
        remove(&repo, "M-001").unwrap();
        assert!(!wt.exists());
        assert!(recorded("M-001").is_none());
        crate::store::set_test_db(None);
    }
}
