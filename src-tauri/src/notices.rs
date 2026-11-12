//! The notification centre: one list of everything that needs the owner's attention, built
//! from the state Studio already keeps (escalations, goals, missions), each item linking to the
//! screen that owns it. Nothing new is stored except which items were read and which were
//! already shown as a Windows notification, both in kv, so no schema change.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::store;

/// How far back finished work stays in the list.
const WINDOW_SECS: i64 = 7 * 24 * 3600;
const READ_KEY: &str = "notices.read_until";
const SHOWN_KEY: &str = "notices.os_shown_until";
pub const OS_SWITCH_KEY: &str = "notices.os";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Notice {
    /// Stable across refreshes: kind plus row id.
    pub id: String,
    /// question, goal_done, goal_failed, mission_blocked, mission_failed, review
    pub kind: String,
    pub title: String,
    pub detail: String,
    /// The screen that owns it (an id from `VIEWS` in App.tsx).
    pub view: String,
    pub at: i64,
    pub unread: bool,
    /// Still needs an action (an open question, a mission waiting for review).
    pub open: bool,
}

fn short(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= n {
        one
    } else {
        one.chars().take(n).collect::<String>() + "..."
    }
}

/// Every notice, newest first.
pub fn list_in(c: &Connection, now: i64) -> Result<Vec<Notice>> {
    let read_until: i64 = store::kv_get(c, READ_KEY)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let since = now - WINDOW_SECS;
    let mut out = Vec::new();

    let mut q = c.prepare(
        "SELECT id, kind, question, created_at FROM escalations WHERE status = 'open' ORDER BY created_at DESC",
    )?;
    for r in q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })? {
        let (id, kind, question, at) = r?;
        out.push(Notice {
            id: format!("question.{id}"),
            kind: "question".into(),
            title: "A question waits for you".into(),
            detail: format!("{}: {}", kind.replace('_', " "), short(&question, 140)),
            view: "today".into(),
            at,
            unread: at > read_until,
            open: true,
        });
    }

    let mut q = c.prepare(
        "SELECT id, outcome, status, note, updated_at FROM goals
         WHERE status IN ('done', 'failed', 'awaiting') AND updated_at >= ?1 ORDER BY updated_at DESC",
    )?;
    for r in q.query_map(params![since], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })? {
        let (id, outcome, status, note, at) = r?;
        let (kind, title, open) = match status.as_str() {
            "done" => ("goal_done", format!("Goal {id} is finished"), false),
            "failed" => ("goal_failed", format!("Goal {id} failed"), false),
            _ => (
                "goal_plan",
                format!("Goal {id} has a plan to approve"),
                true,
            ),
        };
        out.push(Notice {
            id: format!("{kind}.{id}.{at}"),
            kind: kind.into(),
            title,
            detail: if note.trim().is_empty() {
                short(&outcome, 140)
            } else {
                short(&note, 140)
            },
            view: "goals".into(),
            at,
            unread: at > read_until,
            open,
        });
    }

    let mut q = c.prepare(
        "SELECT id, goal, status, COALESCE(review_note, ''), updated_at FROM missions
         WHERE status IN ('blocked', 'failed', 'review') AND updated_at >= ?1 ORDER BY updated_at DESC",
    )?;
    for r in q.query_map(params![since], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })? {
        let (id, goal, status, note, at) = r?;
        let (kind, title, open) = match status.as_str() {
            "blocked" => ("mission_blocked", format!("Mission {id} is blocked"), true),
            "failed" => ("mission_failed", format!("Mission {id} failed"), false),
            _ => ("review", format!("Mission {id} is ready for review"), true),
        };
        out.push(Notice {
            id: format!("{kind}.{id}.{at}"),
            kind: kind.into(),
            title,
            detail: if status == "blocked" && !note.trim().is_empty() {
                short(&note, 140)
            } else {
                short(&goal, 140)
            },
            view: "mission".into(),
            at,
            unread: at > read_until,
            open,
        });
    }
    // Subscriptions and keys Hermes is set to use that cannot run right now.
    if let Some(r) = crate::provider_health::last_in(c) {
        for h in r.problems() {
            let at = h.last_failure.as_ref().map_or(r.checked_at, |f| f.at);
            out.push(Notice {
                id: format!("connection.{}.{}", h.id, h.state),
                kind: "connection".into(),
                title: format!("{}: {}", h.label, h.state.replace('_', " ")),
                detail: h.detail.clone(),
                view: "setup".into(),
                at,
                unread: at > read_until,
                open: true,
            });
        }
    }
    out.sort_by_key(|n| std::cmp::Reverse(n.at));
    Ok(out)
}

pub fn list(now: i64) -> Result<Vec<Notice>> {
    list_in(&store::open()?, now)
}

/// Mark everything up to `now` as read.
pub fn mark_read_in(c: &Connection, now: i64) -> Result<()> {
    store::kv_set(c, READ_KEY, &now.to_string())
}

pub fn mark_read(now: i64) -> Result<()> {
    mark_read_in(&store::open()?, now)
}

/// Windows notifications are on unless the owner switched them off.
pub fn os_enabled_in(c: &Connection) -> bool {
    store::kv_get(c, OS_SWITCH_KEY).ok().flatten().as_deref() != Some("off")
}

/// Notices that should raise a Windows notification now: newer than the last one shown, and
/// only the kinds worth interrupting for. The first call after install shows nothing old: it
/// only records where it is. Marks them as shown.
pub fn take_for_os_in(c: &Connection, now: i64) -> Result<Vec<Notice>> {
    let shown: Option<i64> = store::kv_get(c, SHOWN_KEY)?.and_then(|v| v.parse().ok());
    store::kv_set(c, SHOWN_KEY, &now.to_string())?;
    let Some(shown) = shown else {
        return Ok(Vec::new());
    };
    if !os_enabled_in(c) {
        return Ok(Vec::new());
    }
    Ok(list_in(c, now)?
        .into_iter()
        .filter(|n| n.at > shown)
        .filter(|n| {
            matches!(
                n.kind.as_str(),
                "question" | "goal_done" | "goal_failed" | "goal_plan" | "mission_blocked"
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Connection) {
        let d = tempfile::tempdir().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        let c = store::open().unwrap();
        (d, c)
    }

    fn seed(c: &Connection, t: i64) {
        c.execute(
            "INSERT INTO escalations (id, kind, question, safe_answer, created_at, deadline_at)
             VALUES ('E-001', 'over_budget', 'Spend passed the budget. Continue?', 'stop', ?1, ?2)",
            params![t, t + 3600],
        )
        .unwrap();
        c.execute(
            "INSERT INTO goals (id, outcome, status, created_at, updated_at) VALUES ('G-001', 'Add probe()', 'done', ?1, ?1)",
            params![t + 1],
        )
        .unwrap();
        c.execute(
            "INSERT INTO missions (id, repo, goal, role, status, review_note, created_at, updated_at)
             VALUES ('M-001', 'r', 'Fix CI', 'implementer', 'blocked', 'CI failed twice', ?1, ?1)",
            params![t + 2],
        )
        .unwrap();
        c.execute(
            "INSERT INTO missions (id, repo, goal, role, status, created_at, updated_at)
             VALUES ('M-002', 'r', 'Old work', 'implementer', 'done', ?1, ?1)",
            params![t + 3],
        )
        .unwrap();
    }

    #[test]
    fn every_notice_names_its_screen_and_read_state_moves() {
        let (_d, c) = db();
        let t = 1_800_000_000;
        seed(&c, t);
        let n = list_in(&c, t + 10).unwrap();
        let kinds: Vec<&str> = n.iter().map(|x| x.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["mission_blocked", "goal_done", "question"],
            "done missions are not notices"
        );
        assert_eq!(n[0].view, "mission");
        assert_eq!(n[0].detail, "CI failed twice");
        assert_eq!(n[1].view, "goals");
        assert_eq!(n[2].view, "today");
        assert!(n[2].open && !n[1].open);
        assert!(n.iter().all(|x| x.unread));
        mark_read_in(&c, t + 10).unwrap();
        assert!(list_in(&c, t + 10).unwrap().iter().all(|x| !x.unread));
        // Finished work older than a week drops off; an open question stays.
        let later = list_in(&c, t + WINDOW_SECS + 100).unwrap();
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].kind, "question");
        store::set_test_db(None);
    }

    #[test]
    fn os_notifications_skip_history_and_respect_the_switch() {
        let (_d, c) = db();
        let t = 1_800_000_000;
        seed(&c, t);
        assert!(
            take_for_os_in(&c, t + 10).unwrap().is_empty(),
            "first run shows nothing old"
        );
        c.execute(
            "INSERT INTO goals (id, outcome, status, created_at, updated_at) VALUES ('G-002', 'Docs', 'failed', ?1, ?1)",
            params![t + 20],
        )
        .unwrap();
        let shown = take_for_os_in(&c, t + 30).unwrap();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].kind, "goal_failed");
        assert!(take_for_os_in(&c, t + 40).unwrap().is_empty(), "shown once");
        store::kv_set(&c, OS_SWITCH_KEY, "off").unwrap();
        c.execute(
            "INSERT INTO goals (id, outcome, status, created_at, updated_at) VALUES ('G-003', 'More', 'done', ?1, ?1)",
            params![t + 50],
        )
        .unwrap();
        assert!(take_for_os_in(&c, t + 60).unwrap().is_empty());
        store::set_test_db(None);
    }
}
