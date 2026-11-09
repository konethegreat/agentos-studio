//! Goal completion: when every mission under a running goal is merged, run the goal's done checks
//! against the merged code and report.
//!
//! The checks run in a detached worktree of the repository's default branch under Studio's data
//! folder, never in the owner's checkout. All passing: the goal is done, an outcome note is
//! remembered, the project note gets an outcome line, Notion is republished and the report goes to
//! the messaging target. A failing check gets one fix mission; a second failure escalates
//! (`done_check`), and Retry gives it another fix mission.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Result};
use serde::Serialize;

use crate::goals::{self, Check, Goal};
use crate::missions::Mission;
use crate::store;
use crate::vault::Vault;

const COMMAND_LIMIT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CheckResult {
    pub kind: String,
    pub target: String,
    pub repo: String,
    pub ok: bool,
    /// The last lines of a command's output, the URL's status, or why a file is missing.
    pub detail: String,
}

/// Is this mission's work on the default branch? Done and merged (or the owner skipped or closed
/// its pull request, which is recorded and counts as decided). Pure over the recorded outcome.
pub fn mission_settled(m: &Mission, merge_outcome: Option<&str>) -> bool {
    m.status == "done" && merge_outcome.is_some()
}

/// May the goal's checks run now? Running, with missions, every one settled. Pure.
pub fn ready_to_verify(g: &Goal, ms: &[Mission], outcome: impl Fn(&str) -> Option<String>) -> bool {
    g.status == "running"
        && !ms.is_empty()
        && ms
            .iter()
            .all(|m| mission_settled(m, outcome(&m.id).as_deref()))
}

/// At Watch the owner merges with the board's button, which Studio does not record; read the
/// pull request's state once the mission is done and record a merge that happened elsewhere.
fn outcome_of(m: &Mission) -> Option<String> {
    if let Some(o) = crate::goal_loop::merge_outcome(&m.id) {
        return Some(o);
    }
    if m.status != "done" {
        return None;
    }
    let n = crate::missions::pr_number(m)?;
    let repo = crate::github::name_with_owner(&m.repo).ok()?;
    let state = crate::github::pr_status(&repo, n).ok()?;
    let o = match state.as_str() {
        "MERGED" => format!("merged:{}", store::now()),
        "CLOSED" => "closed".to_string(),
        _ => return None,
    };
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, &crate::goal_loop::merge_key(&m.id), &o);
    }
    Some(o)
}

fn fails_key(goal_id: &str) -> String {
    format!("goal.dodfail.{goal_id}")
}

fn fails(goal_id: &str) -> u32 {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, &fails_key(goal_id)).ok().flatten())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn set_fails(goal_id: &str, n: u32) {
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, &fails_key(goal_id), &n.to_string());
    }
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// A detached worktree of the repository's default branch, freshly fetched.
fn verify_worktree(goal_id: &str, repo: &str) -> Result<(PathBuf, PathBuf)> {
    let dir = crate::review_agent::project_dir(repo)
        .filter(|d| d.is_dir())
        .ok_or_else(|| {
            anyhow::anyhow!("repository '{repo}' is not a known project with a folder")
        })?;
    let wt = store::studio_dir()
        .join("verify")
        .join(format!("{goal_id}-{}", crate::vault::slug(repo, 40)));
    let wts = wt.display().to_string();
    let git = |args: &[&str]| {
        crate::procs::run_with_timeout("git", args, None, Some(&dir), Duration::from_secs(120))
    };
    let _ = git(&["worktree", "remove", "--force", &wts]);
    let _ = std::fs::remove_dir_all(&wt);
    let _ = git(&["worktree", "prune"]);
    let (code, _, err) = git(&["fetch", "-q", "origin"])?;
    if code != Some(0) {
        bail!("git fetch in {repo} failed: {}", err.trim());
    }
    let head = git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .ok()
        .and_then(|(c, o, _)| (c == Some(0)).then(|| o.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "origin/main".into());
    if let Some(parent) = wt.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (code, _, err) = git(&["worktree", "add", "-q", "--detach", &wts, &head])?;
    if code != Some(0) {
        bail!("could not check out {head} of {repo}: {}", err.trim());
    }
    Ok((dir, wt))
}

fn remove_worktree(dir: &Path, wt: &Path) {
    let wts = wt.display().to_string();
    let _ = crate::procs::run_with_timeout(
        "git",
        &["worktree", "remove", "--force", &wts],
        None,
        Some(dir),
        Duration::from_secs(60),
    );
    let _ = std::fs::remove_dir_all(wt);
}

fn run_command(dir: &Path, command: &str) -> (bool, String) {
    #[cfg(windows)]
    let r = crate::procs::run_with_timeout("cmd", &["/C", command], None, Some(dir), COMMAND_LIMIT);
    #[cfg(not(windows))]
    let r = crate::procs::run_with_timeout("sh", &["-c", command], None, Some(dir), COMMAND_LIMIT);
    match r {
        Ok((code, out, err)) => {
            let text = format!("{out}\n{err}");
            (code == Some(0), last_lines(&text, 12))
        }
        Err(e) => (false, format!("{e:#}")),
    }
}

fn check_url(url: &str) -> (bool, String) {
    let req = match ureq::http::Request::builder()
        .method("GET")
        .uri(url)
        .body(())
    {
        Ok(r) => r,
        Err(e) => return (false, format!("bad URL: {e}")),
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    match agent.run(req) {
        Ok(res) => {
            let s = res.status().as_u16();
            ((200..300).contains(&s), format!("HTTP {s}"))
        }
        Err(ureq::Error::StatusCode(s)) => (false, format!("HTTP {s}")),
        Err(e) => (false, format!("no answer: {e}")),
    }
}

/// Run every check, grouped per repository so each gets one worktree.
pub fn run_checks(g: &Goal) -> Vec<CheckResult> {
    let mut out = Vec::new();
    let repo_of = |c: &Check| {
        c.repo
            .clone()
            .filter(|r| !r.trim().is_empty())
            .unwrap_or_else(|| g.repos[0].clone())
    };
    let mut repos: Vec<String> = g.checks.iter().map(repo_of).collect();
    repos.sort();
    repos.dedup();
    for repo in repos {
        let here: Vec<&Check> = g.checks.iter().filter(|c| repo_of(c) == repo).collect();
        let needs_tree = here.iter().any(|c| c.kind != "url");
        let tree = if needs_tree {
            Some(verify_worktree(&g.id, &repo))
        } else {
            None
        };
        for c in here {
            let (ok, detail) = match (c.kind.as_str(), &tree) {
                ("url", _) => check_url(&c.target),
                (_, Some(Err(e))) => (false, format!("{e:#}")),
                ("file", Some(Ok((_, wt)))) => {
                    let p = wt.join(&c.target);
                    if p.exists() {
                        (true, "present".into())
                    } else {
                        (false, "missing on the default branch".into())
                    }
                }
                ("command", Some(Ok((_, wt)))) => run_command(wt, &c.target),
                _ => (false, "unknown check".into()),
            };
            out.push(CheckResult {
                kind: c.kind.clone(),
                target: c.target.clone(),
                repo: repo.clone(),
                ok,
                detail,
            });
        }
        if let Some(Ok((dir, wt))) = &tree {
            remove_worktree(dir, wt);
        }
    }
    out
}

/// The completion report: what was asked, what was done, what it cost, how it was checked. Pure.
pub fn report(g: &Goal, ms: &[Mission], results: &[CheckResult], now: i64) -> String {
    let mins = (now - g.created_at).max(0) / 60;
    let took = if mins >= 120 {
        format!("{} h {} min", mins / 60, mins % 60)
    } else {
        format!("{mins} min")
    };
    let mut s = format!(
        "{}: {}\n\nDone in {took}, {} missions, metered spend ${:.2}.\n\nMissions:\n",
        g.id,
        g.outcome,
        ms.len(),
        goals::spent(ms)
    );
    for m in ms {
        s.push_str(&format!(
            "- {} by {}, reviewed by {}: {}{}\n",
            m.id,
            m.agent,
            m.reviewer.as_deref().unwrap_or("nobody"),
            m.goal.lines().next().unwrap_or(""),
            m.pr_url
                .as_deref()
                .map(|u| format!(" {u}"))
                .unwrap_or_default()
        ));
    }
    s.push_str("\nDefinition of done:\n");
    for r in results {
        s.push_str(&format!(
            "- {} {} `{}` in {}: {}\n",
            if r.ok { "passed" } else { "FAILED" },
            r.kind,
            r.target,
            r.repo,
            r.detail.lines().last().unwrap_or("")
        ));
    }
    s
}

/// One fix mission for the failing checks, under the goal, on the repository of the first failure.
fn fix_mission(g: &Goal, failed: &[&CheckResult]) -> Result<Mission> {
    let repo = failed
        .first()
        .map(|r| r.repo.clone())
        .unwrap_or_else(|| g.repos[0].clone());
    let lines: Vec<String> = failed
        .iter()
        .map(|r| format!("- {} `{}` in {}:\n{}", r.kind, r.target, r.repo, r.detail))
        .collect();
    let text = format!(
        "Make the done checks of goal {} pass.\n\nEvery mission of the goal is merged, but these checks fail on the default branch:\n{}\n\nGoal: {}",
        g.id,
        lines.join("\n"),
        g.outcome
    );
    let acceptance: Vec<String> = failed
        .iter()
        .map(|r| format!("- {} `{}` passes in {}", r.kind, r.target, r.repo))
        .collect();
    let m = crate::router::create_mission(
        &repo,
        &text,
        &acceptance.join("\n"),
        "implementer",
        crate::assign::AUTO,
        "",
        "",
        g.budget_usd,
    )?;
    goals::attach(&g.id, &m.id)?;
    Ok(m)
}

/// What happened in a completion pass, for the tick report.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Outcome {
    pub done: Vec<String>,
    pub fix_missions: Vec<String>,
    pub escalated: Vec<String>,
}

/// One pass over running goals.
pub fn pass(v: &Vault) -> Outcome {
    let mut out = Outcome::default();
    for g in goals::list(200).unwrap_or_default() {
        if g.status != "running" {
            continue;
        }
        let ms = goals::missions_of(&g.id).unwrap_or_default();
        if !ready_to_verify(&g, &ms, |id| {
            ms.iter().find(|m| m.id == id).and_then(outcome_of)
        }) {
            continue;
        }
        if let Err(e) = verify(v, &g, &ms, &mut out) {
            let _ = goals::set_note(&g.id, &format!("Checking the goal failed: {e:#}"));
        }
    }
    out
}

fn verify(v: &Vault, g: &Goal, ms: &[Mission], out: &mut Outcome) -> Result<()> {
    goals::set_status(&g.id, &["running"], "verifying", "Running the done checks")?;
    let results = run_checks(g);
    let failed: Vec<&CheckResult> = results.iter().filter(|r| !r.ok).collect();
    let now = store::now();
    if failed.is_empty() {
        let text = report(g, ms, &results, now);
        goals::set_report(&g.id, &text)?;
        goals::set_status(&g.id, &["verifying"], "done", "Every done check passed.")?;
        let _ = goals::write_note(v, &g.id);
        remember(v, g, &text);
        if !cfg!(test) {
            let _ = crate::notion::publish(v, false);
            if let Some(target) = crate::hermes_surfaces::notify_target() {
                let _ = crate::hermes_surfaces::send(
                    &target,
                    &format!("AgentOS Studio: goal done.\n{text}"),
                );
            }
        }
        out.done.push(g.id.clone());
        return Ok(());
    }
    let n = fails(&g.id) + 1;
    set_fails(&g.id, n);
    let names: Vec<String> = failed
        .iter()
        .map(|r| format!("{} `{}`", r.kind, r.target))
        .collect();
    if n >= 2 {
        goals::set_status(
            &g.id,
            &["verifying"],
            "paused",
            &format!("Done checks failed twice: {}", names.join(", ")),
        )?;
        let e = crate::escalation::raise_and_send(
            Some(&g.id),
            None,
            "done_check",
            &format!(
                "{}'s done checks failed twice after its missions merged: {}. Try one more fix mission, or stop the goal?",
                g.id,
                names.join(", ")
            ),
            &["retry", "stop"],
            "stop",
        )?;
        out.escalated.push(e.id);
    } else {
        let m = fix_mission(g, &failed)?;
        goals::set_status(
            &g.id,
            &["verifying"],
            "running",
            &format!(
                "Done checks failed ({}); fix mission {} created.",
                names.join(", "),
                m.id
            ),
        )?;
        out.fix_missions.push(m.id);
    }
    let _ = goals::write_note(v, &g.id);
    Ok(())
}

/// Retry after a second failed check (the owner's answer to a `done_check` escalation): one more
/// fix mission, and the goal runs again.
pub fn retry_after_escalation(v: &Vault, goal_id: &str) -> Result<()> {
    let g = goals::get(goal_id)?.ok_or_else(|| anyhow::anyhow!("no goal '{goal_id}'"))?;
    let results = run_checks(&g);
    let failed: Vec<&CheckResult> = results.iter().filter(|r| !r.ok).collect();
    if failed.is_empty() {
        // Fixed in the meantime: the next pass verifies and finishes the goal.
        goals::set_status(goal_id, &["paused"], "running", "The done checks pass now.")?;
        return Ok(());
    }
    let m = fix_mission(&g, &failed)?;
    set_fails(goal_id, 1);
    goals::set_status(
        goal_id,
        &["paused"],
        "running",
        &format!("Retry: fix mission {} created.", m.id),
    )?;
    let _ = goals::write_note(v, goal_id);
    Ok(())
}

/// The outcome note and the project note's outcome line.
fn remember(v: &Vault, g: &Goal, text: &str) {
    let _ = v.write_typed_memory(
        &format!("Goal {} outcome", g.id),
        text,
        "outcome",
        &crate::vault::MemMeta {
            kind: Some(crate::vault::MemKind::Episode),
            agent: Some("agentos-studio".into()),
            repo: g.repos.first().cloned(),
            ..Default::default()
        },
    );
    for repo in &g.repos {
        let rel = format!("01_Projects/{repo}.md");
        let line = format!(
            "- {}: goal {} done: {} ([[{}]])\n",
            chrono::Local::now().format("%Y-%m-%d"),
            g.id,
            g.outcome,
            goals::note_rel(&g.id).trim_end_matches(".md")
        );
        let body = v.read_note(&rel).unwrap_or_else(|_| format!("# {repo}\n"));
        let updated = if body.contains("\n## Outcomes\n") {
            body.replacen("\n## Outcomes\n", &format!("\n## Outcomes\n\n{line}"), 1)
        } else {
            format!("{}\n\n## Outcomes\n\n{line}", body.trim_end())
        };
        let _ = v.write_note(&rel, &updated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal() -> Goal {
        Goal {
            id: "G-001".into(),
            outcome: "Add a probe endpoint".into(),
            repos: vec!["agentos-sandbox-1".into()],
            checks: vec![Check {
                kind: "file".into(),
                target: "README.md".into(),
                repo: None,
            }],
            budget_usd: 0.0,
            deadline: None,
            autonomy: "ask".into(),
            status: "running".into(),
            note: String::new(),
            report: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn m(id: &str, status: &str) -> Mission {
        Mission {
            id: id.into(),
            repo: "agentos-sandbox-1".into(),
            goal: "Add GET /probe\n\nbody".into(),
            acceptance: "a".into(),
            role: "implementer".into(),
            agent: "claude-code".into(),
            status: status.into(),
            provider: String::new(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.25,
            task_id: None,
            reviewer: Some("antigravity".into()),
            review_verdict: Some("approve".into()),
            review_note: None,
            pr_url: Some("https://github.com/o/r/pull/9".into()),
            created_at: 0,
            updated_at: 0,
            verify: None,
        }
    }

    #[test]
    fn a_goal_is_verified_once_every_mission_is_merged_or_decided() {
        let g = goal();
        let ms = vec![m("M-001", "done"), m("M-002", "done")];
        let merged = |_: &str| Some("merged:1".to_string());
        assert!(ready_to_verify(&g, &ms, merged));
        let one_open = |id: &str| (id == "M-001").then(|| "merged:1".to_string());
        assert!(
            !ready_to_verify(&g, &ms, one_open),
            "M-002 is approved but not merged"
        );
        let ms2 = vec![m("M-001", "done"), m("M-002", "review")];
        assert!(!ready_to_verify(&g, &ms2, merged));
        assert!(!ready_to_verify(&g, &[], merged), "no missions yet");
        let mut paused = goal();
        paused.status = "paused".into();
        assert!(!ready_to_verify(&paused, &ms, merged));
    }

    #[test]
    fn the_report_names_missions_reviewers_spend_and_checks() {
        let g = goal();
        let r = report(
            &g,
            &[m("M-001", "done")],
            &[CheckResult {
                kind: "command".into(),
                target: "npm test".into(),
                repo: "agentos-sandbox-1".into(),
                ok: true,
                detail: "...\nok 3 tests".into(),
            }],
            150 * 60,
        );
        assert!(r.starts_with("G-001: Add a probe endpoint"));
        assert!(r.contains("Done in 2 h 30 min, 1 missions, metered spend $0.25."));
        assert!(r.contains("- M-001 by claude-code, reviewed by antigravity: Add GET /probe https://github.com/o/r/pull/9"));
        assert!(r.contains("- passed command `npm test` in agentos-sandbox-1: ok 3 tests"));
    }

    #[test]
    fn a_command_check_runs_in_its_folder_and_reports_the_tail() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("marker.txt"), "x").unwrap();
        #[cfg(windows)]
        let (ok_cmd, bad_cmd) = ("type marker.txt", "type nope.txt");
        #[cfg(not(windows))]
        let (ok_cmd, bad_cmd) = ("cat marker.txt", "cat nope.txt");
        let (ok, detail) = run_command(d.path(), ok_cmd);
        assert!(ok, "{detail}");
        assert!(detail.contains('x'));
        let (bad, _) = run_command(d.path(), bad_cmd);
        assert!(!bad);
    }

    #[test]
    fn an_unreachable_url_fails_fast() {
        let started = std::time::Instant::now();
        let (ok, detail) = check_url("http://127.0.0.1:9/probe");
        assert!(!ok, "{detail}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!check_url("not a url").0);
    }
}
