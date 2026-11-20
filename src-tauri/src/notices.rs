//! The notification centre: one list of everything that needs the owner's attention, built
//! from the state Studio already keeps (escalations, goals, missions), each item linking to the
//! screen that owns it. Nothing new is stored except which items were read and which were
//! already shown as a Windows notification, both in kv, so no schema change.

use std::collections::BTreeSet;

use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::store;

/// How far back finished work stays in the list.
const WINDOW_SECS: i64 = 7 * 24 * 3600;
// Immutable pre-upgrade cutoff. New acknowledgments only write READ_IDS_KEY.
const READ_KEY: &str = "notices.read_until";
const READ_IDS_KEY: &str = "notices.read_ids";
const MAX_READ_IDS: usize = 4096;
const MAX_NOTICE_ID_BYTES: usize = 512;
const SHOWN_KEY: &str = "notices.os_shown_until";
pub const OS_SWITCH_KEY: &str = "notices.os";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Notice {
    /// Stable across refreshes: kind, source id and event timestamp where available.
    pub id: String,
    /// The source record or provider, without parsing the display text or notice id.
    pub entity_id: String,
    /// The source's repository, if it has one. Goals may span multiple repositories.
    pub repo: Option<String>,
    /// question, goal_done, goal_failed, goal_plan, mission_blocked, mission_failed, review, connection
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

fn read_ids_in(c: &Connection) -> Result<BTreeSet<String>> {
    match store::kv_get(c, READ_IDS_KEY)? {
        Some(value) => serde_json::from_str(&value).context("invalid notices.read_ids"),
        None => Ok(BTreeSet::new()),
    }
}

/// Every notice, newest first.
pub fn list_in(c: &Connection, now: i64) -> Result<Vec<Notice>> {
    let read_until: i64 = store::kv_get(c, READ_KEY)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let read_ids = read_ids_in(c)?;
    let since = now - WINDOW_SECS;
    let mut out = Vec::new();

    let mut q = c.prepare(
        "SELECT e.id, e.kind, e.question, e.created_at, m.repo FROM escalations e
         LEFT JOIN missions m ON m.id = e.mission_id
         WHERE e.status = 'open' ORDER BY e.created_at DESC",
    )?;
    for r in q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })? {
        let (id, kind, question, at, repo) = r?;
        out.push(Notice {
            id: format!("question.{id}"),
            entity_id: id,
            repo,
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
         WHERE status = 'awaiting' OR (status IN ('done', 'failed') AND updated_at >= ?1)
         ORDER BY updated_at DESC",
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
            entity_id: id,
            repo: None,
            at,
            unread: at > read_until,
            open,
        });
    }

    let mut q = c.prepare(
        "SELECT id, goal, status, COALESCE(review_note, ''), updated_at, repo FROM missions
         WHERE status IN ('blocked', 'review') OR (status = 'failed' AND updated_at >= ?1)
         ORDER BY updated_at DESC",
    )?;
    for r in q.query_map(params![since], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, String>(5)?,
        ))
    })? {
        let (id, goal, status, note, at, repo) = r?;
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
            entity_id: id,
            repo: Some(repo),
            at,
            unread: at > read_until,
            open,
        });
    }
    // Subscriptions and keys Hermes is set to use that cannot run right now. A failure event is
    // its own notice; a lasting condition (signed out, plugin off) keeps one notice per
    // occurrence, so acknowledging it survives the five-minute health checks.
    if let Some(r) = crate::provider_health::last_in(c) {
        for h in r.problems() {
            let (at, id) = match &h.last_failure {
                Some(failure) => {
                    let hash = Sha256::digest(serde_json::to_vec(failure)?);
                    (
                        failure.at,
                        format!(
                            "connection.{}.{}.{}.{hash:x}",
                            h.id, h.state, failure.at
                        ),
                    )
                }
                None => (
                    r.checked_at,
                    format!(
                        "connection.{}.{}.{}",
                        h.id,
                        h.state,
                        h.since.unwrap_or(r.checked_at)
                    ),
                ),
            };
            out.push(Notice {
                id,
                entity_id: h.id.clone(),
                repo: None,
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
    for notice in &mut out {
        notice.unread &= !read_ids.contains(&notice.id);
    }
    out.sort_by_key(|n| std::cmp::Reverse(n.at));
    Ok(out)
}

pub fn list(now: i64) -> Result<Vec<Notice>> {
    list_in(&store::open()?, now)
}

/// Acknowledge only the displayed IDs that still exist, preserving other acknowledgments.
pub fn mark_read_in(c: &Connection, ids: &[String], now: i64) -> Result<()> {
    ensure!(now >= 0, "notice time must not be negative");
    ensure!(
        ids.len() <= MAX_READ_IDS,
        "too many notice IDs (maximum {MAX_READ_IDS})"
    );
    ensure!(
        ids.iter().all(|id| !id.is_empty()
            && id.len() <= MAX_NOTICE_ID_BYTES
            && !id.chars().any(char::is_control)),
        "notice IDs must be 1 to {MAX_NOTICE_ID_BYTES} bytes without control characters"
    );

    // Lock before reading so concurrent callers cannot overwrite each other's acknowledgments.
    let tx = Transaction::new_unchecked(c, TransactionBehavior::Immediate)?;
    let mut read_ids = read_ids_in(&tx)?;
    let current: BTreeSet<String> = list_in(&tx, now)?.into_iter().map(|n| n.id).collect();
    read_ids.extend(ids.iter().filter(|id| current.contains(*id)).cloned());
    read_ids.retain(|id| current.contains(id));
    store::kv_set(&tx, READ_IDS_KEY, &serde_json::to_string(&read_ids)?)?;
    tx.commit()?;
    Ok(())
}

pub fn mark_read(ids: &[String], now: i64) -> Result<()> {
    mark_read_in(&store::open()?, ids, now)
}

/// Windows notifications are on unless the owner switched them off.
pub fn os_enabled_in(c: &Connection) -> bool {
    store::kv_get(c, OS_SWITCH_KEY).ok().flatten().as_deref() != Some("off")
}

/// Notices that should raise a Windows notification now: newer than the last one shown, and
/// only the kinds worth interrupting for. The first call after install shows nothing old: it
/// only records where it is. Marks them as shown.
pub fn take_for_os_in(c: &Connection, now: i64) -> Result<Vec<Notice>> {
    let tx = Transaction::new_unchecked(c, TransactionBehavior::Immediate)?;
    let shown: Option<i64> = store::kv_get(&tx, SHOWN_KEY)?.and_then(|v| v.parse().ok());
    let notices = match shown {
        Some(shown) if os_enabled_in(&tx) => list_in(&tx, now)?
            .into_iter()
            .filter(|n| n.at > shown)
            .filter(|n| {
                matches!(
                    n.kind.as_str(),
                    "question" | "goal_done" | "goal_failed" | "goal_plan" | "mission_blocked"
                )
            })
            .collect(),
        // Installation and disabled notifications deliberately skip history without listing it.
        _ => Vec::new(),
    };
    let next = shown.map_or(now, |previous| previous.max(now));
    store::kv_set(&tx, SHOWN_KEY, &next.to_string())?;
    tx.commit()?;
    Ok(notices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice_ids(c: &Connection, now: i64) -> Vec<String> {
        list_in(c, now).unwrap().into_iter().map(|n| n.id).collect()
    }

    fn connection_report(
        checked_at: i64,
        failure_at: Option<i64>,
    ) -> crate::provider_health::Report {
        use crate::provider_health::{Failure, Health, Report};
        Report {
            providers: vec![Health {
                id: "provider.test".into(),
                label: "Test provider".into(),
                in_use: true,
                signed_in: Some(true),
                plugin_enabled: Some(true),
                state: "failing".into(),
                since: None,
                detail: "Connection failed".into(),
                last_failure: failure_at.map(|at| Failure {
                    at,
                    provider: "provider.test".into(),
                    kind: "timeout".into(),
                    summary: "Request timed out".into(),
                    resets: None,
                }),
                fix: None,
            }],
            checked_at,
            ..Report::default()
        }
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
        let c = store::test_conn();
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
        mark_read_in(&c, &notice_ids(&c, t + 10), t + 10).unwrap();
        assert!(list_in(&c, t + 10).unwrap().iter().all(|x| !x.unread));
        // Finished work older than a week drops off; unresolved work stays.
        let later = list_in(&c, t + WINDOW_SECS + 100).unwrap();
        let kinds: Vec<_> = later.iter().map(|n| n.kind.as_str()).collect();
        assert_eq!(kinds, ["mission_blocked", "question"]);
    }

    #[test]
    fn unresolved_notices_remain_after_the_finished_work_window() {
        let c = store::test_conn();
        let at = 1_800_000_000;
        seed(&c, at);
        c.execute(
            "INSERT INTO goals (id, outcome, status, created_at, updated_at) VALUES ('G-002', 'Approve work', 'awaiting', ?1, ?1)",
            params![at],
        )
        .unwrap();
        c.execute(
            "INSERT INTO missions (id, repo, goal, role, status, created_at, updated_at) VALUES ('M-003', 'r', 'Review work', 'implementer', 'review', ?1, ?1)",
            params![at],
        )
        .unwrap();
        let notices = list_in(&c, at + WINDOW_SECS + 100).unwrap();
        let mut kinds: Vec<_> = notices.iter().map(|notice| notice.kind.as_str()).collect();
        kinds.sort_unstable();
        assert_eq!(
            kinds,
            ["goal_plan", "mission_blocked", "question", "review"]
        );
        assert!(notices.iter().all(|notice| notice.open));
    }

    #[test]
    fn notice_targets_identify_the_source_record_without_parsing_display_text() {
        let c = store::test_conn();
        seed(&c, 1_800_000_000);
        c.execute(
            "UPDATE escalations SET mission_id = 'M-002' WHERE id = 'E-001'",
            [],
        )
        .unwrap();
        c.execute("UPDATE missions SET repo = 'ops' WHERE id = 'M-002'", [])
            .unwrap();
        let notices = serde_json::to_value(list_in(&c, 1_800_000_010).unwrap()).unwrap();
        assert_eq!(notices[0]["entity_id"], "M-001");
        assert_eq!(notices[0]["repo"], "r");
        assert_eq!(notices[1]["entity_id"], "G-001");
        assert_eq!(notices[1].get("repo"), Some(&serde_json::Value::Null));
        assert_eq!(notices[2]["entity_id"], "E-001");
        assert_eq!(notices[2]["repo"], "ops");
    }

    #[test]
    fn optional_source_repos_are_explicitly_null() {
        let c = store::test_conn();
        let t = 1_800_000_000;
        seed(&c, t);
        crate::provider_health::record_in(&c, &connection_report(t + 5, Some(t + 4)));
        let notices = serde_json::to_value(list_in(&c, t + 10).unwrap()).unwrap();
        assert_eq!(notices[0]["entity_id"], "provider.test");
        assert_eq!(notices[0].get("repo"), Some(&serde_json::Value::Null));
        assert_eq!(notices[0]["view"], "setup");
        assert_eq!(notices[3].get("repo"), Some(&serde_json::Value::Null));
    }

    #[test]
    fn exact_snapshot_leaves_same_second_and_later_arrivals_unread() {
        let c = store::test_conn();
        let t = 1_800_000_000;
        seed(&c, t);
        let displayed = notice_ids(&c, t + 10);
        c.execute(
            "INSERT INTO goals (id, outcome, status, created_at, updated_at) VALUES
             ('G-002', 'Same second', 'done', ?1, ?1),
             ('G-003', 'Later arrival', 'failed', ?2, ?2)",
            params![t + 1, t + 11],
        )
        .unwrap();
        mark_read_in(&c, &displayed, t + 20).unwrap();
        let notices = list_in(&c, t + 20).unwrap();
        let unread: Vec<_> = notices
            .iter()
            .filter(|n| n.unread)
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(
            unread,
            ["goal_failed.G-003.1800000011", "goal_done.G-002.1800000001"]
        );
        assert!(notices
            .iter()
            .filter(|n| displayed.contains(&n.id))
            .all(|n| !n.unread));
        assert_eq!(store::kv_get(&c, READ_KEY).unwrap(), None);
    }

    #[test]
    fn interleaved_and_stale_acknowledgments_union_ids_without_reading_other_notices() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("notices.db");
        let a = store::open_at(&path).unwrap();
        let b = store::open_at(&path).unwrap();
        let t = 1_800_000_000;
        seed(&a, t);
        let old_snapshot = vec!["question.E-001".to_string()];
        let newer_snapshot = vec!["goal_done.G-001.1800000001".to_string()];
        mark_read_in(&b, &newer_snapshot, t + 20).unwrap();
        mark_read_in(&a, &old_snapshot, t + 10).unwrap();
        mark_read_in(&b, &newer_snapshot, t + 30).unwrap();
        mark_read_in(&a, &[], t + 40).unwrap();
        let unread: Vec<_> = list_in(&a, t + 40)
            .unwrap()
            .into_iter()
            .filter(|n| n.unread)
            .map(|n| n.id)
            .collect();
        assert_eq!(unread, ["mission_blocked.M-001.1800000002"]);
        let saved: Vec<String> =
            serde_json::from_str(&store::kv_get(&a, "notices.read_ids").unwrap().unwrap()).unwrap();
        assert_eq!(saved, ["goal_done.G-001.1800000001", "question.E-001"]);
    }

    #[test]
    fn pre_upgrade_cutoff_is_honored_but_never_advanced() {
        let c = store::test_conn();
        let t = 1_800_000_000;
        seed(&c, t);
        store::kv_set(&c, READ_KEY, &(t + 1).to_string()).unwrap();
        let notices = list_in(&c, t + 10).unwrap();
        assert!(notices[0].unread);
        assert!(!notices[1].unread && !notices[2].unread);
        mark_read_in(&c, &[notices[0].id.clone()], t + 10).unwrap();
        assert_eq!(
            store::kv_get(&c, READ_KEY).unwrap().as_deref(),
            Some("1800000001")
        );
        assert!(list_in(&c, t + 10).unwrap().iter().all(|n| !n.unread));
    }

    #[test]
    fn acknowledgments_ignore_unknown_ids_and_prune_resolved_and_expired_ids() {
        let c = store::test_conn();
        let t = 1_800_000_000;
        seed(&c, t);
        let mut ids = notice_ids(&c, t + 10);
        ids.push("question.E-future".into());
        ids.push(ids[0].clone());
        mark_read_in(&c, &ids, t + 10).unwrap();
        let saved: Vec<String> =
            serde_json::from_str(&store::kv_get(&c, "notices.read_ids").unwrap().unwrap()).unwrap();
        assert_eq!(saved.len(), 3);
        assert!(!saved.contains(&"question.E-future".to_string()));
        c.execute(
            "UPDATE escalations SET status = 'answered' WHERE id = 'E-001'",
            [],
        )
        .unwrap();
        mark_read_in(&c, &ids, t + WINDOW_SECS + 100).unwrap();
        assert_eq!(
            store::kv_get(&c, "notices.read_ids").unwrap().as_deref(),
            Some("[\"mission_blocked.M-001.1800000002\"]")
        );
        c.execute(
            "INSERT INTO escalations (id, kind, question, safe_answer, created_at, deadline_at)
             VALUES ('E-future', 'review', 'New question', 'wait', ?1, ?2)",
            params![t + 1, t + 3600],
        )
        .unwrap();
        assert!(
            list_in(&c, t + WINDOW_SECS + 100)
                .unwrap()
                .iter()
                .find(|n| n.id == "question.E-future")
                .unwrap()
                .unread
        );
    }

    #[test]
    fn malformed_read_ids_fail_listing_and_acknowledgment_without_writes() {
        let c = store::test_conn();
        seed(&c, 1_800_000_000);
        for malformed in ["not-json", "{}", "null", "[1]", "[\"question.E-001\",]"] {
            store::kv_set(&c, "notices.read_ids", malformed).unwrap();
            assert!(list_in(&c, 1_800_000_010).is_err(), "accepted {malformed}");
            assert!(mark_read_in(&c, &["question.E-001".into()], 1_800_000_010).is_err());
            assert_eq!(
                store::kv_get(&c, "notices.read_ids").unwrap().as_deref(),
                Some(malformed)
            );
            assert_eq!(store::kv_get(&c, READ_KEY).unwrap(), None);
        }
    }

    #[test]
    fn invalid_acknowledgments_are_rejected_without_writes() {
        let t = 1_800_000_000;
        let mut accepted = Vec::new();
        for (case, ids, now) in [
            ("too many IDs", vec!["question.E-001".into(); 4097], t),
            ("too long", vec!["x".repeat(513)], t),
            ("empty ID", vec![String::new()], t),
            ("newline", vec!["question.E-001\n".into()], t),
            ("Unicode control", vec!["question.\u{0085}".into()], t),
            ("NUL", vec!["question.E-001\0".into()], t),
            ("negative time", vec!["question.E-001".into()], -1),
        ] {
            let c = store::test_conn();
            seed(&c, t);
            if mark_read_in(&c, &ids, now).is_ok() {
                accepted.push(case);
            } else {
                assert_eq!(store::kv_get(&c, "notices.read_ids").unwrap(), None);
                assert_eq!(store::kv_get(&c, READ_KEY).unwrap(), None);
            }
        }
        assert!(accepted.is_empty(), "accepted invalid inputs: {accepted:?}");
        let c = store::test_conn();
        seed(&c, t);
        mark_read_in(&c, &vec!["x".repeat(512); 4096], t).unwrap();
        assert!(list_in(&c, t + 10).unwrap().iter().all(|n| n.unread));
    }

    #[test]
    fn acknowledgment_list_failure_preserves_both_read_markers() {
        let c = store::test_conn();
        seed(&c, 1_800_000_000);
        let saved = "[\"question.E-001\"]";
        store::kv_set(&c, "notices.read_ids", saved).unwrap();
        store::kv_set(&c, READ_KEY, "100").unwrap();
        c.execute_batch("ALTER TABLE missions RENAME TO hidden_missions")
            .unwrap();
        assert!(mark_read_in(&c, &["goal_done.G-001.1800000001".into()], 1_800_000_010).is_err());
        assert_eq!(
            store::kv_get(&c, "notices.read_ids").unwrap().as_deref(),
            Some(saved)
        );
        assert_eq!(store::kv_get(&c, READ_KEY).unwrap().as_deref(), Some("100"));
    }

    #[test]
    fn connection_ids_distinguish_failure_events_but_not_report_refreshes() {
        let c = store::test_conn();
        let t = 1_800_000_000;
        crate::provider_health::record_in(&c, &connection_report(t + 1, Some(t)));
        let ids = notice_ids(&c, t + 10);
        mark_read_in(&c, &ids, t + 10).unwrap();
        crate::provider_health::record_in(&c, &connection_report(t + 2, Some(t)));
        assert_eq!(notice_ids(&c, t + 10), ids);
        assert!(!list_in(&c, t + 10).unwrap()[0].unread);
        crate::provider_health::record_in(&c, &connection_report(t + 3, Some(t + 1)));
        let next = list_in(&c, t + 10).unwrap();
        assert_ne!(next[0].id, ids[0]);
        assert!(next[0].unread);
        assert_eq!(next[0].at, t + 1);
    }

    #[test]
    fn same_second_connection_failures_do_not_share_acknowledgments() {
        let c = store::test_conn();
        let t = 1_800_000_000;
        let mut report = connection_report(t + 1, Some(t));
        crate::provider_health::record_in(&c, &report);
        let ids = notice_ids(&c, t + 10);
        mark_read_in(&c, &ids, t + 10).unwrap();
        assert!(!list_in(&c, t + 10).unwrap()[0].unread);

        report.providers[0].last_failure.as_mut().unwrap().summary =
            "Another request timed out".into();
        crate::provider_health::record_in(&c, &report);
        let next = list_in(&c, t + 10).unwrap();
        assert!(next[0].unread);
        assert_ne!(next[0].id, ids[0]);
        assert_eq!(next[0].at, t);
        assert!(!next[0].id.contains("Another request timed out"));

        report.checked_at = t + 2;
        crate::provider_health::record_in(&c, &report);
        let refreshed = list_in(&c, t + 10).unwrap();
        assert_eq!(refreshed[0].id, next[0].id);
        assert!(refreshed[0].unread);
    }

    #[test]
    fn connection_ids_stay_stable_while_a_failure_free_condition_persists() {
        let c = store::test_conn();
        let t = 1_800_000_000;
        crate::provider_health::record_in(&c, &connection_report(t, None));
        let ids = notice_ids(&c, t + 10);
        mark_read_in(&c, &ids, t + 10).unwrap();
        crate::provider_health::record_in(&c, &connection_report(t + 1, None));
        let next = list_in(&c, t + 10).unwrap();
        assert_eq!(next[0].id, ids[0]);
        assert!(!next[0].unread);

        let mut recovered = connection_report(t + 2, None);
        recovered.providers[0].state = "ok".into();
        crate::provider_health::record_in(&c, &recovered);
        crate::provider_health::record_in(&c, &connection_report(t + 3, None));
        let again = list_in(&c, t + 10).unwrap();
        assert_ne!(again[0].id, ids[0]);
        assert!(again[0].unread);
    }

    #[test]
    fn os_list_failure_does_not_advance_marker_or_lose_pending_notices() {
        let c = store::test_conn();
        let t = 1_800_000_000;
        seed(&c, t);
        store::kv_set(&c, SHOWN_KEY, &(t - 1).to_string()).unwrap();
        c.execute_batch("ALTER TABLE missions RENAME TO hidden_missions")
            .unwrap();
        assert!(take_for_os_in(&c, t + 10).is_err());
        assert_eq!(
            store::kv_get(&c, SHOWN_KEY).unwrap().as_deref(),
            Some("1799999999")
        );
        c.execute_batch("ALTER TABLE hidden_missions RENAME TO missions")
            .unwrap();
        assert_eq!(take_for_os_in(&c, t + 10).unwrap().len(), 3);
    }

    #[test]
    fn os_marker_never_moves_backwards_even_while_disabled() {
        let c = store::test_conn();
        let t = 1_800_000_000;
        seed(&c, t);
        store::kv_set(&c, SHOWN_KEY, &(t + 10).to_string()).unwrap();
        for switch in ["on", "off"] {
            store::kv_set(&c, OS_SWITCH_KEY, switch).unwrap();
            assert!(take_for_os_in(&c, t - 10).unwrap().is_empty());
            assert_eq!(
                store::kv_get(&c, SHOWN_KEY).unwrap().as_deref(),
                Some("1800000010")
            );
        }
        store::kv_set(&c, OS_SWITCH_KEY, "on").unwrap();
        assert!(take_for_os_in(&c, t + 20).unwrap().is_empty());
    }

    #[test]
    fn os_notifications_skip_history_and_respect_the_switch() {
        let c = store::test_conn();
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
        store::kv_set(&c, OS_SWITCH_KEY, "on").unwrap();
        assert!(
            take_for_os_in(&c, t + 70).unwrap().is_empty(),
            "off skips history"
        );
    }

    #[test]
    fn finished_failures_and_done_goals_expire_only_after_seven_days() {
        let c = store::test_conn();
        let t = 1_800_000_000;
        seed(&c, t);
        c.execute(
            "INSERT INTO goals (id, outcome, status, created_at, updated_at) VALUES ('G-002', 'Failed goal', 'failed', ?1, ?1)",
            params![t + 1],
        ).unwrap();
        c.execute(
            "INSERT INTO missions (id, repo, goal, role, status, created_at, updated_at) VALUES ('M-003', 'r', 'Failed mission', 'implementer', 'failed', ?1, ?1)",
            params![t + 1],
        ).unwrap();
        let at_boundary = list_in(&c, t + 1 + WINDOW_SECS).unwrap();
        let mut finished: Vec<_> = at_boundary
            .iter()
            .filter(|n| !n.open)
            .map(|n| n.kind.as_str())
            .collect();
        finished.sort_unstable();
        assert_eq!(finished, ["goal_done", "goal_failed", "mission_failed"]);
        let after = list_in(&c, t + 2 + WINDOW_SECS).unwrap();
        assert!(after.iter().all(|n| n.open));
    }
}
