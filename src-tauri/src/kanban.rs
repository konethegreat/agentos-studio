//! Hermes missions run on Hermes' own kanban board.
//!
//! Hermes owns execution: `hermes kanban create` puts the mission on the `agentos-studio` board,
//! the dispatcher inside the Hermes gateway claims it, runs a worker in the repository and keeps
//! going while Studio is closed (`hermes kanban daemon` is deprecated; the gateway dispatches).
//! When the worker asks for review, kanban runs its own review pass and marks the task done.
//!
//! Studio owns the cross-agent policy: kanban `done` only means "the work is finished". The
//! mission still goes to Studio's reviewer (a different agent) and through `merge_blockers`; that
//! gate stays the only route to a merge. Claude Code and Antigravity missions keep the Agent Bus
//! path in `missions::start`.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::missions::Mission;
use crate::procs;

pub const BOARD: &str = "agentos-studio";
/// A mission's task id is stored as `kanban:<id>` so the scheduler knows which board to read.
pub const PREFIX: &str = "kanban:";
/// Per-task runtime cap for Hermes missions, seconds, in `studio.db` kv. Empty means no cap.
pub const KV_MAX_RUNTIME: &str = "hermes.mission_max_runtime";
/// How many Hermes missions may run at once in one repository (1 to 4). Above 1, each task gets
/// its own git worktree (`<repo>/.worktrees/<task>`, branch `<slug>/<task>-...`).
pub const KV_PARALLEL: &str = "missions.parallel_per_repo";

pub fn parallel_per_repo() -> usize {
    crate::store::open()
        .ok()
        .and_then(|c| crate::store::kv_get(&c, KV_PARALLEL).ok().flatten())
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 4)
}

pub fn set_parallel_per_repo(n: usize) -> Result<()> {
    let c = crate::store::open()?;
    crate::store::kv_set(&c, KV_PARALLEL, &n.clamp(1, 4).to_string())
}

pub fn is_kanban(task_id: Option<&str>) -> bool {
    task_id.is_some_and(|t| t.starts_with(PREFIX))
}

fn strip(task_id: &str) -> &str {
    task_id.strip_prefix(PREFIX).unwrap_or(task_id)
}

fn hermes(args: &[String]) -> Result<String> {
    let exe = procs::find("hermes").ok_or_else(|| anyhow!("hermes is not installed"))?;
    let out = procs::command(&exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .context("running hermes kanban")?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let msg = stderr
        .lines()
        .chain(stdout.lines())
        .rev()
        .find(|l| !l.starts_with("Skipping unreadable plugin") && !l.trim().is_empty())
        .unwrap_or("hermes kanban failed")
        .to_string();
    bail!("{msg}")
}

fn board_args(rest: &[&str]) -> Vec<String> {
    let mut a: Vec<String> = vec!["kanban".into(), "--board".into(), BOARD.into()];
    a.extend(rest.iter().map(|s| s.to_string()));
    a
}

pub fn max_runtime() -> Option<u64> {
    let c = crate::store::open().ok()?;
    crate::store::kv_get(&c, KV_MAX_RUNTIME)
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
}

pub fn set_max_runtime(seconds: Option<u64>) -> Result<()> {
    let c = crate::store::open()?;
    crate::store::kv_set(
        &c,
        KV_MAX_RUNTIME,
        &seconds
            .filter(|s| *s > 0)
            .map(|s| s.to_string())
            .unwrap_or_default(),
    )
}

/// The worker's brief. It carries the acceptance criteria and, on rework, the reviewer's note.
pub fn task_body(m: &Mission) -> String {
    let mut b = format!(
        "{}\n\nAcceptance criteria:\n{}\n\nWork in a branch, open a pull request with gh, and put the pull request URL in your summary when you request review. Do not merge: AgentOS Studio reviews and merges.",
        m.goal.trim(),
        m.acceptance.trim()
    );
    b.push_str(crate::constitution_gate::WORKER_RULES);
    if m.review_verdict.as_deref() == Some("reject") {
        if let Some(note) = m.review_note.as_deref().filter(|n| !n.trim().is_empty()) {
            b.push_str(&format!(
                "\n\nThis is a rework. The previous attempt was rejected by the reviewer:\n{}",
                note.trim()
            ));
        }
    }
    b
}

/// Arguments for `hermes kanban create`. Pure, so it is unit tested.
pub fn create_args(
    m: &Mission,
    repo_dir: &Path,
    body_file: &Path,
    assignee: &str,
    max_runtime: Option<u64>,
    worktree: bool,
) -> Vec<String> {
    let title: String = m.goal.trim().chars().take(90).collect();
    // A worktree is anchored under the Hermes project's primary repository with its own branch,
    // so several missions can work in one repository without sharing a checkout.
    let workspace = if worktree {
        "worktree".to_string()
    } else {
        format!("dir:{}", repo_dir.display())
    };
    let mut a = board_args(&[
        "create",
        &title,
        "--body-file",
        &body_file.display().to_string(),
        "--assignee",
        assignee,
        "--workspace",
        &workspace,
        "--created-by",
        "agentos-studio",
        // One task per attempt: a rework after a rejection gets a new key.
        "--idempotency-key",
        &format!("{}-{}", m.id, m.updated_at),
        "--json",
    ]);
    // A pinned model must be set at creation: the dispatcher claims a ready task within seconds,
    // before a later `set-model` would apply.
    if !m.model.trim().is_empty() {
        a.extend(["--model".into(), m.model.trim().into()]);
        if !m.provider.trim().is_empty() {
            a.extend(["--provider".into(), m.provider.trim().into()]);
        }
    }
    if let Some(s) = max_runtime {
        a.extend(["--max-runtime".into(), s.to_string()]);
    }
    if worktree {
        a.extend(["--project".into(), m.repo.trim().into()]);
    }
    a
}

/// Put a Hermes mission on the board. Returns the `kanban:<id>` task id.
pub fn create_task(m: &Mission) -> Result<String> {
    let repo_dir = crate::review_agent::project_dir(&m.repo)
        .filter(|d| d.is_dir())
        .ok_or_else(|| {
            anyhow!(
                "No folder found for repository {}. Add it as a Hermes project (Projects) so the worker has a workspace.",
                m.repo
            )
        })?;
    let _ = hermes(&[
        "kanban".into(),
        "boards".into(),
        "create".into(),
        BOARD.into(),
    ]);
    let dir = crate::store::studio_dir().join("kanban");
    std::fs::create_dir_all(&dir)?;
    let body_file = dir.join(format!("{}.md", m.id));
    std::fs::write(&body_file, task_body(m))?;
    let assignee =
        crate::hermes_surfaces::profile_for_role("implementer").unwrap_or_else(|| "default".into());
    let out = hermes(&create_args(
        m,
        &repo_dir,
        &body_file,
        &assignee,
        max_runtime(),
        parallel_per_repo() > 1,
    ))?;
    let v: Value = serde_json::from_str(out.trim())
        .map_err(|e| anyhow!("hermes kanban create did not return JSON: {e}"))?;
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("hermes kanban create returned no id"))?;
    Ok(format!("{PREFIX}{id}"))
}

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
pub struct Run {
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
pub struct TaskView {
    pub status: String,
    /// The folder the worker used: the repository, or its worktree.
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub runs: Vec<Run>,
    /// Newest sign of life in epoch seconds: task creation or start, any run start or end, any
    /// event (heartbeats included). `show --json` carries no `last_heartbeat_at`.
    #[serde(default)]
    pub last_activity: Option<i64>,
}

fn epoch(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_f64).map(|f| f as i64)
}

/// Parse `hermes kanban show --json` (`{task, runs, ...}`).
pub fn parse_show(json: &str) -> Result<TaskView> {
    let v: Value = serde_json::from_str(json.trim()).context("kanban show output")?;
    let task = v
        .get("task")
        .ok_or_else(|| anyhow!("no task in kanban show"))?;
    Ok(TaskView {
        status: task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        result: task.get("result").and_then(Value::as_str).map(String::from),
        workspace_path: task
            .get("workspace_path")
            .and_then(Value::as_str)
            .map(String::from),
        runs: v
            .get("runs")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default(),
        last_activity: {
            let mut times = vec![epoch(task.get("created_at")), epoch(task.get("started_at"))];
            for key in ["runs", "events", "comments"] {
                for item in v.get(key).and_then(Value::as_array).into_iter().flatten() {
                    times.push(epoch(item.get("started_at")));
                    times.push(epoch(item.get("ended_at")));
                    times.push(epoch(item.get("created_at")));
                }
            }
            times.into_iter().flatten().max()
        },
    })
}

/// Release a wedged worker's claim so the dispatcher runs the task again (one stuck retry).
pub fn reclaim(task_id: &str, reason: &str) -> Result<()> {
    hermes(&board_args(&[
        "reclaim",
        strip(task_id),
        "--reason",
        reason,
    ]))
    .map(|_| ())
}

/// Stop a task for good until a person releases it: block it (a blocked task is never dispatched)
/// and then release the worker's claim. Used by the global budget's hard stop.
pub fn halt(task_id: &str, reason: &str) -> Result<()> {
    let block = || {
        hermes(&board_args(&[
            "block",
            strip(task_id),
            "--kind",
            "needs_input",
            reason,
        ]))
    };
    if block().is_ok() {
        let _ = reclaim(task_id, reason);
        return Ok(());
    }
    // Some states refuse a block while a worker holds the claim: release it, then block the
    // task while it is ready, before the dispatcher's next pass.
    reclaim(task_id, reason)?;
    block().map(|_| ())
}

/// One dispatcher pass on the board, for a ready task nobody has picked up.
/// Close a kanban task Studio no longer follows (its mission was handed to another agent).
pub fn complete(task_id: &str, summary: &str) -> Result<()> {
    let id = strip(task_id).to_string();
    hermes(&board_args(&[
        "complete", &id, "--force", "--result", summary,
    ]))?;
    Ok(())
}

pub fn dispatch_pass() -> Result<()> {
    hermes(&board_args(&["dispatch"])).map(|_| ())
}

pub fn show(task_id: &str) -> Result<TaskView> {
    parse_show(&hermes(&board_args(&["show", strip(task_id), "--json"]))?)
}

/// What the scheduler should do with a running Hermes mission. Pure.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// Still working (or kanban is running its own review pass).
    Wait,
    /// The work is finished: send it to Studio's reviewer. Carries every run summary.
    Review(String),
    /// A human is needed.
    Block(String),
}

pub fn next(view: &TaskView) -> Next {
    let summaries = || {
        view.runs
            .iter()
            .filter_map(|r| r.summary.clone())
            .chain(view.result.clone())
            .collect::<Vec<_>>()
            .join("\n")
    };
    match view.status.as_str() {
        "done" => Next::Review(summaries()),
        "blocked" | "archived" => Next::Block(
            view.runs
                .iter()
                .rev()
                .find_map(|r| r.error.clone().or_else(|| r.summary.clone()))
                .unwrap_or_else(|| format!("kanban task is {}", view.status)),
        ),
        _ => Next::Wait,
    }
}

/// Every Hermes session the task's runs used (workers and kanban's own reviewer).
pub fn session_ids(view: &TaskView) -> Vec<String> {
    view.runs
        .iter()
        .filter_map(|r| r.metadata.as_ref())
        .filter_map(|m| m.get("worker_session_id").and_then(Value::as_str))
        .map(String::from)
        .collect()
}

/// `hermes verify --json --skip-start` in the repository: build and test, no server start.
pub fn verify(repo_dir: &Path) -> String {
    let args: Vec<String> = vec![
        "verify".into(),
        "--json".into(),
        "--skip-start".into(),
        repo_dir.display().to_string(),
    ];
    match procs::find("hermes").map(|exe| {
        procs::command(&exe)
            .args(&args)
            .current_dir(repo_dir)
            .stdin(std::process::Stdio::null())
            .output()
    }) {
        // verify exits non-zero when a phase fails; its JSON on stdout is the result either way.
        Some(Ok(out)) => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if serde_json::from_str::<Value>(&text).is_ok() {
                text
            } else {
                serde_json::json!({"ok": false, "error": "unreadable verify output"}).to_string()
            }
        }
        Some(Err(e)) => serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
        None => serde_json::json!({"ok": false, "error": "hermes is not installed"}).to_string(),
    }
}

/// Where a mission's work is verified before review. Never the owner's checkout: it holds
/// whatever branch the owner has (usually `main` before the merge), so a verify there tests the
/// wrong tree and leaves build output in the owner's folder (#117).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyAt {
    /// The task's own worktree, still on disk.
    Workspace(PathBuf),
    /// The worktree is gone: a fresh detached worktree of the pull request's head.
    PullHead(u64),
    /// Nothing safe to verify; recorded as not run, with the reason.
    NotRun(String),
}

/// Pick where to verify. `is_dir` is injected so the choice is unit tested. Pure.
pub fn verify_at(
    workspace: Option<&str>,
    is_dir: impl Fn(&Path) -> bool,
    pr: Option<u64>,
) -> VerifyAt {
    if let Some(ws) = workspace.map(str::trim).filter(|w| !w.is_empty()) {
        let p = PathBuf::from(ws);
        if is_dir(&p) {
            return VerifyAt::Workspace(p);
        }
    }
    match pr {
        Some(n) => VerifyAt::PullHead(n),
        None => VerifyAt::NotRun(
            "the task's worktree is gone and the mission has no pull request".into(),
        ),
    }
}

/// The recorded result when verify did not run. The gate treats it like a missing result (CI
/// still has to pass) and the reviewer is told why. Pure.
pub fn verify_not_run(reason: &str) -> String {
    serde_json::json!({"ok": false, "error": "not-run", "reason": reason}).to_string()
}

/// The pull request number at the end of a GitHub PR URL. Pure.
pub fn pr_number_in(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

/// Verify where `verify_at` says: the worktree, the pull request's head in a worktree under
/// Studio's data folder (removed afterwards), or record why it did not run.
pub fn verify_mission(
    mission_id: &str,
    repo: &str,
    workspace: Option<&str>,
    pr: Option<u64>,
) -> String {
    match verify_at(workspace, |p| p.is_dir(), pr) {
        VerifyAt::Workspace(dir) => verify(&dir),
        VerifyAt::NotRun(reason) => verify_not_run(&reason),
        VerifyAt::PullHead(n) => match pull_head_worktree(mission_id, repo, n) {
            Ok((dir, wt)) => {
                let out = verify(&wt);
                remove_verify_worktree(&dir, &wt);
                out
            }
            Err(e) => verify_not_run(&format!("could not check out pull request #{n}: {e:#}")),
        },
    }
}

fn pull_head_worktree(mission_id: &str, repo: &str, pr: u64) -> Result<(PathBuf, PathBuf)> {
    let dir = crate::review_agent::project_dir(repo)
        .filter(|d| d.is_dir())
        .ok_or_else(|| anyhow!("repository '{repo}' is not a known project with a folder"))?;
    let wt = crate::store::studio_dir()
        .join("verify")
        .join(format!("mission-{}", crate::vault::slug(mission_id, 40)));
    let wts = wt.display().to_string();
    let git = |args: &[&str]| {
        procs::run_with_timeout("git", args, None, Some(&dir), Duration::from_secs(120))
    };
    remove_verify_worktree(&dir, &wt);
    let refspec = format!("pull/{pr}/head");
    let (code, _, err) = git(&["fetch", "-q", "origin", &refspec])?;
    if code != Some(0) {
        bail!("git fetch {refspec} failed: {}", err.trim());
    }
    let (code, sha, err) = git(&["rev-parse", "FETCH_HEAD"])?;
    let sha = sha.trim().to_string();
    if code != Some(0) || sha.is_empty() {
        bail!("no head for pull request #{pr}: {}", err.trim());
    }
    if let Some(parent) = wt.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (code, _, err) = git(&["worktree", "add", "-q", "--detach", &wts, &sha])?;
    if code != Some(0) {
        bail!("git worktree add failed: {}", err.trim());
    }
    Ok((dir, wt))
}

fn remove_verify_worktree(dir: &Path, wt: &Path) {
    let wts = wt.display().to_string();
    let _ = procs::run_with_timeout(
        "git",
        &["worktree", "remove", "--force", &wts],
        None,
        Some(dir),
        Duration::from_secs(60),
    );
    let _ = std::fs::remove_dir_all(wt);
    let _ = procs::run_with_timeout(
        "git",
        &["worktree", "prune"],
        None,
        Some(dir),
        Duration::from_secs(60),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_never_falls_back_to_the_owners_checkout() {
        let all = |_: &Path| true;
        let none = |_: &Path| false;
        assert_eq!(
            verify_at(Some("C:/wt/t_1"), all, Some(7)),
            VerifyAt::Workspace(PathBuf::from("C:/wt/t_1"))
        );
        // Worktree gone: the pull request's head, never the project folder.
        assert_eq!(
            verify_at(Some("C:/wt/t_1"), none, Some(7)),
            VerifyAt::PullHead(7)
        );
        assert_eq!(verify_at(None, all, Some(7)), VerifyAt::PullHead(7));
        assert_eq!(verify_at(Some("  "), all, Some(7)), VerifyAt::PullHead(7));
        assert!(matches!(verify_at(None, none, None), VerifyAt::NotRun(_)));
        assert!(matches!(
            verify_at(Some("C:/gone"), none, None),
            VerifyAt::NotRun(_)
        ));
    }

    /// Live (#117): with the task worktree gone, verify the pull request's head of the sandbox
    /// repository in Studio's own worktree, leave the owner's checkout untouched, and clean up.
    /// `cargo test --lib -- --ignored verify_pull_head_live --nocapture`
    #[test]
    #[ignore]
    fn verify_pull_head_live() {
        let repo = "agentos-sandbox-1";
        let dir = crate::review_agent::project_dir(repo).expect("sandbox project");
        let status = |d: &Path| {
            procs::run_with_timeout(
                "git",
                &["status", "--porcelain"],
                None,
                Some(d),
                Duration::from_secs(30),
            )
            .unwrap()
            .1
        };
        let before = status(&dir);
        let out = verify_mission("LIVE-117", repo, Some("C:/does/not/exist"), Some(16));
        println!("{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_ne!(
            v.get("error").and_then(Value::as_str),
            Some("not-run"),
            "{out}"
        );
        assert_eq!(status(&dir), before, "the owner's checkout changed");
        let name = format!("mission-{}", crate::vault::slug("LIVE-117", 40));
        let wt = crate::store::studio_dir().join("verify").join(&name);
        assert!(!wt.exists(), "verify worktree left behind");
        let (_, list, _) = procs::run_with_timeout(
            "git",
            &["worktree", "list"],
            None,
            Some(&dir),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(!list.contains(&name), "{list}");
    }

    #[test]
    fn a_verify_that_did_not_run_is_missing_not_failed() {
        let j = verify_not_run("the task's worktree is gone");
        assert_eq!(crate::missions::verify_failure(&j), None);
        assert_eq!(
            crate::review_agent::verify_line(&j),
            "not run: the task's worktree is gone"
        );
        assert_eq!(pr_number_in("https://github.com/o/r/pull/42"), Some(42));
        assert_eq!(pr_number_in("https://github.com/o/r/pull/42/"), Some(42));
        assert_eq!(pr_number_in("https://github.com/o/r"), None);
    }

    fn mission() -> Mission {
        Mission {
            id: "M-7".into(),
            repo: "demo".into(),
            goal: "Add a health endpoint".into(),
            acceptance: "GET /health returns 200".into(),
            role: "implementer".into(),
            agent: "hermes".into(),
            status: "queued".into(),
            provider: "openrouter".into(),
            model: "deepseek/deepseek-v4.1-flash".into(),
            budget_usd: 1.0,
            spent_usd: 0.0,
            task_id: None,
            reviewer: None,
            review_verdict: None,
            review_note: None,
            pr_url: None,
            created_at: 1,
            updated_at: 42,
            verify: None,
        }
    }

    #[test]
    fn create_pins_model_at_creation_and_keys_each_attempt() {
        let a = create_args(
            &mission(),
            Path::new("C:/r"),
            Path::new("C:/b.md"),
            "default",
            Some(1800),
            false,
        )
        .join(" ");
        assert!(a.starts_with("kanban --board agentos-studio create Add a health endpoint"));
        assert!(a.contains("--workspace dir:C:/r"));
        assert!(a.contains("--idempotency-key M-7-42"));
        assert!(a.contains("--model deepseek/deepseek-v4.1-flash --provider openrouter"));
        assert!(a.contains("--max-runtime 1800"));
        let mut unpinned = mission();
        unpinned.model.clear();
        let b = create_args(
            &unpinned,
            Path::new("C:/r"),
            Path::new("C:/b.md"),
            "x",
            None,
            false,
        )
        .join(" ");
        let w = create_args(
            &mission(),
            Path::new("C:/r"),
            Path::new("C:/b.md"),
            "x",
            None,
            true,
        )
        .join(" ");
        assert!(
            w.contains("--workspace worktree") && w.contains("--project demo"),
            "{w}"
        );
        assert!(!w.contains("dir:"));
        assert!(
            !b.contains("--model") && !b.contains("--provider") && !b.contains("--max-runtime")
        );
    }

    #[test]
    fn rework_carries_the_reviewer_note() {
        let mut m = mission();
        assert!(!task_body(&m).contains("rework"));
        m.review_verdict = Some("reject".into());
        m.review_note = Some("Missing test for 404".into());
        assert!(task_body(&m).contains("Missing test for 404"));
    }

    #[test]
    fn kanban_done_means_studio_review_never_approval() {
        let shown = r#"{"task":{"status":"done","result":null},"runs":[
            {"profile":"default","status":"review","outcome":"review_requested","summary":"Opened https://github.com/o/r/pull/9","metadata":{"worker_session_id":"s1"}},
            {"profile":"default","status":"done","outcome":"completed","summary":"Reviewed and approved","metadata":{"worker_session_id":"s2"}}]}"#;
        let v = parse_show(shown).unwrap();
        match next(&v) {
            Next::Review(s) => assert!(s.contains("/pull/9")),
            other => panic!("expected review, got {other:?}"),
        }
        assert_eq!(session_ids(&v), vec!["s1", "s2"]);
        assert_eq!(v.last_activity, None, "no times in this fixture");
        let timed = parse_show(
            r#"{"task":{"status":"running","created_at":100,"started_at":150},
                "runs":[{"status":"running","started_at":160}],
                "events":[{"kind":"heartbeat","created_at":900},{"kind":"claimed","created_at":155}]}"#,
        )
        .unwrap();
        assert_eq!(timed.last_activity, Some(900), "the newest heartbeat wins");
        for status in ["ready", "running", "review", "todo", "triage", "scheduled"] {
            let v = TaskView {
                status: status.into(),
                ..Default::default()
            };
            assert_eq!(next(&v), Next::Wait, "{status}");
        }
        let blocked = TaskView {
            status: "blocked".into(),
            runs: vec![Run {
                error: Some("max runtime exceeded".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(next(&blocked), Next::Block("max runtime exceeded".into()));
    }
}
