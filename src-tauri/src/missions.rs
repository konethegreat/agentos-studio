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
    pub review_note: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
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
                created_at, updated_at
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
        review_note: r.get(14)?,
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
    })
}

pub fn list(limit: usize) -> Result<Vec<Mission>> {
    let c = store::open()?;
    let mut st = c.prepare(
        "SELECT id, repo, goal, acceptance, role, agent, status, provider, model,
                budget_usd, spent_usd, task_id, reviewer, review_verdict, review_note,
                created_at, updated_at
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
    Ok(())
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
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_db<T>(f: impl FnOnce() -> T) -> T {
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("AGENTOS_STUDIO_DB", d.path().join("studio.db"));
        let out = f();
        std::env::remove_var("AGENTOS_STUDIO_DB");
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
