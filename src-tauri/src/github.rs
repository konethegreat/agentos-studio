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

/// `owner/name` for a mission's repository. Missions store the Hermes project slug, which `gh -R`
/// does not accept, so a slug is resolved through the project folder's remote.
pub fn name_with_owner(repo: &str) -> Result<String> {
    if valid_repo(repo) {
        return Ok(repo.to_string());
    }
    let dir = crate::review_agent::project_dir(repo)
        .filter(|d| d.is_dir())
        .ok_or_else(|| anyhow!("repository '{repo}' is not owner/name or a known project"))?;
    let out = gh(
        &[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ],
        None,
        Some(&dir),
    )?;
    let name = out.trim().to_string();
    if !valid_repo(&name) {
        bail!("gh could not name the remote of {}", dir.display());
    }
    Ok(name)
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

/// True only when the pull request has checks and every one passed.
pub fn pr_checks_green(repo: &str, number: u64) -> Result<bool> {
    let r = repo_arg(repo)?;
    let v = match gh_json(
        &[
            "pr",
            "checks",
            &number.to_string(),
            "-R",
            r,
            "--json",
            "state",
        ],
        None,
    ) {
        Ok(v) => v,
        // No checks, or the command failed: not green.
        Err(_) => return Ok(false),
    };
    let Some(arr) = v.as_array() else {
        return Ok(false);
    };
    if arr.is_empty() {
        return Ok(false);
    }
    Ok(arr.iter().all(|c| {
        matches!(
            c.get("state").and_then(|s| s.as_str()),
            Some("SUCCESS") | Some("SKIPPED") | Some("NEUTRAL")
        )
    }))
}

/// One check on a pull request, as `gh pr checks --json name,state,link` reports it.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct CheckRun {
    pub name: String,
    pub state: String,
    pub link: String,
}

/// Where a pull request's CI stands. Pending wins over failed so a fix or rerun only starts once
/// the run has finished; no checks at all is `Missing`, which is never green.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ci {
    Green,
    Pending,
    Failed(Vec<CheckRun>),
    Missing,
}

const CHECK_OK: [&str; 3] = ["SUCCESS", "SKIPPED", "NEUTRAL"];
const CHECK_PENDING: [&str; 6] = [
    "PENDING",
    "QUEUED",
    "IN_PROGRESS",
    "WAITING",
    "REQUESTED",
    "EXPECTED",
];

/// `gh pr checks --json` output as check runs. Anything that is not a JSON array is no checks. Pure.
pub fn parse_check_runs(json: &str) -> Vec<CheckRun> {
    let Ok(Value::Array(a)) = serde_json::from_str::<Value>(json.trim()) else {
        return Vec::new();
    };
    a.iter()
        .map(|c| {
            let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            CheckRun {
                name: s("name"),
                state: s("state").to_uppercase(),
                link: s("link"),
            }
        })
        .collect()
}

/// Pure.
pub fn ci_state(runs: &[CheckRun]) -> Ci {
    if runs.is_empty() {
        return Ci::Missing;
    }
    if runs
        .iter()
        .any(|r| CHECK_PENDING.contains(&r.state.as_str()))
    {
        return Ci::Pending;
    }
    let failed: Vec<CheckRun> = runs
        .iter()
        .filter(|r| !CHECK_OK.contains(&r.state.as_str()))
        .cloned()
        .collect();
    if failed.is_empty() {
        Ci::Green
    } else {
        Ci::Failed(failed)
    }
}

/// One line per check, for the reviewer's prompt and the digest. Pure.
pub fn checks_summary(runs: &[CheckRun]) -> String {
    if runs.is_empty() {
        return "No CI checks reported on this pull request.".into();
    }
    runs.iter()
        .map(|r| format!("- {}: {}", r.name, r.state))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The checks on a pull request. `gh pr checks` exits non-zero while checks are pending or have
/// failed, with the JSON still on stdout, so the exit code is not treated as an error here.
pub fn pr_check_runs(repo: &str, number: u64) -> Result<Vec<CheckRun>> {
    let r = repo_arg(repo)?;
    if procs::find("gh").is_none() {
        bail!("The GitHub CLI (gh) is not installed. Install it from Setup and Integrations.");
    }
    let n = number.to_string();
    let (_, out, err) = procs::run_with_timeout(
        "gh",
        &["pr", "checks", &n, "-R", r, "--json", "name,state,link"],
        None,
        None,
        LIMIT,
    )?;
    if !out.trim_start().starts_with('[') {
        let msg = err.trim();
        // No checks yet is an answer, not a failure.
        if msg.contains("no checks reported") {
            return Ok(Vec::new());
        }
        bail!("gh pr checks failed: {msg}");
    }
    Ok(parse_check_runs(&out))
}

pub fn pr_merge(
    repo: &str,
    number: u64,
    method: &str,
    delete_branch: bool,
    approved: bool,
) -> Result<String> {
    pr_merge_with(repo, number, method, delete_branch, approved, None, None)
}

/// Merge with an explicit commit subject and body. The mission gate uses it to keep the pull
/// request's description and append the provenance trailers to it.
#[allow(clippy::too_many_arguments)]
pub fn pr_merge_with(
    repo: &str,
    number: u64,
    method: &str,
    delete_branch: bool,
    approved: bool,
    subject: Option<&str>,
    body: Option<&str>,
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
        let args = merge_args(r, number, flag, delete_branch, subject, body);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        gh(&refs, None, None).map(|o| format!("Merged {target}. {}", o.trim()))
    })
}

fn merge_args(
    repo: &str,
    number: u64,
    flag: &str,
    delete_branch: bool,
    subject: Option<&str>,
    body: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "pr".to_string(),
        "merge".to_string(),
        number.to_string(),
        "-R".to_string(),
        repo.to_string(),
        flag.to_string(),
    ];
    if delete_branch {
        args.push("--delete-branch".into());
    }
    if let Some(s) = subject.filter(|s| !s.trim().is_empty()) {
        args.push("--subject".into());
        args.push(s.to_string());
    }
    if let Some(b) = body.filter(|b| !b.trim().is_empty()) {
        args.push("--body".into());
        args.push(b.to_string());
    }
    args
}

/// The pull request title and description, for a merge that keeps them and appends to the body.
/// GitHub's squash subject is the pull request title followed by " (#n)". Passing only the title
/// would drop the number that the rest of main's history carries, so the subject is built here.
pub fn squash_subject(title: &str, number: u64) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    Some(format!("{title} (#{number})"))
}

/// The pull request's head branch, where a fix mission pushes.
pub fn pr_head_branch(repo: &str, number: u64) -> Result<String> {
    let r = repo_arg(repo)?;
    let n = number.to_string();
    let v = gh_json(
        &["pr", "view", n.as_str(), "-R", r, "--json", "headRefName"],
        None,
    )?;
    v.get("headRefName")
        .and_then(Value::as_str)
        .map(String::from)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("no head branch for {repo}#{number}"))
}

/// The Actions run id in a check's link (`.../actions/runs/<id>/job/<job>`). Pure.
pub fn run_id_of(link: &str) -> Option<u64> {
    let rest = link.split("/actions/runs/").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Rerun only the failed jobs of an Actions run.
pub fn rerun_failed(repo: &str, run_id: u64) -> Result<()> {
    let r = repo_arg(repo)?;
    let id = run_id.to_string();
    gh(&["run", "rerun", &id, "--failed", "-R", r], None, None).map(|_| ())
}

/// The failed steps' log of an Actions run, trimmed to its last `max` characters.
pub fn failed_log(repo: &str, run_id: u64, max: usize) -> Result<String> {
    let r = repo_arg(repo)?;
    let id = run_id.to_string();
    let out = gh(&["run", "view", &id, "--log-failed", "-R", r], None, None)?;
    let chars: Vec<char> = out.chars().collect();
    let start = chars.len().saturating_sub(max);
    Ok(chars[start..].iter().collect())
}

/// `OPEN`, `MERGED` or `CLOSED`.
pub fn pr_status(repo: &str, number: u64) -> Result<String> {
    let r = repo_arg(repo)?;
    let n = number.to_string();
    let v = gh_json(
        &["pr", "view", n.as_str(), "-R", r, "--json", "state"],
        None,
    )?;
    Ok(v.get("state")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_uppercase())
}

pub fn pr_title_and_body(repo: &str, number: u64) -> Result<(String, String)> {
    let r = repo_arg(repo)?;
    let n = number.to_string();
    let v = gh_json(
        &["pr", "view", n.as_str(), "-R", r, "--json", "title,body"],
        None,
    )?;
    Ok((
        v.get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        v.get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim_end()
            .to_string(),
    ))
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
    fn a_check_link_names_its_actions_run() {
        assert_eq!(
            run_id_of("https://github.com/o/r/actions/runs/42655657348/job/129293323580"),
            Some(42655657348)
        );
        assert_eq!(run_id_of("https://dashboard.gitguardian.com"), None);
    }

    #[test]
    fn ci_waits_for_pending_checks_and_names_the_failed_ones() {
        // Shape of `gh pr checks --json name,state,link`.
        let runs = parse_check_runs(
            r#"[{"name":"build-and-test","state":"FAILURE","link":"https://x/1"},
                {"name":"GitGuardian Security Checks","state":"SUCCESS","link":"https://x/2"}]"#,
        );
        assert_eq!(runs.len(), 2);
        match ci_state(&runs) {
            Ci::Failed(f) => assert_eq!(f[0].name, "build-and-test"),
            other => panic!("expected failed, got {other:?}"),
        }
        let mut pending = runs.clone();
        pending[1].state = "IN_PROGRESS".into();
        assert_eq!(ci_state(&pending), Ci::Pending);
        let green = parse_check_runs(
            r#"[{"name":"a","state":"success","link":""},{"name":"b","state":"SKIPPED","link":""}]"#,
        );
        assert_eq!(ci_state(&green), Ci::Green);
        assert_eq!(ci_state(&parse_check_runs("no checks")), Ci::Missing);
        assert_eq!(
            checks_summary(&runs),
            "- build-and-test: FAILURE\n- GitGuardian Security Checks: SUCCESS"
        );
    }

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
    fn a_merge_can_carry_a_subject_and_a_body() {
        let plain = merge_args("o/r", 7, "--squash", true, None, None);
        assert_eq!(
            plain,
            [
                "pr",
                "merge",
                "7",
                "-R",
                "o/r",
                "--squash",
                "--delete-branch"
            ]
        );
        let mission = merge_args(
            "o/r",
            7,
            "--squash",
            false,
            Some("Add the helper"),
            Some("The helper is tested.\n\nMission: M-1\n"),
        );
        let at = |flag: &str| mission.iter().position(|a| a == flag);
        assert_eq!(
            at("--subject").map(|i| mission[i + 1].as_str()),
            Some("Add the helper")
        );
        assert_eq!(
            at("--body").map(|i| mission[i + 1].as_str()),
            Some("The helper is tested.\n\nMission: M-1\n")
        );
        assert!(!mission.iter().any(|a| a == "--delete-branch"));
        let blank = merge_args("o/r", 7, "--squash", false, Some("  "), Some(""));
        assert!(!blank.iter().any(|a| a == "--subject" || a == "--body"));
    }

    #[test]
    fn a_squash_subject_keeps_the_pull_request_number() {
        assert_eq!(
            squash_subject("Add retryOnce", 25).as_deref(),
            Some("Add retryOnce (#25)")
        );
        assert_eq!(
            squash_subject("  Add retryOnce  ", 25).as_deref(),
            Some("Add retryOnce (#25)")
        );
        // Without a title, gh keeps its own default subject.
        assert_eq!(squash_subject("   ", 25), None);
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
