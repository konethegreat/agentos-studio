//! Missions: the only object allowed to authorise a merge.
//!
//! A mission is not a chat. It is a durable unit of work with an owner role, acceptance criteria,
//! a provider, a budget and a review verdict. Chats and workers are what a mission spawns.
//!
//! The status machine is deliberately small:
//!
//! ```text
//! queued -> running -> review -> done
//!                  \-> blocked -> queued
//!                         \-> failed
//! ```
//!
//! `review` is where a second agent decides, and `done` is only reachable through an approval.
//! Nothing here merges; `merge_blockers` is the policy a merger must satisfy.

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::store;

pub const ROLES: [&str; 5] = ["planner", "implementer", "reviewer", "gatekeeper", "memory"];
pub const STATUSES: [&str; 6] = ["queued", "running", "review", "blocked", "done", "failed"];

#[derive(Debug, Clone, Serialize)]
pub struct Mission {
    pub id: String,
    pub repo: String,
    pub goal: String,
    pub acceptance: String,
    pub role: String,
    pub agent: String,
    pub status: String,
    pub provider: String,
    pub model: String,
    pub budget_usd: f64,
    pub spent_usd: f64,
    pub task_id: Option<String>,
    pub reviewer: Option<String>,
    pub review_verdict: Option<String>,
    pub pr_url: Option<String>,
    pub review_note: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// `hermes verify --json` output from the mission's repository, recorded before review.
    pub verify: Option<String>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn next_id(c: &Connection) -> Result<String> {
    let n: i64 = c.query_row("SELECT COUNT(*) FROM missions", [], |r| r.get(0))?;
    Ok(format!("M-{:03}", n + 1))
}

#[allow(clippy::too_many_arguments)]
pub fn create(
    repo: &str,
    goal: &str,
    acceptance: &str,
    role: &str,
    agent: &str,
    provider: &str,
    model: &str,
    budget_usd: f64,
) -> Result<Mission> {
    if repo.trim().is_empty() || goal.trim().is_empty() {
        bail!("a mission needs a repo and a goal");
    }
    if acceptance.trim().is_empty() {
        bail!(
            "a mission needs acceptance criteria; without them the reviewer has nothing to judge"
        );
    }
    if !ROLES.contains(&role) {
        bail!("unknown role '{role}'");
    }
    if budget_usd < 0.0 {
        bail!("budget cannot be negative");
    }
    let c = store::open()?;
    let id = next_id(&c)?;
    let t = now();
    c.execute(
        "INSERT INTO missions
         (id, repo, goal, acceptance, role, agent, status, provider, model,
          budget_usd, spent_usd, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8, ?9, 0, ?10, ?10)",
        params![
            id,
            repo.trim(),
            goal.trim(),
            acceptance.trim(),
            role,
            agent,
            provider,
            model,
            budget_usd,
            t
        ],
    )?;
    get(&id)?.ok_or_else(|| anyhow::anyhow!("mission vanished after insert"))
}

pub fn get(id: &str) -> Result<Option<Mission>> {
    let c = store::open()?;
    Ok(c.query_row(
        "SELECT id, repo, goal, acceptance, role, agent, status, provider, model,
                budget_usd, spent_usd, task_id, reviewer, review_verdict, review_note,
                created_at, updated_at, pr_url, verify_json
         FROM missions WHERE id = ?1",
        params![id],
        row,
    )
    .optional()?)
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Mission> {
    Ok(Mission {
        id: r.get(0)?,
        repo: r.get(1)?,
        goal: r.get(2)?,
        acceptance: r.get(3)?,
        role: r.get(4)?,
        agent: r.get(5)?,
        status: r.get(6)?,
        provider: r.get(7)?,
        model: r.get(8)?,
        budget_usd: r.get(9)?,
        spent_usd: r.get(10)?,
        task_id: r.get(11)?,
        reviewer: r.get(12)?,
        review_verdict: r.get(13)?,
        pr_url: r.get(17)?,
        review_note: r.get(14)?,
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
        verify: r.get(18)?,
    })
}

pub fn list(limit: usize) -> Result<Vec<Mission>> {
    let c = store::open()?;
    let mut st = c.prepare(
        "SELECT id, repo, goal, acceptance, role, agent, status, provider, model,
                budget_usd, spent_usd, task_id, reviewer, review_verdict, review_note,
                created_at, updated_at, pr_url, verify_json
         FROM missions
         ORDER BY CASE status WHEN 'review' THEN 0 WHEN 'running' THEN 1 WHEN 'queued' THEN 2
                              WHEN 'blocked' THEN 3 ELSE 4 END, updated_at DESC
         LIMIT ?1",
    )?;
    let rows = st.query_map(params![limit as i64], row)?;
    Ok(rows.flatten().collect())
}

fn set_status(c: &Connection, id: &str, from: &[&str], to: &str) -> Result<()> {
    let m = get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    if !from.contains(&m.status.as_str()) {
        bail!("{id} is {}, cannot move to {to}", m.status);
    }
    c.execute(
        "UPDATE missions SET status = ?1, updated_at = ?2 WHERE id = ?3",
        params![to, now(), id],
    )?;
    // Unit tests use a thread-local store; the notifier's thread would read the real one.
    if matches!(to, "done" | "failed") && m.status != to && !cfg!(test) {
        // `hermes send` to the user's messaging target, when one is set; off by default.
        let title: String = m.goal.chars().take(80).collect();
        let pr = m.pr_url.clone();
        let outcome = if to == "done" { "is done" } else { "failed" };
        std::thread::spawn(move || {
            crate::hermes_surfaces::notify_mission(&title, outcome, pr.as_deref())
        });
    }
    Ok(())
}

/// Start a queued mission. The single dispatch path for the board button and the scheduler:
/// Hermes missions go on Hermes' kanban board (Hermes owns execution); every other agent gets an
/// Agent Bus task.
pub fn start(v: &crate::vault::Vault, id: &str) -> Result<Mission> {
    let m = get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    if m.status != "queued" && m.status != "blocked" {
        bail!(
            "{} is {}; only a queued or blocked mission can start",
            id,
            m.status
        );
    }
    if uses_kanban(&m) {
        let task_id = crate::kanban::create_task(&m)?;
        return dispatch(id, &task_id);
    }
    let args = crate::vault::DelegateArgs {
        title: m.goal.chars().take(90).collect(),
        assigned_to: m.agent.clone(),
        creator: "agentos-studio".into(),
        project: m.repo.clone(),
        context_files: Vec::new(),
        instructions: format!(
            "{}\n\nAcceptance criteria:\n{}\n\nRole: {}. Provider: {}. Return a summary and the PR URL.",
            m.goal, m.acceptance, m.role, m.provider
        ),
        priority: "high".into(),
    };
    let task = v.delegate_task(args)?;
    let task_id = task
        .get("id")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("the bus did not return a task id"))?;
    dispatch(id, &task_id)
}

/// Hermes missions run through `hermes kanban`; the rest through the Agent Bus. Pure.
pub fn uses_kanban(m: &Mission) -> bool {
    m.agent.trim().eq_ignore_ascii_case("hermes")
}

/// Attach the bus task that will do the work, and start the mission.
pub fn dispatch(id: &str, task_id: &str) -> Result<Mission> {
    let c = store::open()?;
    set_status(&c, id, &["queued", "blocked"], "running")?;
    c.execute(
        "UPDATE missions SET task_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![task_id, now(), id],
    )?;
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// The pull request a mission produced. Used by the reviewer and the merge gate.
pub fn set_pr(id: &str, url: &str) -> Result<()> {
    let c = store::open()?;
    c.execute(
        "UPDATE missions SET pr_url = ?1, updated_at = ?2 WHERE id = ?3",
        params![url.trim(), now(), id],
    )?;
    Ok(())
}

/// The first GitHub pull request URL in a piece of text.
pub fn pull_url(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(i) = rest.find("https://github.com/") {
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == ')' || c == '>' || c == '"')
            .unwrap_or(tail.len());
        let url = &tail[..end];
        if url.contains("/pull/") {
            return Some(url.trim_end_matches(['.', ',']).to_string());
        }
        rest = &tail[end..];
    }
    None
}

/// Record the `hermes verify --json` result for the mission's repository.
pub fn set_verify(id: &str, json: &str) -> Result<()> {
    let c = store::open()?;
    c.execute(
        "UPDATE missions SET verify_json = ?1, updated_at = ?2 WHERE id = ?3",
        params![json, now(), id],
    )?;
    Ok(())
}

/// Replace the measured spend (Hermes usage data is cumulative, so this is a total, not a delta).
pub fn set_spent(id: &str, total_usd: f64) -> Result<()> {
    let c = store::open()?;
    c.execute(
        "UPDATE missions SET spent_usd = ?1, updated_at = ?2 WHERE id = ?3",
        params![total_usd.max(0.0), now(), id],
    )?;
    Ok(())
}

/// Why a recorded `hermes verify` result blocks a merge, if it does. A repository Hermes cannot
/// detect a recipe for (`no-recipe`) is not a failure: there is nothing to run. Pure.
pub fn verify_failure(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    if v.get("ok").and_then(serde_json::Value::as_bool) != Some(false) {
        return None;
    }
    if v.get("error").and_then(serde_json::Value::as_str) == Some("no-recipe") {
        return None;
    }
    let failed = v
        .get("phases")
        .and_then(serde_json::Value::as_array)
        .and_then(|ps| {
            ps.iter()
                .find(|p| p.get("ok").and_then(serde_json::Value::as_bool) == Some(false))
        })
        .map(|p| {
            format!(
                "{} ({})",
                p.get("phase").and_then(|x| x.as_str()).unwrap_or("?"),
                p.get("command").and_then(|x| x.as_str()).unwrap_or("")
            )
        })
        .or_else(|| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| "failed".into());
    Some(format!("hermes verify failed: {failed}"))
}

/// The pull request number in a mission URL, if any.
pub fn pr_number(m: &Mission) -> Option<u64> {
    let url = m.pr_url.as_deref()?;
    url.rsplit('/').next()?.parse().ok()
}

/// The worker reported back. Nothing is approved here; the reviewer decides.
pub fn submit_for_review(id: &str, spent_usd: f64) -> Result<Mission> {
    let c = store::open()?;
    set_status(&c, id, &["running"], "review")?;
    c.execute(
        "UPDATE missions SET spent_usd = spent_usd + ?1, updated_at = ?2 WHERE id = ?3",
        params![spent_usd.max(0.0), now(), id],
    )?;
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// A different agent, on a different provider where possible, rules on the work.
pub fn review(id: &str, reviewer: &str, verdict: &str, note: &str) -> Result<Mission> {
    let m = get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    if m.status != "review" {
        bail!(
            "{} is {}; only a mission in review can be judged",
            id,
            m.status
        );
    }
    if reviewer.trim().is_empty() {
        bail!("a review needs a named reviewer");
    }
    if reviewer.trim().eq_ignore_ascii_case(m.agent.trim()) {
        bail!("the reviewer must not be the agent that did the work");
    }
    if note.trim().is_empty() {
        bail!("a review needs a note, approve or reject");
    }
    let c = store::open()?;
    match verdict {
        "approve" => {
            c.execute(
                "UPDATE missions SET status = 'done', reviewer = ?1, review_verdict = 'approve',
                    review_note = ?2, updated_at = ?3 WHERE id = ?4",
                params![reviewer.trim(), note.trim(), now(), id],
            )?;
        }
        "reject" => {
            c.execute(
                "UPDATE missions SET status = 'queued', reviewer = ?1, review_verdict = 'reject',
                    review_note = ?2, updated_at = ?3 WHERE id = ?4",
                params![reviewer.trim(), note.trim(), now(), id],
            )?;
        }
        other => bail!("verdict must be approve or reject, got '{other}'"),
    }
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// Cancel a mission that has not started work or is waiting on a human: queued or blocked to
/// failed. Running and in-review missions are stopped in Studio, where the run can be ended.
pub fn cancel(id: &str, reason: &str) -> Result<Mission> {
    let c = store::open()?;
    set_status(&c, id, &["queued", "blocked"], "failed")?;
    c.execute(
        "UPDATE missions SET review_note = ?1, updated_at = ?2 WHERE id = ?3",
        params![reason.trim(), now(), id],
    )?;
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// Blocked and failed are the two states a human has to resolve.
pub fn block(id: &str, reason: &str, failed: bool) -> Result<Mission> {
    let c = store::open()?;
    let target = if failed { "failed" } else { "blocked" };
    set_status(&c, id, &["queued", "running", "review"], target)?;
    c.execute(
        "UPDATE missions SET review_note = ?1, updated_at = ?2 WHERE id = ?3",
        params![reason.trim(), now(), id],
    )?;
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// Everything that must hold before a gatekeeper merges the pull request.
/// Returns the reasons it is not allowed, so the UI can say why.
pub fn merge_blockers(m: &Mission, ci_green: bool, allowlisted: bool) -> Vec<String> {
    let mut out = Vec::new();
    if m.status != "done" {
        out.push(format!("mission is {}", m.status));
    }
    if m.review_verdict.as_deref() != Some("approve") {
        out.push("no approving review".into());
    }
    if let Some(r) = m.reviewer.as_deref() {
        if r.eq_ignore_ascii_case(m.agent.as_str()) {
            out.push("reviewer is the implementer".into());
        }
    } else {
        out.push("no reviewer".into());
    }
    if !ci_green {
        out.push("CI is not green".into());
    }
    if m.budget_usd > 0.0 && m.spent_usd > m.budget_usd {
        out.push(format!("spent ${:.2} of ${:.2}", m.spent_usd, m.budget_usd));
    }
    if !allowlisted {
        out.push("repository is not on the auto-merge allow list".into());
    }
    if let Some(reason) = m.verify.as_deref().and_then(verify_failure) {
        out.push(reason);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_db<T>(f: impl FnOnce() -> T) -> T {
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let out = f();
        crate::store::set_test_db(None);
        out
    }

    fn make() -> Mission {
        create(
            "agentos-studio",
            "Add the missions board",
            "Board lists missions with status and review verdict",
            "implementer",
            "hermes",
            "claude-subscription",
            "",
            0.0,
        )
        .unwrap()
    }

    #[test]
    fn a_failed_hermes_verify_blocks_merge_and_no_recipe_does_not() {
        assert_eq!(
            verify_failure(r#"{"ok": false, "error": "no-recipe"}"#),
            None
        );
        assert_eq!(verify_failure(r#"{"ok": true, "phases": []}"#), None);
        let failed = r#"{"ok": false, "phases": [{"phase": "build", "command": "npm run build", "ok": true},
            {"phase": "test", "command": "npm test", "ok": false}]}"#;
        assert_eq!(
            verify_failure(failed).as_deref(),
            Some("hermes verify failed: test (npm test)")
        );
        with_db(|| {
            let mut m = make();
            m.verify = Some(failed.into());
            let b = merge_blockers(&m, true, true);
            assert!(
                b.iter().any(|r| r.starts_with("hermes verify failed")),
                "{b:?}"
            );
        });
    }

    #[test]
    fn a_mission_needs_acceptance_criteria() {
        with_db(|| {
            assert!(create("r", "g", "  ", "implementer", "hermes", "x", "", 0.0).is_err());
            assert!(create("r", "g", "ok", "wizard", "hermes", "x", "", 0.0).is_err());
            assert!(create("", "g", "ok", "implementer", "hermes", "x", "", 0.0).is_err());
        });
    }

    #[test]
    fn only_an_approved_review_reaches_done() {
        with_db(|| {
            let m = make();
            assert_eq!(m.status, "queued");
            let m = dispatch(&m.id, "TASK-001").unwrap();
            assert_eq!(m.status, "running");
            let m = submit_for_review(&m.id, 0.0).unwrap();
            assert_eq!(m.status, "review");

            // The implementer cannot approve its own work.
            assert!(review(&m.id, "hermes", "approve", "looks fine").is_err());
            // A bare verdict with no note is refused.
            assert!(review(&m.id, "claude-code", "approve", "  ").is_err());

            let done = review(&m.id, "claude-code", "approve", "diff matches acceptance").unwrap();
            assert_eq!(done.status, "done");
            assert_eq!(done.review_verdict.as_deref(), Some("approve"));
        });
    }

    #[test]
    fn rejection_returns_to_the_queue_with_the_note() {
        with_db(|| {
            let m = dispatch(&make().id, "TASK-002").unwrap();
            let m = submit_for_review(&m.id, 1.5).unwrap();
            let m = review(
                &m.id,
                "qoder",
                "reject",
                "tests do not cover the empty case",
            )
            .unwrap();
            assert_eq!(m.status, "queued");
            assert_eq!(m.review_verdict.as_deref(), Some("reject"));
            assert!(m.review_note.unwrap().contains("empty case"));
            assert_eq!(m.spent_usd, 1.5);
        });
    }

    #[test]
    fn merge_policy_names_every_reason() {
        with_db(|| {
            let m = dispatch(&make().id, "TASK-003").unwrap();
            let m = submit_for_review(&m.id, 0.0).unwrap();
            let blockers = merge_blockers(&m, true, true);
            assert!(blockers.iter().any(|b| b.contains("mission is review")));

            let done = review(&m.id, "claude-code", "approve", "ok").unwrap();
            assert!(merge_blockers(&done, true, true).is_empty());
            let blocked = merge_blockers(&done, false, false);
            assert_eq!(blocked.len(), 2, "CI and allow list: {blocked:?}");
        });
    }
}
