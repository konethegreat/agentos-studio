//! Controlled reopening: failed goals go back into the queue a few at a time.
//!
//! A failed goal does not come back by itself. One automatic round per goal is allowed, one goal
//! per scheduler pass, one goal per repository, and only while nothing else is queued: the fleet
//! never holds more than the work it is already doing, and the dispatcher's own concurrency cap
//! and per-repository slot do the rest. A goal with more failed missions than the pass may reopen
//! waits for the owner's Reopen click, which queues every failed mission at once. Opening a goal
//! is refused while any of its missions is running or in review.
//!
//! Reopening forgets the previous round of each mission (its pull request, reviewer, merge
//! outcome, holds, rejects, heal state, no-PR count and the questions that stopped it) but keeps
//! the money spent, so the goal's budget stays honest. `AGENTOS_REOPEN_BUDGET`,
//! `<goals>/<missions>`, sets the cap; the default is `1/6`.

use anyhow::Result;
use serde::Serialize;

use crate::goals::{self, Goal};
use crate::missions::{self, Mission};
use crate::store;
use crate::vault::Vault;

pub const ENV_BUDGET: &str = "AGENTOS_REOPEN_BUDGET";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    pub goals: usize,
    pub missions: usize,
}

pub const DEFAULT_BUDGET: Budget = Budget {
    goals: 1,
    missions: 6,
};

/// `<goals>/<missions>`, clamped so a typo or a zero can never leave reopening off or flood the
/// queue. Anything unparsable is the default. Pure.
pub fn parse_budget(raw: &str) -> Budget {
    let mut parts = raw.split('/');
    let goals = parts.next().and_then(|s| s.trim().parse::<usize>().ok());
    let missions = parts.next().and_then(|s| s.trim().parse::<usize>().ok());
    if parts.next().is_some() {
        return DEFAULT_BUDGET;
    }
    match (goals, missions) {
        (Some(g), Some(m)) => Budget {
            goals: g.clamp(1, 5),
            missions: m.clamp(1, 24),
        },
        _ => DEFAULT_BUDGET,
    }
}

pub fn budget() -> Budget {
    match std::env::var(ENV_BUDGET) {
        Ok(raw) => parse_budget(&raw),
        Err(_) => DEFAULT_BUDGET,
    }
}

fn rounds_key(id: &str) -> String {
    format!("goal.reopen.{id}")
}

/// Automatic reopening rounds this goal has taken. One is the limit; the owner's click sets it
/// too, so a goal reopened by hand is not reopened again by the scheduler.
pub fn auto_rounds(id: &str) -> u32 {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, &rounds_key(id)).ok().flatten())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Record that this goal has had its automatic round.
pub fn mark_round_taken(id: &str) {
    if let Ok(c) = store::open() {
        let n = auto_rounds(id).max(1);
        let _ = store::kv_set(&c, &rounds_key(id), &n.to_string());
    }
}

/// Why these missions cannot all be reopened automatically, in one line. `None` means every
/// mission is settled or failed and the failed ones fit the pass's mission budget. Pure.
pub fn hold_reason(ms: &[Mission], mission_budget: usize) -> Option<String> {
    if ms.is_empty() {
        return Some("it has no missions".into());
    }
    let failed = ms.iter().filter(|m| m.status == "failed").count();
    if failed == 0 {
        return Some("no mission failed".into());
    }
    if failed > mission_budget {
        return Some(format!(
            "{failed} failed missions, more than the {mission_budget} a pass reopens"
        ));
    }
    for m in ms {
        match m.status.as_str() {
            "failed" | "done" | "queued" => {}
            "running" | "review" => return Some(format!("{} is {}", m.id, m.status)),
            "blocked" => return Some(format!("{} waits on a question", m.id)),
            "paused" => return Some(format!("{} is paused", m.id)),
            other => return Some(format!("{} is {other}", m.id)),
        }
    }
    None
}

/// The failed goals one pass may reopen, at most `budget.goals` and one per repository. A goal
/// whose mission list does not pass [`hold_reason`] is skipped rather than half opened. Pure.
pub fn select(batch: &[(Goal, Vec<Mission>)], budget: Budget) -> Vec<String> {
    let mut taken: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (g, ms) in batch {
        if out.len() >= budget.goals {
            break;
        }
        if g.status != "failed" || hold_reason(ms, budget.missions).is_some() {
            continue;
        }
        let repos: Vec<String> = if g.repos.is_empty() {
            ms.iter()
                .map(|m| m.repo.to_lowercase())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect()
        } else {
            g.repos.iter().map(|r| r.to_lowercase()).collect()
        };
        if repos.iter().any(|r| taken.contains(r)) {
            continue;
        }
        taken.extend(repos);
        out.push(g.id.clone());
    }
    out
}

/// Reopen a failed goal: every failed mission is queued for a new attempt, the previous round's
/// state is forgotten, and the goal runs again. The goal's note says why and lists what it
/// queued. Returns the reopened missions.
pub fn reopen_goal(v: &Vault, id: &str, by: &str) -> Result<Vec<String>> {
    let g = goals::get(id)?.ok_or_else(|| anyhow::anyhow!("no goal '{id}'"))?;
    if g.status != "failed" {
        anyhow::bail!("{id} is {}, so there is nothing to reopen", g.status);
    }
    let all = goals::missions_of(id)?;
    if let Some(busy) = all
        .iter()
        .find(|m| m.status == "running" || m.status == "review")
    {
        anyhow::bail!(
            "{id} has {} which is {}, so reopen it when that settles",
            busy.id,
            busy.status
        );
    }
    // Running first: the scheduler must never see missions queued under a failed goal, where it
    // would stop them as orphans before they could start.
    goals::set_status(
        id,
        &["failed"],
        "running",
        "Reopening: queueing the failed work again.",
    )?;
    let mut reopened = Vec::new();
    for m in all {
        if m.status != "failed" {
            continue;
        }
        let old: String = m
            .review_note
            .as_deref()
            .unwrap_or("no reason recorded")
            .chars()
            .take(300)
            .collect();
        let pr = m
            .pr_url
            .as_deref()
            .map(|u| format!(" Previous pull request: {u}."))
            .unwrap_or_default();
        missions::reopen(
            &m.id,
            &format!("Reopened: a new attempt. The previous round ended: {old}.{pr}"),
        )?;
        let _ = missions::set_agent(&m.id, "auto");
        crate::goal_loop::clear_round(&m.id);
        crate::stuck::reset(&m.id);
        crate::review_agent::clear_auto(&m.id);
        let _ = crate::assign::set_urgent(&m.id, false);
        let _ = crate::escalation::delete_for_mission(
            &m.id,
            &["stuck", "destructive", "second_reject", "merge"],
        );
        reopened.push(m.id);
    }
    let note = if reopened.is_empty() {
        format!("Reopened by {by}: no failed mission to queue, so the goal runs its remaining work and checks.")
    } else {
        format!(
            "Reopened by {by}: {} queued for a new attempt ({}).",
            reopened.len(),
            reopened.join(", ")
        )
    };
    goals::set_note(id, &note)?;
    goals::write_note(v, id)?;
    let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
        kind: "note",
        from_agent: "agentos-studio",
        summary: &format!("{id}: {note}"),
        ..Default::default()
    });
    Ok(reopened)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    /// Goals reopened by this pass.
    pub reopened: Vec<String>,
    /// Missions queued again by this pass.
    pub missions: Vec<String>,
    pub error: Option<String>,
}

/// Reopen one failed goal when nothing is queued anywhere, so the queue grows only as fast as the
/// fleet drains it. Goals are taken oldest first, and one per repository.
pub fn step(v: &Vault) -> Report {
    let mut r = Report::default();
    if crate::pause::paused() {
        return r;
    }
    let queued = missions::list(300)
        .map(|l| l.iter().filter(|m| m.status == "queued").count())
        .unwrap_or(0);
    if queued > 0 {
        return r;
    }
    let b = budget();
    let mut batch = Vec::new();
    for g in goals::list(200).unwrap_or_default() {
        if g.status != "failed" || auto_rounds(&g.id) > 0 {
            continue;
        }
        match goals::missions_of(&g.id) {
            Ok(ms) => batch.push((g, ms)),
            Err(e) => r.error = Some(e.to_string()),
        }
    }
    batch.sort_by_key(|(g, _)| (g.created_at, g.id.clone()));
    for id in select(&batch, b) {
        match reopen_goal(v, &id, "Studio on its automatic round") {
            Ok(ms) => {
                mark_round_taken(&id);
                r.reopened.push(id);
                r.missions.extend(ms);
            }
            Err(e) => r.error = Some(format!("{id}: {e:#}")),
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission(id: &str, repo: &str, status: &str) -> Mission {
        Mission {
            id: id.into(),
            repo: repo.into(),
            goal: "do it".into(),
            acceptance: "done".into(),
            role: "implementer".into(),
            agent: "claude-code".into(),
            status: status.into(),
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
            updated_at: 0,
            verify: None,
        }
    }

    fn goal(id: &str, repos: &[&str], created_at: i64) -> Goal {
        Goal {
            id: id.into(),
            outcome: "o".into(),
            repos: repos.iter().map(|r| r.to_string()).collect(),
            checks: Vec::new(),
            budget_usd: 0.0,
            deadline: None,
            autonomy: "ask".into(),
            status: "failed".into(),
            note: String::new(),
            report: None,
            created_at,
            updated_at: created_at,
        }
    }

    #[test]
    fn the_budget_is_goals_over_missions_with_a_floor_and_a_ceiling() {
        assert_eq!(
            parse_budget("2/8"),
            Budget {
                goals: 2,
                missions: 8
            }
        );
        assert_eq!(
            parse_budget("0/0"),
            Budget {
                goals: 1,
                missions: 1
            }
        );
        assert_eq!(
            parse_budget("9/99"),
            Budget {
                goals: 5,
                missions: 24
            }
        );
        assert_eq!(parse_budget("junk"), DEFAULT_BUDGET);
        assert_eq!(parse_budget("2/8/1"), DEFAULT_BUDGET);
    }

    #[test]
    fn a_goal_is_reopened_whole_or_left_alone() {
        let b = 6;
        assert!(hold_reason(&[], b).is_some(), "nothing planned");
        assert!(hold_reason(&[mission("M-1", "r", "done")], b).is_some());
        assert!(hold_reason(
            &[mission("M-1", "r", "failed"), mission("M-2", "r", "done")],
            b
        )
        .is_none());
        let why = hold_reason(
            &[
                mission("M-1", "r", "failed"),
                mission("M-2", "r", "running"),
            ],
            b,
        )
        .unwrap();
        assert!(why.contains("M-2 is running"), "{why}");
        assert!(hold_reason(&[mission("M-1", "r", "blocked")], b).is_some());
        let seven: Vec<Mission> = (1..=7)
            .map(|n| mission(&format!("M-{n}"), "r", "failed"))
            .collect();
        let why = hold_reason(&seven, b).unwrap();
        assert!(why.contains("7 failed missions"), "{why}");
    }

    #[test]
    fn a_pass_takes_one_goal_per_repository_and_stops_at_its_goal_budget() {
        let batch = vec![
            (
                goal("G-001", &["a"], 1),
                vec![mission("M-1", "a", "failed")],
            ),
            (
                goal("G-002", &["a"], 2),
                vec![mission("M-2", "a", "failed")],
            ),
            (
                goal("G-003", &["b", "c"], 3),
                vec![mission("M-3", "b", "failed")],
            ),
            (
                goal("G-004", &["c"], 4),
                vec![mission("M-4", "c", "failed")],
            ),
            (
                goal("G-005", &["d"], 5),
                vec![mission("M-5", "d", "failed")],
            ),
        ];
        assert_eq!(
            select(
                &batch,
                Budget {
                    goals: 2,
                    missions: 6
                }
            ),
            vec!["G-001".to_string(), "G-003".to_string()]
        );
        assert_eq!(
            select(
                &batch,
                Budget {
                    goals: 1,
                    missions: 6
                }
            ),
            vec!["G-001".to_string()]
        );
    }

    fn with_db<T>(f: impl FnOnce() -> T) -> T {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        store::set_test_db(Some(d.path().join("studio.db")));
        let out = f();
        store::set_test_db(None);
        out
    }

    fn goal_with(status: &str) -> (tempfile::TempDir, Vault, Goal) {
        let d = tempfile::tempdir().unwrap();
        let v = Vault::new(d.path());
        let g = goals::create(&goals::NewGoal {
            outcome: "Finish".into(),
            repos: vec!["agentos-sandbox-1".into()],
            checks: vec![goals::Check {
                kind: "command".into(),
                target: "npm test".into(),
                repo: None,
            }],
            budget_usd: 0.0,
            deadline: None,
            autonomy: Some("ask".into()),
        })
        .unwrap();
        let g = goals::set_status(&g.id, &["planning"], status, "for the test").unwrap();
        (d, v, g)
    }

    fn mission_in(g: &Goal, repo: &str) -> Mission {
        let m = missions::create(
            repo,
            &g.id,
            "accept",
            "implementer",
            "claude-code",
            "",
            "",
            0.0,
        )
        .unwrap();
        goals::attach(&g.id, &m.id).unwrap();
        m
    }

    #[test]
    fn reopening_queues_the_failed_missions_and_forgets_the_previous_round() {
        with_db(|| {
            let (_d, v, g) = goal_with("running");
            let m = mission_in(&g, "agentos-sandbox-1");
            let c = store::open().unwrap();
            c.execute(
                "UPDATE missions SET status = 'failed', pr_url = 'https://github.com/o/r/pull/9',
                        reviewer = 'antigravity', review_verdict = 'approve', task_id = 't_1',
                        review_note = 'Stopped: E-3 stop (answered from orchestrator)', spent_usd = 1.5
                 WHERE id = ?1",
                rusqlite::params![m.id],
            )
            .unwrap();
            store::kv_set(&c, &crate::goal_loop::merge_key(&m.id), "skipped").unwrap();
            store::kv_set(&c, &crate::goal_loop::merge_fail_key(&m.id), "1:1:no").unwrap();
            crate::goal_loop::count_reject(&m.id);
            crate::goal_loop::count_reject(&m.id);
            let (e, _) = crate::escalation::raise(
                Some(&g.id),
                Some(&m.id),
                "stuck",
                "Retry?",
                &["retry", "stop"],
                "stop",
            )
            .unwrap();
            let done = mission_in(&g, "agentos-sandbox-1");
            missions::block(&done.id, "already done", false).unwrap();
            store::kv_set(&c, &crate::goal_loop::merge_key(&done.id), "merged:1").unwrap();
            goals::set_status(&g.id, &["running"], "failed", "Stopped: E-3 stop").unwrap();

            let reopened = reopen_goal(&v, &g.id, "you").unwrap();
            assert_eq!(reopened, vec![m.id.clone()]);
            let after = missions::get(&m.id).unwrap().unwrap();
            assert_eq!(after.status, "queued");
            assert_eq!(after.agent, "auto");
            assert!(after.pr_url.is_none() && after.reviewer.is_none());
            assert!(after.review_verdict.is_none() && after.task_id.is_none());
            assert_eq!(after.spent_usd, 1.5, "the money was spent");
            assert!(after.review_note.as_deref().unwrap().contains("pull/9"));
            assert!(
                crate::escalation::get(&e.id).unwrap().is_none(),
                "its questions are gone"
            );
            assert!(crate::goal_loop::merge_outcome(&m.id).is_none());
            assert_eq!(crate::goal_loop::rejects(&m.id), 0);
            assert!(!crate::goal_loop::held(&m.id));
            assert_eq!(goals::get(&g.id).unwrap().unwrap().status, "running");
            let note = goals::get(&g.id).unwrap().unwrap().note;
            assert!(
                note.contains("Reopened by you") && note.contains(&m.id),
                "{note}"
            );
            assert_eq!(
                missions::get(&done.id).unwrap().unwrap().status,
                "blocked",
                "settled and blocked missions keep their state"
            );
        })
    }

    #[test]
    fn reopening_waits_for_a_running_or_reviewed_mission() {
        with_db(|| {
            let (_d, v, g) = goal_with("running");
            let m = mission_in(&g, "agentos-sandbox-1");
            let c = store::open().unwrap();
            c.execute(
                "UPDATE missions SET status = 'running' WHERE id = ?1",
                rusqlite::params![m.id],
            )
            .unwrap();
            goals::set_status(&g.id, &["running"], "failed", "Stopped: E-3 stop").unwrap();

            let err = reopen_goal(&v, &g.id, "you").unwrap_err();
            assert!(format!("{err:#}").contains("is running"), "{err:#}");
            assert_eq!(goals::get(&g.id).unwrap().unwrap().status, "failed");
            assert_eq!(missions::get(&m.id).unwrap().unwrap().status, "running");
        })
    }

    #[test]
    fn an_automatic_pass_waits_for_an_empty_queue_and_one_round_per_goal() {
        with_db(|| {
            let (_d, v, a) = goal_with("running");
            let ma = mission_in(&a, "agentos-sandbox-1");
            missions::cancel(&ma.id, "Stopped").unwrap();
            goals::set_status(&a.id, &["running"], "failed", "Stopped").unwrap();
            let (_d2, _v2, b) = goal_with("running");
            let mb = mission_in(&b, "agentos-sandbox-2");
            missions::cancel(&mb.id, "Stopped").unwrap();
            goals::set_status(&b.id, &["running"], "failed", "Stopped").unwrap();

            // Work is already queued: nothing opens.
            let busy = mission_in(&b, "agentos-sandbox-2");
            assert_eq!(busy.status, "queued");
            assert!(step(&v).reopened.is_empty());

            missions::cancel(&busy.id, "test").unwrap();
            let first = step(&v);
            assert_eq!(first.reopened.len(), 1, "{first:?}");
            let opened = first.reopened[0].clone();
            assert_eq!(first.missions.len(), 1);
            assert_eq!(auto_rounds(&opened), 1);
            assert_eq!(
                missions::list(10)
                    .unwrap()
                    .iter()
                    .filter(|m| m.status == "queued")
                    .count(),
                1,
                "the reopened mission is in the queue and stops a second open"
            );

            // The opened goal fails again; its automatic round is gone, so the other one goes.
            for m in goals::missions_of(&opened).unwrap() {
                missions::cancel(&m.id, "failed again").unwrap();
            }
            goals::set_status(&opened, &["running"], "failed", "failed again").unwrap();
            let second = step(&v);
            assert_eq!(second.reopened.len(), 1, "{second:?}");
            assert_ne!(second.reopened[0], opened);
        })
    }
}
