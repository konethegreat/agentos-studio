//! The evaluation harness: replay a finished mission on another agent or model.
//!
//! A replay starts at the mission's pull request base commit in a detached worktree under
//! `.worktrees/` and asks the chosen agent to satisfy the same acceptance criteria again. It is
//! scored on two halves: the repository's own test command (50) and a reviewer verdict on the
//! replay's local diff (50). It never pushes, never opens a pull request and removes its worktree
//! on every path out, including errors. Each replay writes one note under `03_Memory/evals/`.
//!
//! The reviewer contract is [`crate::review_agent::prompt`] plus
//! [`crate::review_agent::parse_verdict`], but the verdict is never written back to the mission:
//! `review_agent::review` takes a mission id and would move that real mission's status.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::missions::{self, Mission};
use crate::procs;
use crate::vault::{Doc, Vault};

/// A coding turn on another agent can take a while, so it gets half an hour.
const REPLAY_LIMIT: Duration = Duration::from_secs(1800);
const REVIEW_LIMIT: Duration = Duration::from_secs(600);
const TEST_LIMIT: Duration = Duration::from_secs(1800);
/// The reviewer never sees more than this much diff, the same bound `review_agent` uses.
const MAX_DIFF: usize = 12_000;
const MAX_TURNS: &str = "24";
const DETAIL_CHARS: usize = 500;

pub const AGENTS: [&str; 2] = ["claude-code", "hermes"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TestOutcome {
    pub command: String,
    pub ok: bool,
    /// The last lines of the run, enough to explain a failure without pasting the whole log.
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct TestCommand {
    pub program: String,
    pub args: Vec<String>,
    pub label: String,
}

/// The original mission's half of the comparison.
#[derive(Debug, Clone, Serialize)]
pub struct OriginalRun {
    pub agent: String,
    pub model: String,
    pub reviewer: Option<String>,
    pub verdict: Option<String>,
    pub tests: Option<bool>,
    pub score: u32,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalResult {
    pub date: String,
    pub mission: String,
    pub repo: String,
    pub agent: String,
    pub model: String,
    pub reviewer: String,
    pub base_commit: String,
    pub pr_url: String,
    pub duration_s: u64,
    pub cost_usd: f64,
    pub tests: Option<TestOutcome>,
    pub verdict: String,
    pub reviewer_note: String,
    pub score: u32,
    pub original_score: u32,
    pub comparison: String,
    pub changed_files: usize,
    /// A fresh worktree has no `node_modules`, so a JavaScript repository got an install first.
    pub deps_installed: bool,
    pub note_path: String,
    pub original: OriginalRun,
}

/// One row of the Brain view list, read back from the note's frontmatter.
#[derive(Debug, Clone, Serialize)]
pub struct EvalSummary {
    pub file: String,
    pub date: String,
    pub mission: String,
    pub repo: String,
    pub agent: String,
    pub model: String,
    pub reviewer: String,
    pub score: u32,
    pub original_score: u32,
    pub verdict: String,
    pub tests: String,
    pub cost_usd: f64,
    pub duration_s: u64,
    pub comparison: String,
}

/// `claude`, `claude-code` and `hermes` only: those are the agents that run headless here.
pub fn normalize_agent(raw: &str) -> Result<String> {
    let a = raw.trim().to_lowercase().replace('_', "-");
    let a = match a.as_str() {
        "claude" | "claudecode" => "claude-code".to_string(),
        other => other.to_string(),
    };
    if AGENTS.contains(&a.as_str()) {
        Ok(a)
    } else {
        bail!("'{raw}' cannot be replayed: only claude-code and hermes run headless")
    }
}

pub fn tests_label(tests: Option<bool>) -> &'static str {
    match tests {
        Some(true) => "pass",
        Some(false) => "fail",
        None => "none",
    }
}

/// Tests are half the score (a repository with no test command gets the midpoint, because there
/// was nothing to fail), and the reviewer's verdict is the other half. Pure.
pub fn score(tests: Option<bool>, verdict: &str) -> u32 {
    let tested = match tests {
        Some(true) => 50,
        Some(false) => 0,
        None => 25,
    };
    let approved = if verdict.trim().eq_ignore_ascii_case("approve") {
        50
    } else {
        0
    };
    tested + approved
}

/// The test result recorded on the original mission's `hermes verify` output. Pure.
pub fn original_tests(m: &Mission) -> Option<bool> {
    let v: Value = serde_json::from_str(m.verify.as_deref()?.trim()).ok()?;
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Some(true)
    } else if v.get("error").and_then(Value::as_str) == Some("no-recipe") {
        None
    } else {
        Some(false)
    }
}

/// The original run's score on the same rubric, from `verify` and the recorded verdict. Pure.
pub fn original_score(m: &Mission) -> u32 {
    score(
        original_tests(m),
        m.review_verdict.as_deref().unwrap_or_default(),
    )
}

/// One sentence naming the direction and size of the difference. Pure.
pub fn compare(replay: u32, original: u32) -> String {
    match replay.cmp(&original) {
        std::cmp::Ordering::Equal => format!("Level with the original: both scored {original}."),
        std::cmp::Ordering::Greater => format!(
            "The replay scores {replay}, {} more than the original {original}.",
            replay - original
        ),
        std::cmp::Ordering::Less => format!(
            "The replay scores {replay}, {} less than the original {original}.",
            original - replay
        ),
    }
}

fn part(text: &str, max: usize) -> String {
    crate::vault::slug(text, max)
        .replace('_', "-")
        .to_lowercase()
}

/// `2026-11-09-M-007-claude-code-claude-opus-5-5.md`. Pure.
pub fn file_name(date: &str, mission: &str, agent: &str, model: &str) -> String {
    let model = if model.trim().is_empty() {
        "default".to_string()
    } else {
        model.to_string()
    };
    format!(
        "{}-{}-{}-{}.md",
        part(date, 20),
        part(mission, 20),
        part(agent, 20),
        part(&model, 60)
    )
}

fn model_label(model: &str) -> &str {
    if model.trim().is_empty() {
        "default"
    } else {
        model.trim()
    }
}

fn cost_label(agent: &str, usd: f64) -> String {
    if agent == "claude-code" {
        "flat (subscription)".to_string()
    } else {
        format!("${usd:.2}")
    }
}

/// One line of plain text for the note: quoted model output never carries an em dash or a newline
/// into a file we own. Pure.
fn one_line(text: &str) -> String {
    text.replace(['\u{2014}', '\u{2013}'], "-")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn tail(text: &str, max: usize) -> String {
    let mut chars: Vec<char> = text.chars().rev().take(max).collect();
    chars.reverse();
    chars.into_iter().collect::<String>().trim().to_string()
}

/// The result note. Frontmatter is what the list reads; the body is for a person. Pure.
pub fn note_markdown(r: &EvalResult) -> String {
    let mut d = Doc::default();
    d.set_str("type", "eval");
    d.set_str("date", &r.date);
    d.set_str("mission", &r.mission);
    d.set_str("repo", &r.repo);
    d.set_str("agent", &r.agent);
    d.set_str("model", model_label(&r.model));
    d.set_str("reviewer", &r.reviewer);
    d.set_str("score", r.score.to_string());
    d.set_str("original_score", r.original_score.to_string());
    d.set_str("verdict", &r.verdict);
    d.set_str("tests", tests_label(r.tests.as_ref().map(|t| t.ok)));
    d.set_str("cost_usd", format!("{:.4}", r.cost_usd));
    d.set_str("duration_s", r.duration_s.to_string());
    d.set_str("base_commit", &r.base_commit);
    d.set_str("pr", &r.pr_url);
    d.set_str("comparison", &r.comparison);
    let short: String = r.base_commit.chars().take(7).collect();
    let tests = match &r.tests {
        Some(t) => format!(
            "{} (`{}`)",
            if t.ok { "pass" } else { "fail" },
            one_line(&t.command)
        ),
        None => "no test command was detected".to_string(),
    };
    let deps = if r.deps_installed {
        "\n- dependencies: installed for the fresh worktree"
    } else {
        ""
    };
    d.body = format!(
        "# {mission} replayed on {agent}\n\n\
{comparison}\n\n\
## Replay\n\n\
- base commit: `{short}` (the pull request's base)\n\
- duration: {duration}s\n\
- cost: {cost}\n\
- tests: {tests}{deps}\n\
- diff: {files} file(s) changed\n\n\
## Reviewer ({reviewer})\n\n\
verdict: **{verdict}**\n\n\
> {note}\n\n\
## Original run\n\n\
- agent: {oagent} ({omodel}), reviewer: {oreviewer}\n\
- tests: {otests}\n\
- verdict: {overdict}\n\
- score: {oscore}\n\
- cost: ${ocost:.2}\n\n\
The replay ran in a detached worktree, left no branch and opened no pull request.",
        mission = r.mission,
        agent = r.agent,
        comparison = r.comparison,
        short = short,
        duration = r.duration_s,
        cost = cost_label(&r.agent, r.cost_usd),
        tests = tests,
        files = r.changed_files,
        reviewer = r.reviewer,
        verdict = r.verdict,
        note = one_line(&r.reviewer_note),
        oagent = r.original.agent,
        omodel = model_label(&r.original.model),
        oreviewer = r.original.reviewer.as_deref().unwrap_or("none"),
        otests = tests_label(r.original.tests),
        overdict = r.original.verdict.as_deref().unwrap_or("none"),
        oscore = r.original.score,
        ocost = r.original.cost_usd,
    );
    d.render()
}

/// The repository's own test command, or `None` when there is nothing to run. Pure apart from
/// reading the files. A package.json script that is still the npm placeholder is not a test.
pub fn detect_test(dir: &Path) -> Option<TestCommand> {
    let pkg = dir.join("package.json");
    if let Ok(raw) = std::fs::read_to_string(&pkg) {
        if let Some(cmd) = npm_test(&raw) {
            return Some(cmd);
        }
    }
    if dir.join("Cargo.toml").is_file() {
        return Some(TestCommand {
            program: "cargo".into(),
            args: vec!["test".into(), "--quiet".into()],
            label: "cargo test".into(),
        });
    }
    if dir.join("go.mod").is_file() {
        return Some(TestCommand {
            program: "go".into(),
            args: vec!["test".into(), "./...".into()],
            label: "go test ./...".into(),
        });
    }
    let python = dir.join("pyproject.toml").is_file()
        || dir.join("pytest.ini").is_file()
        || dir.join("tox.ini").is_file();
    if python {
        if dir.join("uv.lock").is_file() && procs::find("uv").is_some() {
            return Some(TestCommand {
                program: "uv".into(),
                args: vec!["run".into(), "pytest".into(), "-q".into()],
                label: "uv run pytest".into(),
            });
        }
        return Some(TestCommand {
            program: "python".into(),
            args: vec!["-m".into(), "pytest".into(), "-q".into()],
            label: "python -m pytest".into(),
        });
    }
    None
}

fn npm_test(raw: &str) -> Option<TestCommand> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let script = v.get("scripts")?.get("test")?.as_str()?.trim();
    if script.is_empty() || script.contains("no test specified") {
        return None;
    }
    Some(TestCommand {
        program: "npm".into(),
        args: vec!["test".into(), "--silent".into()],
        label: "npm test".into(),
    })
}

/// Ask the agent to do the mission again. Pure.
pub fn replay_prompt(m: &Mission, base: &str) -> String {
    format!(
        "You are replaying a finished mission from its pull request's base commit ({base}).\n\n\
Goal:\n{goal}\n\nAcceptance criteria:\n{criteria}\n\n\
Work in the current folder and make the change that satisfies every criterion. Keep it focused. \
Do not create a branch, do not commit, do not push, do not open a pull request and do not leave \
this folder. Stop when the change is made; someone else runs the tests and the review.",
        goal = m.goal,
        criteria = m.acceptance
    )
}

fn original_of(m: &Mission) -> OriginalRun {
    OriginalRun {
        agent: m.agent.clone(),
        model: m.model.clone(),
        reviewer: m.reviewer.clone(),
        verdict: m.review_verdict.clone(),
        tests: original_tests(m),
        score: original_score(m),
        cost_usd: m.spent_usd,
    }
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

fn gh_json(dir: &Path, args: &[&str]) -> Result<Value> {
    let exe = procs::find("gh")
        .ok_or_else(|| anyhow!("gh is not installed, so the base commit cannot be read"))?;
    let out = procs::command(&exe).args(args).current_dir(dir).output()?;
    if !out.status.success() {
        bail!(
            "gh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout).map_err(|e| anyhow!("gh returned no JSON: {e}"))
}

/// The base commit has to exist locally before a worktree can start from it.
fn ensure_commit(dir: &Path, sha: &str) -> Result<()> {
    let probe = format!("{sha}^{{commit}}");
    if git(dir, &["cat-file", "-e", &probe]).is_ok() {
        return Ok(());
    }
    git(dir, &["fetch", "--quiet", "origin"])
        .and_then(|_| git(dir, &["cat-file", "-e", &probe]))
        .with_context(|| {
            format!(
                "the base commit {sha} is not available in {}",
                dir.display()
            )
        })?;
    Ok(())
}

/// A detached worktree that removes itself, on success and on every error path.
struct Sandbox {
    repo: PathBuf,
    path: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let p = self.path.display().to_string();
        let _ = git(&self.repo, &["worktree", "remove", "--force", &p]);
        let _ = git(&self.repo, &["worktree", "prune"]);
        if self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn sandbox(repo_dir: &Path, name: &str, base: &str) -> Result<Sandbox> {
    let common = git(repo_dir, &["rev-parse", "--git-common-dir"])?;
    let common = {
        let p = PathBuf::from(&common);
        if p.is_absolute() {
            p
        } else {
            repo_dir.join(p)
        }
    };
    crate::worktree::ensure_excluded(&common)?;
    let path = repo_dir.join(crate::worktree::DIR).join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    git(
        repo_dir,
        &[
            "worktree",
            "add",
            "--detach",
            &path.display().to_string(),
            base,
        ],
    )?;
    Ok(Sandbox {
        repo: repo_dir.to_path_buf(),
        path,
    })
}

fn claude_turn(
    dir: &Path,
    prompt: &str,
    model: &str,
    accept_edits: bool,
    limit: Duration,
) -> Result<(String, Option<String>)> {
    let mut args: Vec<&str> = vec!["-p"];
    if accept_edits {
        args.extend(["--permission-mode", "acceptEdits"]);
    }
    if !model.trim().is_empty() {
        args.extend(["--model", model.trim()]);
    }
    let (code, out, err) =
        procs::run_with_timeout("claude", &args, Some(prompt), Some(dir), limit)?;
    if code != Some(0) && out.trim().is_empty() {
        bail!("claude failed: {}", err.trim());
    }
    Ok((out, None))
}

fn hermes_turn(
    dir: &Path,
    prompt: &str,
    model: &str,
    profile: Option<&str>,
    max_turns: Option<&str>,
    limit: Duration,
) -> Result<(String, Option<String>)> {
    let qdir = crate::store::studio_dir().join("evals");
    std::fs::create_dir_all(&qdir)?;
    let qf = qdir.join(format!("{}.md", uuid::Uuid::new_v4().simple()));
    std::fs::write(&qf, prompt)?;
    let mut args: Vec<String> = Vec::new();
    if let Some(p) = profile.map(str::trim).filter(|p| !p.is_empty()) {
        args.extend(["-p".to_string(), p.to_string()]);
    }
    args.extend(["chat", "-Q", "--oneshot"].map(String::from));
    // The replay is not a mission: the repository's constitution must not send it to open a PR.
    args.push("--ignore-rules".into());
    if let Some(t) = max_turns {
        args.extend(["--max-turns".to_string(), t.to_string()]);
    }
    args.extend(["--query-file".to_string(), qf.display().to_string()]);
    args.extend(["--in".to_string(), dir.display().to_string()]);
    if !model.trim().is_empty() {
        args.extend(["-m".to_string(), model.trim().to_string()]);
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let r = procs::run_with_timeout("hermes", &refs, None, Some(dir), limit);
    let _ = std::fs::remove_file(&qf);
    let (code, out, err) = r?;
    if code != Some(0) && out.trim().is_empty() {
        bail!("the hermes turn failed: {}", err.trim());
    }
    let sid = crate::review_agent::session_id_in(&err);
    Ok((out, sid))
}

fn run_tests(dir: &Path) -> Option<TestOutcome> {
    let cmd = detect_test(dir)?;
    let args: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
    let r = procs::run_with_timeout(&cmd.program, &args, None, Some(dir), TEST_LIMIT);
    Some(match r {
        Ok((code, out, err)) => {
            let ok = code == Some(0);
            let text = if ok { out } else { format!("{out}\n{err}") };
            TestOutcome {
                command: cmd.label,
                ok,
                detail: tail(&text, DETAIL_CHARS),
            }
        }
        Err(e) => TestOutcome {
            command: cmd.label,
            ok: false,
            detail: one_line(&e.to_string()),
        },
    })
}

/// Stage everything the replay touched, so new files appear in the diff too.
fn replay_diff(dir: &Path) -> Result<(String, usize)> {
    git(dir, &["add", "-A"])?;
    let diff = git(dir, &["diff", "--cached"])?;
    let names = git(dir, &["diff", "--cached", "--name-only"])?;
    let files = names.lines().filter(|l| !l.trim().is_empty()).count();
    Ok((diff, files))
}

/// A worktree starts without installed packages, so a JavaScript repository gets one quiet
/// install before the tests run. Best effort: a failed install shows up as failing tests.
fn install_deps(dir: &Path) -> bool {
    if !dir.join("package.json").is_file() || dir.join("node_modules").is_dir() {
        return false;
    }
    procs::run_with_timeout(
        "npm",
        &["install", "--silent", "--no-audit", "--no-fund"],
        None,
        Some(dir),
        Duration::from_secs(900),
    )
    .is_ok()
}

fn write_note(v: &Vault, r: &EvalResult) -> Result<String> {
    let dir = v.memory().join("evals");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(file_name(
        &r.date,
        &r.mission,
        &r.agent,
        model_label(&r.model),
    ));
    std::fs::write(&path, note_markdown(r))?;
    Ok(path.display().to_string())
}

/// Replay one finished mission on `agent` (and `model`, when given) and write one eval note.
pub fn run(v: &Vault, mission_id: &str, agent: &str, model: Option<&str>) -> Result<EvalResult> {
    let agent = normalize_agent(agent)?;
    let model = model
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or_default()
        .to_string();
    let m = missions::get(mission_id)?.ok_or_else(|| anyhow!("no mission '{mission_id}'"))?;
    if m.status != "done" && m.status != "failed" {
        bail!(
            "{mission_id} is {}; only a finished mission can be replayed",
            m.status
        );
    }
    let number = missions::pr_number(&m).ok_or_else(|| {
        anyhow!("{mission_id} has no pull request, so there is no base commit to replay from")
    })?;
    let repo = crate::github::name_with_owner(&m.repo)?;
    let dir = crate::learning::workspace_for(&m.repo).ok_or_else(|| {
        anyhow!(
            "repository '{}' has no checkout on this machine, so there is nowhere to replay",
            m.repo
        )
    })?;
    let started = Instant::now();
    let pr = gh_json(
        &dir,
        &[
            "pr",
            "view",
            &number.to_string(),
            "-R",
            &repo,
            "--json",
            "baseRefOid,url",
        ],
    )?;
    let base = pr
        .get("baseRefOid")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if base.is_empty() {
        bail!("could not read the base commit of {repo}#{number}");
    }
    let pr_url = pr
        .get("url")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| m.pr_url.clone())
        .unwrap_or_default();
    ensure_commit(&dir, &base)?;
    // A second agent on another provider where possible, as `review_agent` intends.
    let reviewer = if agent == "claude-code" {
        "hermes".to_string()
    } else {
        "claude-code".to_string()
    };
    let name = format!(
        "{}-eval-{}",
        m.id.to_lowercase(),
        uuid::Uuid::new_v4().simple()
    );
    let sandbox = sandbox(&dir, &name, &base)?;
    let wt = sandbox.path.clone();
    let prompt = replay_prompt(&m, &base);
    let (_, session) = if agent == "claude-code" {
        claude_turn(&wt, &prompt, &model, true, REPLAY_LIMIT)?
    } else {
        hermes_turn(&wt, &prompt, &model, None, Some(MAX_TURNS), REPLAY_LIMIT)?
    };
    let (diff, changed_files) = replay_diff(&wt)?;
    let deps_installed = install_deps(&wt);
    let tests = run_tests(&wt);
    let review_prompt =
        crate::review_agent::prompt(&m, &diff.chars().take(MAX_DIFF).collect::<String>());
    let (text, review_session) = if reviewer == "claude-code" {
        claude_turn(&wt, &review_prompt, "", false, REVIEW_LIMIT)?
    } else {
        let profile = crate::hermes_surfaces::profile_for_role("reviewer");
        hermes_turn(
            &wt,
            &review_prompt,
            "",
            profile.as_deref(),
            None,
            REVIEW_LIMIT,
        )?
    };
    let (verdict, reviewer_note) = crate::review_agent::parse_verdict(&text).ok_or_else(|| {
        anyhow!(
            "the reviewer did not return a verdict. It said: {}",
            text.chars().take(300).collect::<String>()
        )
    })?;
    let sessions: Vec<String> = session
        .into_iter()
        .chain(review_session)
        .filter(|s| !s.trim().is_empty())
        .collect();
    let cost_usd = crate::usage::metered_cost(&sessions);
    let original = original_of(&m);
    let earned = score(tests.as_ref().map(|t| t.ok), &verdict);
    let comparison = compare(earned, original.score);
    let mut result = EvalResult {
        date: chrono::Local::now().format("%Y-%m-%d").to_string(),
        mission: m.id.clone(),
        repo: m.repo.clone(),
        agent,
        model,
        reviewer,
        base_commit: base,
        pr_url,
        duration_s: started.elapsed().as_secs(),
        cost_usd,
        tests,
        verdict,
        reviewer_note,
        score: earned,
        original_score: original.score,
        comparison,
        changed_files,
        deps_installed,
        note_path: String::new(),
        original,
    };
    // The worktree is gone by the time this returns, even when the note write fails.
    drop(sandbox);
    result.note_path = write_note(v, &result)?;
    Ok(result)
}

fn meta(d: &Doc, key: &str) -> String {
    d.get(key).unwrap_or_default().trim().to_string()
}

fn meta_u32(d: &Doc, key: &str) -> u32 {
    meta(d, key).parse().unwrap_or(0)
}

fn meta_f64(d: &Doc, key: &str) -> f64 {
    meta(d, key).parse().unwrap_or(0.0)
}

/// Every replay note in the vault, newest first.
pub fn list(v: &Vault) -> Result<Vec<EvalSummary>> {
    let dir = v.memory().join("evals");
    let mut out: Vec<EvalSummary> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_file() || p.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let raw = std::fs::read_to_string(&p).unwrap_or_default();
        let d = Doc::parse(&raw);
        if d.get("type") != Some("eval") {
            continue;
        }
        out.push(EvalSummary {
            file: p.display().to_string(),
            date: meta(&d, "date"),
            mission: meta(&d, "mission"),
            repo: meta(&d, "repo"),
            agent: meta(&d, "agent"),
            model: meta(&d, "model"),
            reviewer: meta(&d, "reviewer"),
            score: meta_u32(&d, "score"),
            original_score: meta_u32(&d, "original_score"),
            verdict: meta(&d, "verdict"),
            tests: meta(&d, "tests"),
            cost_usd: meta_f64(&d, "cost_usd"),
            duration_s: meta_u32(&d, "duration_s") as u64,
            comparison: meta(&d, "comparison"),
        });
    }
    out.sort_by(|a, b| b.date.cmp(&a.date).then(b.file.cmp(&a.file)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission() -> Mission {
        Mission {
            id: "M-007".into(),
            repo: "agentos-sandbox".into(),
            goal: "Add a health endpoint".into(),
            acceptance: "GET /health answers 200".into(),
            role: "implementer".into(),
            agent: "hermes".into(),
            status: "done".into(),
            provider: "claude-subscription".into(),
            model: "claude-opus-5-5".into(),
            budget_usd: 0.0,
            spent_usd: 0.42,
            task_id: Some("TASK-1".into()),
            reviewer: Some("claude-code".into()),
            review_verdict: Some("approve".into()),
            review_note: Some("covers the endpoint".into()),
            pr_url: Some("https://github.com/konethegreat/agentos-sandbox/pull/42".into()),
            created_at: 0,
            updated_at: 0,
            verify: Some(r#"{"ok": true, "phases": []}"#.into()),
        }
    }

    fn result() -> EvalResult {
        EvalResult {
            date: "2026-11-09".into(),
            mission: "M-007".into(),
            repo: "agentos-sandbox".into(),
            agent: "claude-code".into(),
            model: "claude-opus-5-5".into(),
            reviewer: "hermes".into(),
            base_commit: "0123456789abcdef0123456789abcdef01234567".into(),
            pr_url: "https://github.com/konethegreat/agentos-sandbox/pull/42".into(),
            duration_s: 95,
            cost_usd: 0.0,
            tests: Some(TestOutcome {
                command: "npm test".into(),
                ok: true,
                detail: "3 passed".into(),
            }),
            verdict: "approve".into(),
            reviewer_note: "Satisfies every criterion.".into(),
            score: 100,
            original_score: 100,
            comparison: compare(100, 100),
            changed_files: 2,
            deps_installed: false,
            note_path: String::new(),
            original: original_of(&mission()),
        }
    }

    #[test]
    fn the_two_halves_add_up() {
        assert_eq!(score(Some(true), "approve"), 100);
        assert_eq!(score(Some(false), "reject"), 0);
        assert_eq!(score(Some(true), "reject"), 50);
        assert_eq!(score(None, "reject"), 25);
        assert_eq!(
            score(None, "Approve"),
            75,
            "the verdict is case insensitive"
        );
    }

    #[test]
    fn the_verify_record_says_whether_tests_ran() {
        let mut m = mission();
        assert_eq!(original_tests(&m), Some(true));
        m.verify = Some(r#"{"ok": false, "error": "no-recipe"}"#.into());
        assert_eq!(original_tests(&m), None, "nothing to run is not a failure");
        m.verify = Some(r#"{"ok": false, "phases": [{"phase": "test", "ok": false}]}"#.into());
        assert_eq!(original_tests(&m), Some(false));
        m.verify = Some("not json".into());
        assert_eq!(original_tests(&m), None);
        m.verify = None;
        assert_eq!(original_tests(&m), None);
    }

    #[test]
    fn the_original_score_uses_the_same_rubric() {
        let mut m = mission();
        assert_eq!(original_score(&m), 100);
        m.review_verdict = Some("reject".into());
        assert_eq!(original_score(&m), 50);
        m.verify = Some(r#"{"ok": false, "error": "no-recipe"}"#.into());
        assert_eq!(original_score(&m), 25);
        m.review_verdict = None;
        assert_eq!(original_score(&m), 25);
    }

    #[test]
    fn the_comparison_names_direction_and_size() {
        assert_eq!(
            compare(100, 100),
            "Level with the original: both scored 100."
        );
        assert!(compare(75, 50).contains("25 more than the original 50"));
        assert!(compare(25, 75).contains("50 less than the original 75"));
    }

    #[test]
    fn the_file_name_is_readable() {
        assert_eq!(
            file_name("2026-11-09", "M-007", "claude-code", "claude-opus-5-5"),
            "2026-11-09-m-007-claude-code-claude-opus-5-5.md"
        );
        assert_eq!(
            file_name("2026-11-09", "M-007", "hermes", ""),
            "2026-11-09-m-007-hermes-default.md"
        );
    }

    #[test]
    fn a_replay_is_one_agent_only() {
        assert_eq!(normalize_agent("Claude").unwrap(), "claude-code");
        assert_eq!(normalize_agent("hermes").unwrap(), "hermes");
        assert!(normalize_agent("antigravity").is_err());
    }

    #[test]
    fn the_replay_prompt_carries_the_criteria_and_the_brakes() {
        let p = replay_prompt(&mission(), "abc123");
        assert!(p.contains("GET /health answers 200"));
        assert!(p.contains("abc123"));
        assert!(p.contains("do not open a pull request"));
    }

    #[test]
    fn the_note_round_trips_through_the_vault_parser() {
        let r = result();
        let text = note_markdown(&r);
        let d = Doc::parse(&text);
        assert_eq!(d.get("type"), Some("eval"));
        assert_eq!(d.get("mission"), Some("M-007"));
        assert_eq!(d.get("agent"), Some("claude-code"));
        assert_eq!(d.get("score"), Some("100"));
        assert_eq!(d.get("tests"), Some("pass"));
        assert_eq!(d.get("verdict"), Some("approve"));
        assert!(d.body.contains("Level with the original"));
        assert!(d.body.contains("`0123456`"));
        assert!(d.body.contains("npm test"));
        assert!(d.body.contains("Satisfies every criterion."));
        assert!(!text.contains('\u{2014}') && !text.contains('\u{2013}'));
    }

    #[test]
    fn the_note_never_carries_a_line_break_into_the_quote() {
        let mut r = result();
        r.reviewer_note = "Fine.\nBut tighten\u{2014}please.".into();
        let text = note_markdown(&r);
        assert!(text.contains("> Fine. But tighten-please."), "{text}");
    }

    #[test]
    fn test_detection_prefers_a_real_script_then_the_language() {
        let d = tempfile::tempdir().unwrap();
        assert!(detect_test(d.path()).is_none());
        std::fs::write(
            d.path().join("package.json"),
            r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#,
        )
        .unwrap();
        assert!(
            detect_test(d.path()).is_none(),
            "the npm placeholder is not a test"
        );
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert_eq!(detect_test(d.path()).unwrap().label, "cargo test");
        std::fs::write(
            d.path().join("package.json"),
            r#"{"scripts": {"test": "vitest run"}}"#,
        )
        .unwrap();
        assert_eq!(detect_test(d.path()).unwrap().label, "npm test");
        std::fs::remove_file(d.path().join("package.json")).unwrap();
        std::fs::remove_file(d.path().join("Cargo.toml")).unwrap();
        std::fs::write(d.path().join("pyproject.toml"), "[project]\nname = \"x\"\n").unwrap();
        assert_eq!(detect_test(d.path()).unwrap().label, "python -m pytest");
        std::fs::remove_file(d.path().join("pyproject.toml")).unwrap();
        std::fs::write(d.path().join("go.mod"), "module x\n").unwrap();
        assert_eq!(detect_test(d.path()).unwrap().label, "go test ./...");
    }

    #[test]
    fn the_list_reads_eval_notes_only() {
        let d = tempfile::tempdir().unwrap();
        let v = Vault::new(d.path());
        let evals = v.memory().join("evals");
        std::fs::create_dir_all(&evals).unwrap();
        std::fs::write(evals.join("a.md"), note_markdown(&result())).unwrap();
        std::fs::write(
            evals.join("b.md"),
            "---\ntype: fact\ntopic: something\n---\n\nA note.",
        )
        .unwrap();
        let rows = list(&v).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "a memory note in the same folder is not an eval"
        );
        assert_eq!(rows[0].mission, "M-007");
        assert_eq!(rows[0].score, 100);
        assert_eq!(rows[0].original_score, 100);
        assert_eq!(rows[0].tests, "pass");
        assert!(rows[0].comparison.starts_with("Level with the original"));
        assert!(list(&Vault::new(d.path().join("empty")))
            .unwrap()
            .is_empty());
    }
}
