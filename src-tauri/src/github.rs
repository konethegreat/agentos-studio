//! GitHub through the `gh` CLI: auth, repos, branches, commits, pull requests (diff,
//! checks, review, merge, create), issues (create, comment, close) and Actions runs (logs,
//! rerun). Every call passes arguments as an argv array (no shell), validates repository
//! names, numbers and branch names, and sends free text through stdin. Writes go through
//! the permission guard and the audit log.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::{permissions, procs};

const LIMIT: Duration = Duration::from_secs(90);
const MAX_TEXT: usize = 400 * 1024;

pub fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let ok = |s: Option<&str>| {
        s.is_some_and(|s| {
            !s.is_empty()
                && s.len() <= 100
                && !s.starts_with('.')
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

pub fn valid_branch(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 200
        && !b.starts_with('-')
        && !b.contains("..")
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

fn repo_arg(repo: &str) -> Result<&str> {
    if valid_repo(repo) {
        Ok(repo)
    } else {
        bail!("Invalid repository name {repo:?} (expected owner/name)")
    }
}

fn gh(args: &[&str], stdin: Option<&str>, cwd: Option<&Path>) -> Result<String> {
    if procs::find("gh").is_none() {
        bail!("The GitHub CLI (gh) is not installed. Install it from Setup and Integrations.");
    }
    let (code, out, err) = procs::run_with_timeout("gh", args, stdin, cwd, LIMIT)?;
    if code != Some(0) {
        let msg = err.trim();
        let msg = if msg.is_empty() { out.trim() } else { msg };
        if msg.contains("gh auth login") || msg.contains("not logged") {
            bail!("GitHub is not signed in. Use Sign in on the GitHub view (gh auth login). {msg}");
        }
        bail!("gh {} failed: {msg}", args.first().copied().unwrap_or(""));
    }
    Ok(out)
}

fn gh_json(args: &[&str], cwd: Option<&Path>) -> Result<Value> {
    let out = gh(args, None, cwd)?;
    if out.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&out).map_err(|e| anyhow!("gh returned unexpected output: {e}"))
}

fn clip(mut s: String, tail: bool) -> String {
    if s.len() <= MAX_TEXT {
        return s;
    }
    if tail {
        let mut cut = s.len() - MAX_TEXT;
        while !s.is_char_boundary(cut) {
            cut += 1;
        }
        format!("(earlier output trimmed)\n{}", &s[cut..])
    } else {
        let mut cut = MAX_TEXT;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n(output trimmed)");
        s
    }
}

// ------------------------------------------------------------------ reads

pub fn auth_status() -> Result<Value> {
    if procs::find("gh").is_none() {
        return Ok(json!({ "installed": false, "logged_in": false }));
    }
    let (_, out, _) = procs::run_with_timeout(
        "gh",
        &["auth", "status", "--json", "hosts"],
        None,
        None,
        LIMIT,
    )?;
    let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
    let account = v
        .pointer("/hosts/github.com")
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .find(|x| x.get("active").and_then(Value::as_bool) == Some(true))
                .or(a.first())
        })
        .cloned();
    Ok(match account {
        Some(a) if a.get("state").and_then(Value::as_str) == Some("success") => json!({
            "installed": true,
            "logged_in": true,
            "login": a.get("login"),
            "scopes": a.get("scopes"),
            "host": a.get("host"),
            "protocol": a.get("gitProtocol"),
        }),
        _ => json!({ "installed": true, "logged_in": false }),
    })
}

pub fn login() -> Result<String> {
    procs::spawn_console("gh", &["auth", "login", "--web", "--git-protocol", "https"])?;
    Ok("A sign-in window opened. Finish it in your browser, then press Refresh.".into())
}

pub fn repos(limit: u32) -> Result<Value> {
    let l = limit.clamp(1, 300).to_string();
    gh_json(
        &[
            "repo", "list", "--limit", &l, "--json",
            "nameWithOwner,description,isPrivate,isArchived,updatedAt,url,defaultBranchRef,stargazerCount,primaryLanguage",
        ],
        None,
    )
}

/// The GitHub repository a local directory belongs to, if any.
pub fn repo_for_dir(dir: &str) -> Result<Value> {
    let d = Path::new(dir);
    if !d.is_dir() {
        bail!("{dir} is not a directory");
    }
    gh_json(
        &[
            "repo",
            "view",
            "--json",
            "nameWithOwner,url,defaultBranchRef,isPrivate",
        ],
        Some(d),
    )
}

pub fn branches(repo: &str) -> Result<Value> {
    let r = repo_arg(repo)?;
    gh_json(&["api", &format!("repos/{r}/branches?per_page=100")], None)
}

pub fn commits(repo: &str, branch: Option<&str>) -> Result<Value> {
    let r = repo_arg(repo)?;
    let mut path = format!("repos/{r}/commits?per_page=40");
    if let Some(b) = branch.filter(|b| !b.is_empty()) {
        if !valid_branch(b) {
            bail!("Invalid branch name");
        }
        path.push_str(&format!("&sha={b}"));
    }
    gh_json(&["api", &path], None)
}

fn pr_state(state: &str) -> Result<&str> {
    match state {
        "open" | "closed" | "merged" | "all" => Ok(state),
        _ => bail!("State must be open, closed, merged or all"),
    }
}

pub fn prs(repo: &str, state: &str) -> Result<Value> {
    let r = repo_arg(repo)?;
    gh_json(
        &[
            "pr", "list", "-R", r, "--state", pr_state(state)?, "--limit", "60", "--json",
            "number,title,author,headRefName,baseRefName,isDraft,reviewDecision,updatedAt,url,additions,deletions,state,labels,statusCheckRollup",
        ],
        None,
    )
}

pub fn pr_detail(repo: &str, number: u64) -> Result<Value> {
    let r = repo_arg(repo)?;
    gh_json(
        &[
            "pr", "view", &number.to_string(), "-R", r, "--json",
            "number,title,body,author,state,isDraft,headRefName,baseRefName,mergeable,mergeStateStatus,reviewDecision,reviews,comments,files,commits,statusCheckRollup,url,additions,deletions,createdAt,labels",
        ],
        None,
    )
}

pub fn pr_diff(repo: &str, number: u64) -> Result<String> {
    let r = repo_arg(repo)?;
    Ok(clip(
        gh(
            &[
                "pr",
                "diff",
                &number.to_string(),
                "-R",
                r,
                "--color",
                "never",
            ],
            None,
            None,
        )?,
        false,
    ))
}

pub fn issues(repo: &str, state: &str) -> Result<Value> {
    let r = repo_arg(repo)?;
    let s = match state {
        "open" | "closed" | "all" => state,
        _ => bail!("State must be open, closed or all"),
    };
    gh_json(
        &[
            "issue",
            "list",
            "-R",
            r,
            "--state",
            s,
            "--limit",
            "60",
            "--json",
            "number,title,author,labels,state,updatedAt,url,comments,assignees",
        ],
        None,
    )
}

pub fn issue_detail(repo: &str, number: u64) -> Result<Value> {
    let r = repo_arg(repo)?;
    gh_json(
        &[
            "issue",
            "view",
            &number.to_string(),
            "-R",
            r,
            "--json",
            "number,title,body,author,state,labels,comments,url,createdAt,assignees",
        ],
        None,
    )
}

pub fn runs(repo: &str, branch: Option<&str>) -> Result<Value> {
    let r = repo_arg(repo)?;
    let mut args = vec![
        "run", "list", "-R", r, "--limit", "40", "--json",
        "databaseId,number,displayTitle,status,conclusion,workflowName,headBranch,event,createdAt,updatedAt,url",
    ];
    if let Some(b) = branch.filter(|b| !b.is_empty()) {
        if !valid_branch(b) {
            bail!("Invalid branch name");
        }
        args.push("--branch");
        args.push(b);
    }
    gh_json(&args, None)
}

pub fn run_detail(repo: &str, id: u64) -> Result<Value> {
    let r = repo_arg(repo)?;
    gh_json(
        &[
            "run",
            "view",
            &id.to_string(),
            "-R",
            r,
            "--json",
            "databaseId,displayTitle,status,conclusion,workflowName,headBranch,url,jobs,createdAt",
        ],
        None,
    )
}

pub fn run_failed_log(repo: &str, id: u64) -> Result<String> {
    let r = repo_arg(repo)?;
    let out = gh(
        &["run", "view", &id.to_string(), "-R", r, "--log-failed"],
        None,
        None,
    )?;
    let cleaned: String = out
        .lines()
        .map(procs::clean_line)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(clip(cleaned, true))
}

// ------------------------------------------------------------------ writes

fn require_text(label: &str, s: &str, max: usize) -> Result<String> {
    let t = s.trim();
    if t.is_empty() {
        bail!("{label} cannot be empty");
    }
    if t.len() > max {
        bail!("{label} is too long");
    }
    Ok(t.to_string())
}

pub fn pr_merge(
    repo: &str,
    number: u64,
    method: &str,
    delete_branch: bool,
    approved: bool,
) -> Result<String> {
    let r = repo_arg(repo)?;
    let flag = match method {
        "squash" => "--squash",
        "merge" => "--merge",
        "rebase" => "--rebase",
        _ => bail!("Method must be squash, merge or rebase"),
    };
    let target = format!("{r}#{number}");
    let summary = format!(
        "{method} merge pull request {target}{}",
        if delete_branch {
            " and delete its branch"
        } else {
            ""
        }
    );
    permissions::run_guarded("github.pr.merge", &target, &summary, approved, || {
        let n = number.to_string();
        let mut args = vec!["pr", "merge", n.as_str(), "-R", r, flag];
        if delete_branch {
            args.push("--delete-branch");
        }
        gh(&args, None, None).map(|o| format!("Merged {target}. {}", o.trim()))
    })
}

pub fn pr_review(
    repo: &str,
    number: u64,
    event: &str,
    body: &str,
    approved: bool,
) -> Result<String> {
    let r = repo_arg(repo)?;
    let flag = match event {
        "approve" => "--approve",
        "request_changes" => "--request-changes",
        "comment" => "--comment",
        _ => bail!("Review must be approve, request_changes or comment"),
    };
    let body = if event == "approve" && body.trim().is_empty() {
        String::new()
    } else {
        require_text("Review text", body, 60_000)?
    };
    let target = format!("{r}#{number}");
    let summary = format!("Submit a {} review on {target}", event.replace('_', " "));
    permissions::run_guarded("github.pr.review", &target, &summary, approved, || {
        let n = number.to_string();
        let mut args = vec!["pr", "review", n.as_str(), "-R", r, flag];
        if !body.is_empty() {
            args.extend(["--body-file", "-"]);
        }
        gh(&args, (!body.is_empty()).then_some(body.as_str()), None)
            .map(|_| format!("Review submitted on {target}"))
    })
}

#[allow(clippy::too_many_arguments)]
pub fn pr_create(
    dir: &str,
    title: &str,
    body: &str,
    base: Option<&str>,
    head: Option<&str>,
    draft: bool,
    approved: bool,
) -> Result<String> {
    let d = Path::new(dir);
    if !d.is_dir() {
        bail!("{dir} is not a directory");
    }
    let title = require_text("Title", title, 256)?;
    for b in [base, head].into_iter().flatten().filter(|b| !b.is_empty()) {
        if !valid_branch(b) {
            bail!("Invalid branch name {b}");
        }
    }
    let summary = format!("Open a pull request \"{title}\" from {}", d.display());
    permissions::run_guarded("github.pr.create", dir, &summary, approved, || {
        let title_arg = format!("--title={title}");
        let base_arg = base
            .filter(|b| !b.is_empty())
            .map(|b| format!("--base={b}"));
        let head_arg = head
            .filter(|b| !b.is_empty())
            .map(|b| format!("--head={b}"));
        let mut args = vec!["pr", "create", title_arg.as_str(), "--body-file", "-"];
        if let Some(a) = &base_arg {
            args.push(a);
        }
        if let Some(a) = &head_arg {
            args.push(a);
        }
        if draft {
            args.push("--draft");
        }
        gh(&args, Some(body), Some(d)).map(|o| o.trim().to_string())
    })
}

pub fn issue_create(repo: &str, title: &str, body: &str, approved: bool) -> Result<String> {
    let r = repo_arg(repo)?;
    let title = require_text("Title", title, 256)?;
    let summary = format!("Open issue \"{title}\" in {r}");
    permissions::run_guarded("github.issue.create", r, &summary, approved, || {
        let t = format!("--title={title}");
        gh(
            &["issue", "create", "-R", r, t.as_str(), "--body-file", "-"],
            Some(body),
            None,
        )
        .map(|o| o.trim().to_string())
    })
}

/// Comment on an issue (`kind = "issue"`) or a pull request (`kind = "pr"`).
pub fn comment(repo: &str, kind: &str, number: u64, body: &str, approved: bool) -> Result<String> {
    let r = repo_arg(repo)?;
    let sub = match kind {
        "issue" => "issue",
        "pr" => "pr",
        _ => bail!("Kind must be issue or pr"),
    };
    let body = require_text("Comment", body, 60_000)?;
    let target = format!("{r}#{number}");
    permissions::run_guarded(
        "github.issue.comment",
        &target,
        &format!("Comment on {target}"),
        approved,
        || {
            let n = number.to_string();
            gh(
                &[sub, "comment", n.as_str(), "-R", r, "--body-file", "-"],
                Some(&body),
                None,
            )
            .map(|o| o.trim().to_string())
        },
    )
}

pub fn issue_close(repo: &str, number: u64, reason: &str, approved: bool) -> Result<String> {
    let r = repo_arg(repo)?;
    let reason = match reason {
        "completed" | "not planned" => reason,
        _ => bail!("Reason must be completed or not planned"),
    };
    let target = format!("{r}#{number}");
    permissions::run_guarded(
        "github.issue.close",
        &target,
        &format!("Close issue {target} as {reason}"),
        approved,
        || {
            let n = number.to_string();
            gh(
                &["issue", "close", n.as_str(), "-R", r, "--reason", reason],
                None,
                None,
            )
            .map(|_| format!("Closed {target}"))
        },
    )
}

pub fn run_rerun(repo: &str, id: u64, failed_only: bool, approved: bool) -> Result<String> {
    let r = repo_arg(repo)?;
    let target = format!("{r} run {id}");
    let summary = format!(
        "Re-run {}workflow run {id} in {r}",
        if failed_only {
            "the failed jobs of "
        } else {
            ""
        }
    );
    permissions::run_guarded("github.run.rerun", &target, &summary, approved, || {
        let n = id.to_string();
        let mut args = vec!["run", "rerun", n.as_str(), "-R", r];
        if failed_only {
            args.push("--failed");
        }
        gh(&args, None, None).map(|_| format!("Re-run requested for {target}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_inputs() {
        assert!(valid_repo("konethegreat/agentos-studio"));
        assert!(valid_repo("a.b/c_d-e"));
        assert!(!valid_repo("no-slash"));
        assert!(!valid_repo("a/b/c"));
        assert!(!valid_repo("a/b;rm"));
        assert!(!valid_repo("../etc"));
        assert!(!valid_repo("a/../b"));
        assert!(valid_branch("feat/v0.3.0-control-plane"));
        assert!(!valid_branch("--force"));
        assert!(!valid_branch("a b"));
        assert!(!valid_branch("x&per_page=1"));
        assert!(!valid_branch("a..b"));
    }

    #[test]
    fn rejects_bad_write_arguments_before_running_gh() {
        assert!(pr_merge("bad", 1, "squash", true, true).is_err());
        assert!(pr_merge("o/r", 1, "octopus", true, true).is_err());
        assert!(pr_review("o/r", 1, "comment", "   ", true).is_err());
        assert!(issue_close("o/r", 1, "because", true).is_err());
        assert!(comment("o/r", "commit", 1, "hi", true).is_err());
        assert!(issues("o/r", "weird").is_err());
        assert!(prs("o/r", "weird").is_err());
    }

    #[test]
    fn clips_on_char_boundaries() {
        let s = "\u{e9}".repeat(MAX_TEXT);
        let head = clip(s.clone(), false);
        assert!(head.ends_with("(output trimmed)"));
        let tail = clip(s, true);
        assert!(tail.starts_with("(earlier output trimmed)"));
    }
}
