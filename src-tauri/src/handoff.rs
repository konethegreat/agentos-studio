//! Handoff packets: one agent passes a running mission to another, and the receiver never starts
//! cold.
//!
//! The `handoff` hub tool calls `handoff()`. Studio assembles the packet itself instead of trusting
//! the sender to remember everything: the project brief, the mission's goal and acceptance
//! criteria, the diff so far in the mission's worktree, failing pull request checks and the
//! relevant `recall` results, plus the sender's summary, next step, files and open questions. The
//! packet becomes the body of a new Agent Bus task for the receiver, with the same `Repository:`,
//! `Worktree:` and `Branch:` lines, so a headless receiver is started by the bus watcher in the
//! same worktree and reads the packet as its first message. The mission moves to the receiver, the
//! sender's task is closed as handed off, and the exchange is logged for the run timeline.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::missions::{self, Mission};
use crate::vault::{DelegateArgs, Vault};

const MAX_DIFF: usize = 8_000;
const MAX_BRIEF: usize = 3_000;

/// What the sender says, as the hub tool received it.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub to: String,
    pub mission_id: String,
    pub summary: String,
    pub next_step: String,
    pub files: Vec<String>,
    pub questions: Vec<String>,
}

/// Everything the receiver gets.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Packet {
    pub from: String,
    pub to: String,
    pub mission_id: String,
    pub repo: String,
    pub goal: String,
    pub acceptance: String,
    pub summary: String,
    pub next_step: String,
    pub files: Vec<String>,
    pub questions: Vec<String>,
    pub brief: String,
    pub diff_stat: String,
    pub diff: String,
    pub failing_checks: Vec<String>,
    pub recall: Vec<String>,
    pub pr_url: Option<String>,
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push_str("\n... (truncated)");
    out
}

/// Refuse a handoff that cannot be right: unknown or same agent, a mission that is not running,
/// or a sender that does not hold the mission. Pure.
pub fn validate(from: &str, req: &Request, m: &Mission) -> Result<String> {
    let to = crate::vault::norm_agent(&req.to)?;
    if to == "human" {
        bail!("a handoff goes to an agent; ask the owner through delegate_task with assigned_to human");
    }
    if to == from {
        bail!("{from} cannot hand {} to itself", m.id);
    }
    if m.status != "running" {
        bail!(
            "{} is {}; only a running mission can be handed off",
            m.id,
            m.status
        );
    }
    let holder = m.agent.trim().to_lowercase();
    if holder != from {
        bail!(
            "{} is held by {holder}, not {from}; only the agent working on it can hand it off",
            m.id
        );
    }
    if req.summary.trim().is_empty() || req.next_step.trim().is_empty() {
        bail!("a handoff needs a summary of what is done and the next step");
    }
    Ok(to)
}

/// The packet as the receiver reads it: Markdown with the worker location lines at the end, in the
/// form `bus::workdir_from_body` reads. Pure.
pub fn render(
    p: &Packet,
    repo_dir: &Path,
    worktree: Option<&Path>,
    branch: &str,
    model: &str,
) -> String {
    let list = |items: &[String]| -> String {
        if items.is_empty() {
            "- none\n".into()
        } else {
            items.iter().map(|i| format!("- {i}\n")).collect()
        }
    };
    let mut b = format!(
        "HANDOFF from {from} to {to} for mission {id}. You are continuing work another agent started. Read this packet first; it is everything you need.\n\n\
## Goal\n{goal}\n\n## Acceptance criteria\n{acceptance}\n\n\
## What {from} did\n{summary}\n\n## Your next step\n{next}\n\n\
## Files to look at\n{files}\n## Open questions\n{questions}\n",
        from = p.from,
        to = p.to,
        id = p.mission_id,
        goal = p.goal.trim(),
        acceptance = p.acceptance.trim(),
        summary = p.summary.trim(),
        next = p.next_step.trim(),
        files = list(&p.files),
        questions = list(&p.questions),
    );
    if let Some(pr) = &p.pr_url {
        b.push_str(&format!("\n## Pull request\n{pr}\n"));
    }
    b.push_str("\n## Failing checks\n");
    b.push_str(&list(&p.failing_checks));
    b.push_str(&format!(
        "\n## Diff so far\n{}\n```diff\n{}\n```\n",
        if p.diff_stat.trim().is_empty() {
            "No changes yet."
        } else {
            p.diff_stat.trim()
        },
        p.diff.trim()
    ));
    if !p.recall.is_empty() {
        b.push_str("\n## From shared memory (recall)\n");
        b.push_str(&list(&p.recall));
    }
    if !p.brief.trim().is_empty() {
        b.push_str(&format!("\n## Project brief\n{}\n", p.brief.trim()));
    }
    b.push_str(crate::constitution_gate::WORKER_RULES.trim_start());
    b.push('\n');
    b.push_str(&format!(
        "\nMission: {}. Work in the worktree below, on its branch. Open or update the pull request with gh and put its URL in your complete_task summary. Do not merge: AgentOS Studio reviews and merges.\n\nRepository: {}",
        p.mission_id,
        repo_dir.display()
    ));
    if let Some(wt) = worktree {
        b.push_str(&format!("\nWorktree: {}\nBranch: {branch}", wt.display()));
    }
    if !model.trim().is_empty() {
        b.push_str(&format!("\nModel: {}", model.trim()));
    }
    b
}

fn git(dir: &Path, args: &[&str]) -> String {
    let Some(exe) = crate::procs::find("git") else {
        return String::new();
    };
    crate::procs::command(&exe)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// The diff of the work so far: committed on the branch since it left the default branch, plus
/// anything not committed yet.
fn diff_so_far(wt: &Path) -> (String, String) {
    let base = ["origin/main", "origin/master", "main", "master"]
        .into_iter()
        .find(|b| {
            !git(wt, &["rev-parse", "--verify", "--quiet", b])
                .trim()
                .is_empty()
        })
        .unwrap_or("HEAD");
    let range = format!("{base}...HEAD");
    let mut stat = git(wt, &["diff", "--stat", &range]);
    let mut diff = git(wt, &["diff", &range]);
    let dirty = git(wt, &["diff", "HEAD"]);
    if !dirty.trim().is_empty() {
        stat.push_str(&git(wt, &["diff", "--stat", "HEAD"]));
        diff.push_str("\n# not committed yet\n");
        diff.push_str(&dirty);
    }
    (stat, clip(&diff, MAX_DIFF))
}

/// Checks on the mission's pull request that are not passing, as `name: state`.
fn failing_checks(dir: &Path, m: &Mission) -> Vec<String> {
    let Some(n) = missions::pr_number(m) else {
        return Vec::new();
    };
    let Some(exe) = crate::procs::find("gh") else {
        return Vec::new();
    };
    let out = crate::procs::command(&exe)
        .args(["pr", "checks", &n.to_string(), "--json", "name,state"])
        .current_dir(dir)
        .output();
    let Ok(out) = out else { return Vec::new() };
    serde_json::from_slice::<Vec<Value>>(&out.stdout)
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let state = c.get("state")?.as_str()?;
            if matches!(state, "SUCCESS" | "SKIPPED" | "NEUTRAL") {
                return None;
            }
            Some(format!("{}: {state}", c.get("name")?.as_str()?))
        })
        .collect()
}

/// Assemble the packet from the mission and the machine.
pub fn assemble(
    v: &Vault,
    from: &str,
    to: &str,
    req: &Request,
    m: &Mission,
) -> (Packet, PathBuf, Option<PathBuf>) {
    let dir = crate::review_agent::project_dir(&m.repo).unwrap_or_default();
    // Studio's worktree for bus missions; for a Hermes kanban mission, the task's own workspace.
    let wt = crate::worktree::recorded(&m.id)
        .filter(|p| p.is_dir())
        .or_else(|| {
            m.task_id
                .as_deref()
                .filter(|t| crate::kanban::is_kanban(Some(t)))
                .and_then(|t| crate::kanban::show(t).ok())
                .and_then(|view| view.workspace_path)
                .map(PathBuf::from)
                .filter(|p| p.is_dir() && p.starts_with(&dir))
        });
    let (diff_stat, diff) = wt.as_deref().map(diff_so_far).unwrap_or_default();
    let recall = crate::memory_index::recall(v, &m.goal, Some(&m.repo), 4, 600)
        .map(|chunks| {
            chunks
                .into_iter()
                .map(|c| {
                    format!(
                        "{}:{} {}",
                        c.path,
                        c.line,
                        clip(&c.text.replace('\n', " "), 240)
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let packet = Packet {
        from: from.into(),
        to: to.into(),
        mission_id: m.id.clone(),
        repo: m.repo.clone(),
        goal: m.goal.clone(),
        acceptance: m.acceptance.clone(),
        summary: req.summary.trim().into(),
        next_step: req.next_step.trim().into(),
        files: req.files.clone(),
        questions: req.questions.clone(),
        brief: clip(&v.project_brief(&m.repo).unwrap_or_default(), MAX_BRIEF),
        diff_stat,
        diff,
        failing_checks: if dir.is_dir() {
            failing_checks(&dir, m)
        } else {
            Vec::new()
        },
        recall,
        pr_url: m.pr_url.clone(),
    };
    (packet, dir, wt)
}

/// Hand a running mission from `from` (the verified hub caller) to `req.to`.
pub fn handoff(v: &Vault, from: &str, req: &Request) -> Result<Value> {
    let m = missions::get(&req.mission_id)?
        .ok_or_else(|| anyhow::anyhow!("no mission '{}'", req.mission_id))?;
    let to = validate(from, req, &m)?;
    let (packet, dir, wt) = assemble(v, from, &to, req, &m);
    if !dir.is_dir() {
        bail!(
            "{}: repository '{}' is not a known project with a folder",
            m.id,
            m.repo
        );
    }
    let branch = wt
        .as_deref()
        .map(|w| {
            git(w, &["rev-parse", "--abbrev-ref", "HEAD"])
                .trim()
                .to_string()
        })
        .filter(|b| !b.is_empty() && b != "HEAD")
        .or_else(|| crate::worktree::recorded_branch(&m.id))
        .unwrap_or_else(|| crate::worktree::branch_for(&m));
    let body = render(&packet, &dir, wt.as_deref(), &branch, &m.model);
    let task = v.delegate_task(DelegateArgs {
        title: format!(
            "Handoff from {from}: {}",
            m.goal.chars().take(70).collect::<String>()
        ),
        assigned_to: to.clone(),
        creator: from.into(),
        project: m.repo.clone(),
        context_files: missions::context_files(&dir),
        instructions: body,
        priority: "high".into(),
    })?;
    let task_id = task
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("the bus did not return a task id"))?;
    let old = m.task_id.clone();
    missions::hand_to(
        &m.id,
        &to,
        &task_id,
        &format!("handed off by {from}: {}", packet.summary),
    )?;
    crate::stuck::reset(&m.id);
    // Close the sender's task: its part is done and the mission now follows the new task.
    let closing = format!(
        "Handed off to {to} as {task_id}.\n\nSummary: {}\n\nNext step: {}",
        packet.summary, packet.next_step
    );
    if let Some(old) = old.as_deref() {
        if crate::kanban::is_kanban(Some(old)) {
            let _ = crate::kanban::complete(old, &closing);
        } else if v.task_state(old) == Some(crate::vault::State::InProgress) {
            let _ = v.complete_task(old, from, &closing);
        } else if v.task_state(old) == Some(crate::vault::State::Pending) {
            let _ = v.claim_task(old, from);
            let _ = v.complete_task(old, from, &closing);
        }
    }
    let detail = serde_json::to_string(&packet).unwrap_or_default();
    crate::agent_log::record(&crate::agent_log::NewEvent {
        mission_id: Some(&m.id),
        kind: "handoff",
        from_agent: from,
        to_agent: &to,
        summary: &format!("{} Next: {}", packet.summary, packet.next_step),
        detail: &detail,
        ..Default::default()
    })?;
    Ok(json!({
        "handed_off": true,
        "mission_id": m.id,
        "to": to,
        "task_id": task_id,
        "previous_task": old,
        "packet_sections": {
            "diff_lines": packet.diff.lines().count(),
            "failing_checks": packet.failing_checks.len(),
            "recall": packet.recall.len(),
            "brief_chars": packet.brief.len(),
        },
        "note": format!("{to} now holds {}. Your task is closed as handed off: stop here and do not call complete_task.", m.id),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission(agent: &str, status: &str) -> Mission {
        Mission {
            id: "M-007".into(),
            repo: "agentos-sandbox-1".into(),
            goal: "Add a probe endpoint".into(),
            acceptance: "GET /probe returns 200 and npm test passes".into(),
            role: "implementer".into(),
            agent: agent.into(),
            status: status.into(),
            provider: String::new(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: Some("TASK-010".into()),
            reviewer: None,
            review_verdict: None,
            pr_url: None,
            review_note: None,
            created_at: 0,
            updated_at: 0,
            verify: None,
        }
    }

    fn req(to: &str) -> Request {
        Request {
            to: to.into(),
            mission_id: "M-007".into(),
            summary: "Endpoint added, tests pass".into(),
            next_step: "Write the README section".into(),
            files: vec!["src/server.js".into()],
            questions: vec!["Keep the port at 3000?".into()],
        }
    }

    #[test]
    fn only_the_holder_hands_off_a_running_mission_to_another_agent() {
        let m = mission("claude-code", "running");
        assert_eq!(
            validate("claude-code", &req("hermes"), &m).unwrap(),
            "hermes"
        );
        assert_eq!(
            validate("claude-code", &req("Hermes"), &m).unwrap(),
            "hermes"
        );
        let e = validate("antigravity", &req("hermes"), &m)
            .unwrap_err()
            .to_string();
        assert!(e.contains("held by claude-code"), "{e}");
        assert!(validate("claude-code", &req("claude-code"), &m).is_err());
        assert!(validate("claude-code", &req("human"), &m).is_err());
        assert!(validate("claude-code", &req("nobody"), &m).is_err());
        assert!(validate(
            "claude-code",
            &req("hermes"),
            &mission("claude-code", "review")
        )
        .is_err());
        let mut empty = req("hermes");
        empty.next_step = " ".into();
        assert!(validate("claude-code", &empty, &m).is_err());
    }

    #[test]
    fn the_packet_carries_everything_and_the_worker_finds_its_folder() {
        let d = tempfile::tempdir().unwrap();
        let wt = d.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let p = Packet {
            from: "claude-code".into(),
            to: "hermes".into(),
            mission_id: "M-007".into(),
            goal: "Add a probe endpoint".into(),
            acceptance: "GET /probe returns 200".into(),
            summary: "Endpoint added".into(),
            next_step: "Docs".into(),
            files: vec!["src/server.js".into()],
            questions: vec!["Port?".into()],
            brief: "# agentos-sandbox-1".into(),
            diff_stat: " src/server.js | 4 ++++".into(),
            diff: "+app.get('/probe')".into(),
            failing_checks: vec!["test: FAILURE".into()],
            recall: vec!["03_Memory/Ports.md:3 3000 is the port".into()],
            pr_url: Some("https://github.com/o/r/pull/3".into()),
            ..Default::default()
        };
        let body = render(
            &p,
            d.path(),
            Some(&wt),
            "agentos/m-007-1",
            "claude-opus-5-5",
        );
        for want in [
            "HANDOFF from claude-code to hermes",
            "GET /probe returns 200",
            "## Your next step\nDocs",
            "- src/server.js",
            "- Port?",
            "test: FAILURE",
            "+app.get('/probe')",
            "3000 is the port",
            "# agentos-sandbox-1",
            "https://github.com/o/r/pull/3",
            "Mission: M-007.",
        ] {
            assert!(body.contains(want), "missing {want:?} in\n{body}");
        }
        assert_eq!(crate::bus::workdir_from_body(&body).unwrap(), wt);
        assert_eq!(crate::bus::repo_from_body(&body).unwrap(), d.path());
        assert_eq!(
            crate::bus::model_from_body(&body).as_deref(),
            Some("claude-opus-5-5")
        );
        assert!(body.contains("Branch: agentos/m-007-1"));
    }

    #[test]
    fn a_handoff_moves_the_mission_and_closes_the_senders_task() {
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let v = Vault::new(d.path().join("vault"));
        v.scaffold().unwrap();
        let m = missions::create("r", "g", "a", "implementer", "claude-code", "", "", 0.0).unwrap();
        let t = v
            .delegate_task(DelegateArgs {
                title: "t".into(),
                assigned_to: "claude-code".into(),
                creator: "agentos-studio".into(),
                project: "r".into(),
                context_files: vec![],
                instructions: "x".into(),
                priority: "high".into(),
            })
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        missions::dispatch(&m.id, &t).unwrap();
        v.claim_task(&t, "claude-code").unwrap();
        let moved = missions::hand_to(&m.id, "hermes", "TASK-099", "handed off").unwrap();
        assert_eq!(moved.agent, "hermes");
        assert_eq!(moved.task_id.as_deref(), Some("TASK-099"));
        assert_eq!(moved.status, "running");
        crate::store::set_test_db(None);
    }
}
