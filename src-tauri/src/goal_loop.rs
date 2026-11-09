//! The goal loop: what each scheduler pass does for work the owner is not clicking through.
//!
//! - Approved work is merged when the autonomy level allows it: Full always, Ask on risk on the
//!   merge allow list, Watch never (the owner clicks). Every other gate condition still applies
//!   (`review_agent::merge` recomputes them): another agent approved, CI green with checks
//!   present, budget intact, `hermes verify` clean.
//! - The escalation list is raised here: a merge outside the allow list, a second reject, a goal
//!   mission blocked after its retry (a destructive approval when the reason says so), a goal
//!   over its metered budget.
//! - Answers are applied once (kv `escalation.applied.<id>`), including the safe answer taken at a
//!   deadline.
//!
//! Standalone missions only get the merge step, and only while Automations are on.

use anyhow::Result;
use serde::Serialize;

use crate::escalation::{self, Escalation};
use crate::goals::{self, Goal};
use crate::missions::{self, Mission};
use crate::store;
use crate::vault::Vault;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Pass {
    pub merged: Vec<String>,
    pub escalated: Vec<String>,
    pub applied: Vec<String>,
    pub errors: Vec<String>,
}

fn kv_get(key: &str) -> Option<String> {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, key).ok().flatten())
}

fn kv_set(key: &str, value: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, key, value);
    }
}

fn kv_del(key: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_delete(&c, key);
    }
}

/// What happened to a mission's pull request after approval: `merged:<epoch>`, `skipped`
/// (the owner or a deadline said no) or `closed`.
pub fn merge_key(id: &str) -> String {
    format!("mission.merged.{id}")
}

pub fn merge_outcome(id: &str) -> Option<String> {
    kv_get(&merge_key(id))
}

fn hold_key(id: &str) -> String {
    format!("mission.hold.{id}")
}

/// A goal mission held until the owner answers an escalation about it.
pub fn held(id: &str) -> bool {
    kv_get(&hold_key(id)).is_some()
}

fn rejects_key(id: &str) -> String {
    format!("mission.rejects.{id}")
}

/// Rejections of this mission so far (kv `mission.rejects.<id>`, counted by `missions::review`).
pub fn rejects(id: &str) -> u32 {
    kv_get(&rejects_key(id))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

pub fn count_reject(id: &str) {
    kv_set(&rejects_key(id), &(rejects(id) + 1).to_string());
}

fn applied_key(id: &str) -> String {
    format!("escalation.applied.{id}")
}

/// Is a blocked mission's reason a destructive approval rather than ordinary stuck work? Pure.
pub fn is_destructive(reason: &str) -> bool {
    let r = reason.to_lowercase();
    r.contains("approval") || r.contains("destructive") || r.contains("dangerous")
}

/// One pass. Never fails as a whole; each step's error is recorded and the rest still run.
pub fn pass(v: &Vault, automation_on: bool) -> Pass {
    let mut p = Pass::default();
    let now = store::now();
    if let Err(e) = escalation::expire_due(now) {
        p.errors.push(format!("expiring escalations: {e:#}"));
    }
    match escalation::list(500) {
        Ok(all) => {
            for e in all.iter().filter(|e| e.status != "open") {
                if kv_get(&applied_key(&e.id)).is_some() {
                    continue;
                }
                match apply(v, e) {
                    Ok(()) => {
                        kv_set(&applied_key(&e.id), &now.to_string());
                        p.applied.push(e.id.clone());
                    }
                    Err(err) => p.errors.push(format!("{}: {err:#}", e.id)),
                }
            }
        }
        Err(e) => p.errors.push(format!("listing escalations: {e:#}")),
    }

    let goals_running: Vec<Goal> = goals::list(200)
        .unwrap_or_default()
        .into_iter()
        .filter(|g| g.status == "running")
        .collect();
    for g in &goals_running {
        if let Err(e) = budget_step(g, &mut p) {
            p.errors.push(format!("{}: {e:#}", g.id));
        }
    }

    let all = missions::list(300).unwrap_or_default();
    for m in &all {
        let goal = goals::goal_of(&m.id).and_then(|gid| goals::get(&gid).ok().flatten());
        let r = match (m.status.as_str(), goal.as_ref()) {
            ("done", Some(g)) => merge_step(m, Some(g), &mut p),
            ("done", None) if automation_on => merge_step(m, None, &mut p),
            ("queued", Some(g)) => reject_step(m, g, &mut p),
            ("blocked", Some(g)) => blocked_step(m, g, &mut p),
            _ => Ok(()),
        };
        if let Err(e) = r {
            p.errors.push(format!("{}: {e:#}", m.id));
        }
    }
    p
}

fn budget_step(g: &Goal, p: &mut Pass) -> Result<()> {
    if g.budget_usd <= 0.0 {
        return Ok(());
    }
    let spent = goals::spent(&goals::missions_of(&g.id)?);
    if spent <= g.budget_usd {
        return Ok(());
    }
    goals::set_status(
        &g.id,
        &["running"],
        "paused",
        &format!(
            "Paused: metered spend ${spent:.2} passed the ${:.2} budget. Work already running finishes.",
            g.budget_usd
        ),
    )?;
    let e = escalation::raise_and_send(
        Some(&g.id),
        None,
        "budget",
        &format!(
            "{} spent ${spent:.2} of its ${:.2} metered budget. Raise the budget by the same amount and continue, or stop the goal?",
            g.id, g.budget_usd
        ),
        &["raise", "stop"],
        "stop",
    )?;
    p.escalated.push(e.id);
    Ok(())
}

fn merge_step(m: &Mission, goal: Option<&Goal>, p: &mut Pass) -> Result<()> {
    if m.review_verdict.as_deref() != Some("approve") || m.pr_url.is_none() {
        return Ok(());
    }
    if merge_outcome(&m.id).is_some() {
        return Ok(());
    }
    let level = goal.map(|g| g.autonomy.as_str()).unwrap_or("ask");
    let allowlisted = crate::review_agent::project_dir(&m.repo)
        .map(|d| crate::routing::merge_allowed(&d))
        .unwrap_or(false);
    let gid = goal.map(|g| g.id.as_str());
    match goals::merge_decision(level, allowlisted) {
        goals::MergeDecision::Wait => Ok(()),
        goals::MergeDecision::Merge => try_merge(m, p),
        goals::MergeDecision::Ask => {
            if let Some(e) = escalation::latest_decided(gid, Some(&m.id), "merge") {
                return match e.answer.as_deref() {
                    Some("merge") => try_merge(m, p),
                    _ => {
                        kv_set(&merge_key(&m.id), "skipped");
                        Ok(())
                    }
                };
            }
            // Standalone missions keep the board's Merge button; only goals ask.
            if goal.is_none() {
                return Ok(());
            }
            let e = escalation::raise_and_send(
                gid,
                Some(&m.id),
                "merge",
                &format!(
                    "{} was approved by {}, but {} is not on the merge allow list. Merge {}?",
                    m.id,
                    m.reviewer.as_deref().unwrap_or("the reviewer"),
                    m.repo,
                    m.pr_url.as_deref().unwrap_or("the pull request")
                ),
                &["merge", "skip"],
                "skip",
            )?;
            if !p.escalated.contains(&e.id) {
                p.escalated.push(e.id);
            }
            Ok(())
        }
    }
}

/// Merge once CI is green. Pending CI waits for a later pass; a pull request merged or closed by
/// someone else is recorded and left alone.
fn try_merge(m: &Mission, p: &mut Pass) -> Result<()> {
    let number = missions::pr_number(m).ok_or_else(|| anyhow::anyhow!("no pull request"))?;
    let repo = crate::github::name_with_owner(&m.repo)?;
    match crate::github::pr_status(&repo, number)?.as_str() {
        "MERGED" => {
            kv_set(&merge_key(&m.id), &format!("merged:{}", store::now()));
            return Ok(());
        }
        "CLOSED" => {
            kv_set(&merge_key(&m.id), "closed");
            return Ok(());
        }
        _ => {}
    }
    let runs = crate::github::pr_check_runs(&repo, number)?;
    if crate::github::ci_state(&runs) != crate::github::Ci::Green {
        return Ok(());
    }
    crate::review_agent::merge(&m.id, true)?;
    kv_set(&merge_key(&m.id), &format!("merged:{}", store::now()));
    let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
        mission_id: Some(&m.id),
        kind: "note",
        from_agent: "agentos-studio",
        summary: &format!(
            "merged {} on its own: approved, CI green",
            repo_pr(&repo, number)
        ),
        ..Default::default()
    });
    p.merged.push(m.id.clone());
    Ok(())
}

fn repo_pr(repo: &str, n: u64) -> String {
    format!("{repo}#{n}")
}

fn reject_step(m: &Mission, g: &Goal, p: &mut Pass) -> Result<()> {
    if rejects(&m.id) < 2 || held(&m.id) {
        return Ok(());
    }
    if escalation::find_open(Some(&g.id), Some(&m.id), "second_reject").is_some() {
        return Ok(());
    }
    kv_set(&hold_key(&m.id), "second_reject");
    let e = escalation::raise_and_send(
        Some(&g.id),
        Some(&m.id),
        "second_reject",
        &format!(
            "{} was rejected {} times. Last note from {}: {}. Try again, or stop it?",
            m.id,
            rejects(&m.id),
            m.reviewer.as_deref().unwrap_or("the reviewer"),
            m.review_note.as_deref().unwrap_or("none")
        ),
        &["retry", "stop"],
        "stop",
    )?;
    p.escalated.push(e.id);
    Ok(())
}

fn blocked_step(m: &Mission, g: &Goal, p: &mut Pass) -> Result<()> {
    let reason = m.review_note.as_deref().unwrap_or("no reason recorded");
    let kind = if is_destructive(reason) {
        "destructive"
    } else {
        "stuck"
    };
    if escalation::find_open(Some(&g.id), Some(&m.id), kind).is_some() {
        return Ok(());
    }
    let question = if kind == "destructive" {
        format!(
            "{} stopped for a destructive approval: {reason}. Retry it, or stop it?",
            m.id
        )
    } else {
        format!(
            "{} is blocked after its retry: {reason}. Retry it, or stop it?",
            m.id
        )
    };
    let e = escalation::raise_and_send(
        Some(&g.id),
        Some(&m.id),
        kind,
        &question,
        &["retry", "stop"],
        "stop",
    )?;
    if !p.escalated.contains(&e.id) {
        p.escalated.push(e.id);
    }
    Ok(())
}

fn fail_goal(v: &Vault, goal_id: &str, why: &str) -> Result<()> {
    for m in goals::missions_of(goal_id)? {
        if m.status == "queued" || m.status == "blocked" {
            let _ = missions::cancel(&m.id, why);
        }
    }
    let _ = goals::set_status(
        goal_id,
        &["planning", "awaiting", "running", "verifying", "paused"],
        "failed",
        why,
    );
    let _ = goals::write_note(v, goal_id);
    Ok(())
}

/// Carry out a decided escalation.
fn apply(v: &Vault, e: &Escalation) -> Result<()> {
    let answer = e.answer.as_deref().unwrap_or(e.safe_answer.as_str());
    let by = match e.answered_by.as_deref() {
        Some("timeout") => "no answer by the deadline".to_string(),
        Some(b) => format!("answered from {b}"),
        None => "answered".into(),
    };
    let why = format!("{} {} ({by})", e.id, answer);
    match (e.kind.as_str(), answer) {
        // The merge step reads the answer itself on its next pass.
        ("merge", _) => Ok(()),
        ("second_reject", "retry") => {
            if let Some(m) = e.mission_id.as_deref() {
                kv_del(&hold_key(m));
                // The next reject escalates again.
                kv_set(&rejects_key(m), "1");
            }
            Ok(())
        }
        ("stuck" | "destructive", "retry") => {
            if let Some(m) = e.mission_id.as_deref() {
                missions::unblock(m, &format!("Retry: {why}"))?;
                crate::stuck::reset(m);
                crate::review_agent::clear_auto(m);
            }
            Ok(())
        }
        ("budget", "raise") => {
            if let Some(gid) = e.goal_id.as_deref() {
                let g = goals::get(gid)?.ok_or_else(|| anyhow::anyhow!("no goal '{gid}'"))?;
                goals::set_budget(gid, g.budget_usd * 2.0)?;
                goals::set_status(
                    gid,
                    &["paused"],
                    "running",
                    &format!("Budget raised to ${:.2}: {why}", g.budget_usd * 2.0),
                )?;
                let _ = goals::write_note(v, gid);
            }
            Ok(())
        }
        (_, "stop") => {
            if let Some(m) = e.mission_id.as_deref() {
                kv_del(&hold_key(m));
                let _ = missions::cancel(m, &format!("Stopped: {why}"));
            }
            if let Some(gid) = e.goal_id.as_deref() {
                fail_goal(v, gid, &format!("Stopped: {why}"))?;
            }
            Ok(())
        }
        // done_check answers are applied by the completion step.
        ("done_check", _) => Ok(()),
        (k, a) => anyhow::bail!("no action for answer '{a}' to a {k} escalation"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_destructive_approval_is_told_apart_from_stuck_work() {
        assert!(is_destructive(
            "Hermes kanban: waiting for approval of rm -rf build"
        ));
        assert!(is_destructive("dangerous command refused"));
        assert!(!is_destructive("quiet for 15 minutes after one retry"));
    }

    #[test]
    fn rejects_are_counted_and_a_second_one_holds_the_mission() {
        let d = tempfile::tempdir().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        let vdir = tempfile::tempdir().unwrap();
        let v = Vault::new(vdir.path());
        let g = goals::create(&goals::NewGoal {
            outcome: "o".into(),
            repos: vec!["agentos-sandbox-1".into()],
            checks: vec![goals::Check {
                kind: "file".into(),
                target: "README.md".into(),
                repo: None,
            }],
            budget_usd: 0.0,
            deadline: None,
            autonomy: None,
        })
        .unwrap();
        goals::set_status(&g.id, &["planning"], "running", "").unwrap();
        let g = goals::get(&g.id).unwrap().unwrap();
        let m = missions::create(
            "agentos-sandbox-1",
            "x",
            "y",
            "implementer",
            "claude-code",
            "",
            "",
            0.0,
        )
        .unwrap();
        goals::attach(&g.id, &m.id).unwrap();
        let mut p = Pass::default();
        count_reject(&m.id);
        reject_step(&m, &g, &mut p).unwrap();
        assert!(
            p.escalated.is_empty() && !held(&m.id),
            "one reject is ordinary rework"
        );
        count_reject(&m.id);
        reject_step(&m, &g, &mut p).unwrap();
        assert_eq!(p.escalated, vec!["E-1".to_string()]);
        assert!(held(&m.id));
        let e = escalation::answer("E-1", "retry", "studio").unwrap();
        apply(&v, &e).unwrap();
        assert!(!held(&m.id));
        assert_eq!(rejects(&m.id), 1, "the next reject asks again");

        // A stop fails the mission and the goal, and says why.
        count_reject(&m.id);
        reject_step(&m, &g, &mut p).unwrap();
        let e2 = escalation::find_open(Some(&g.id), Some(&m.id), "second_reject").unwrap();
        let e2 = escalation::answer(&e2.id, "stop", "studio").unwrap();
        apply(&v, &e2).unwrap();
        assert_eq!(missions::get(&m.id).unwrap().unwrap().status, "failed");
        let g = goals::get(&g.id).unwrap().unwrap();
        assert_eq!(g.status, "failed");
        assert!(
            g.note.contains("E-2 stop (answered from studio)"),
            "{}",
            g.note
        );
        store::set_test_db(None);
    }

    #[test]
    fn a_goal_over_budget_pauses_and_asks_and_a_raise_doubles_it() {
        let d = tempfile::tempdir().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        let vdir = tempfile::tempdir().unwrap();
        let v = Vault::new(vdir.path());
        let g = goals::create(&goals::NewGoal {
            outcome: "o".into(),
            repos: vec!["agentos-sandbox-1".into()],
            checks: vec![goals::Check {
                kind: "file".into(),
                target: "README.md".into(),
                repo: None,
            }],
            budget_usd: 1.0,
            deadline: None,
            autonomy: None,
        })
        .unwrap();
        goals::set_status(&g.id, &["planning"], "running", "").unwrap();
        let m = missions::create(
            "agentos-sandbox-1",
            "x",
            "y",
            "implementer",
            "hermes",
            "",
            "",
            1.0,
        )
        .unwrap();
        goals::attach(&g.id, &m.id).unwrap();
        let mut p = Pass::default();
        budget_step(&goals::get(&g.id).unwrap().unwrap(), &mut p).unwrap();
        assert!(p.escalated.is_empty(), "within budget");
        missions::set_spent(&m.id, 1.5).unwrap();
        budget_step(&goals::get(&g.id).unwrap().unwrap(), &mut p).unwrap();
        assert_eq!(goals::get(&g.id).unwrap().unwrap().status, "paused");
        let e = escalation::answer(&p.escalated[0], "raise", "phone").unwrap();
        apply(&v, &e).unwrap();
        let g = goals::get(&g.id).unwrap().unwrap();
        assert_eq!((g.status.as_str(), g.budget_usd), ("running", 2.0));
        store::set_test_db(None);
    }
}
