//! Reports: the read-only record of work the fleet did, newest first.
//!
//! The orchestrator and the scheduler write one row per event (mission start, review, merge,
//! block, recovery, question answered automatically, digest sent, update applied). The owner
//! reads the feed and never has to act on it.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::store;

/// Every kind a row may carry.
pub const KINDS: [&str; 11] = [
    "mission.started",
    "mission.routed",
    "mission.reviewed",
    "mission.merged",
    "mission.blocked",
    "mission.unstuck",
    "escalation.auto_answered",
    "autonomy.pass",
    "digest.sent",
    "fleet_report.sent",
    "update.applied",
];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Report {
    pub id: i64,
    pub at: i64,
    pub repo: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RepoSummary {
    pub repo: String,
    pub count: i64,
    pub last_at: i64,
}

fn trim_to(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + " ..."
    }
}

const CURSOR: &str = "reports.read_until";
/// Newest rows kept; older rows are pruned by the scheduler.
pub const KEEP: usize = 2000;

/// Record one row on an open connection.
pub fn record(
    c: &Connection,
    kind: &str,
    repo: &str,
    title: &str,
    body: &str,
    reference: &str,
) -> Result<i64> {
    anyhow::ensure!(KINDS.contains(&kind), "unknown report kind '{kind}'");
    let title = trim_to(title, 200);
    anyhow::ensure!(!title.is_empty(), "a report needs a title");
    c.execute(
        "INSERT INTO reports (at, repo, kind, title, body, reference) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            store::now(),
            trim_to(repo, 120),
            kind,
            title,
            trim_to(body, 4000),
            trim_to(reference, 500)
        ],
    )?;
    Ok(c.last_insert_rowid())
}

/// Record one row on a fresh connection. Failures are the caller's to ignore: a report must
/// never stop the work it describes.
pub fn record_here(
    kind: &str,
    repo: &str,
    title: &str,
    body: &str,
    reference: &str,
) -> Result<i64> {
    let c = store::open()?;
    record(&c, kind, repo, title, body, reference)
}

/// Newest first, optionally after `since_id`, optionally one repository.
pub fn list(since_id: i64, repo: &str, limit: usize) -> Result<Vec<Report>> {
    let c = store::open()?;
    let limit = limit.clamp(1, 500) as i64;
    let mut st = c.prepare(
        "SELECT id, at, repo, kind, title, body, reference FROM reports
         WHERE id > ?1 AND (?2 = '' OR repo = ?2)
         ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = st
        .query_map(params![since_id, repo.trim(), limit], |r| {
            Ok(Report {
                id: r.get(0)?,
                at: r.get(1)?,
                repo: r.get(2)?,
                kind: r.get(3)?,
                title: r.get(4)?,
                body: r.get(5)?,
                reference: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Per repository counts since `since` (unix seconds), busiest first.
pub fn summary(since: i64) -> Result<Vec<RepoSummary>> {
    let c = store::open()?;
    let mut st = c.prepare(
        "SELECT repo, COUNT(*), MAX(at) FROM reports WHERE at >= ?1
         GROUP BY repo ORDER BY COUNT(*) DESC, repo",
    )?;
    let rows = st
        .query_map([since], |r| {
            Ok(RepoSummary {
                repo: r.get(0)?,
                count: r.get(1)?,
                last_at: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Rows of one kind recorded since `since` (unix seconds).
pub fn count_kind_since(kind: &str, since: i64) -> Result<i64> {
    let c = store::open()?;
    Ok(c.query_row(
        "SELECT COUNT(*) FROM reports WHERE kind = ?1 AND at >= ?2",
        params![kind, since],
        |r| r.get(0),
    )?)
}

fn cursor() -> i64 {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, CURSOR).ok().flatten())
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

/// Rows newer than the read cursor.
pub fn unread() -> Result<i64> {
    let c = store::open()?;
    Ok(c.query_row(
        "SELECT COUNT(*) FROM reports WHERE id > ?1",
        [cursor()],
        |r| r.get(0),
    )?)
}

/// Move the read cursor to `id` (never backwards).
pub fn mark_read(id: i64) -> Result<()> {
    let next = cursor().max(id);
    let c = store::open()?;
    store::kv_set(&c, CURSOR, &next.to_string())
}

/// Keep the newest `keep` rows.
pub fn prune(c: &Connection, keep: usize) -> Result<usize> {
    Ok(c.execute(
        "DELETE FROM reports WHERE id NOT IN
           (SELECT id FROM reports ORDER BY id DESC LIMIT ?1)",
        [keep.max(1) as i64],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let g = store::test_env_lock();
        let d = tempfile::TempDir::new().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        (d, g)
    }

    #[test]
    fn records_and_lists_newest_first_with_filters() {
        let (_d, _g) = db();
        record_here("mission.started", "repo-a", "M-1 started", "working", "M-1").unwrap();
        record_here(
            "mission.merged",
            "repo-b",
            "M-2 merged",
            "green",
            "https://x/pull/2",
        )
        .unwrap();
        record_here(
            "mission.blocked",
            "repo-a",
            "M-3 blocked",
            "no retry left",
            "M-3",
        )
        .unwrap();

        let all = list(0, "", 10).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].kind, "mission.blocked");
        assert_eq!(all[2].kind, "mission.started");

        let one = list(0, "repo-a", 10).unwrap();
        assert_eq!(one.len(), 2);
        assert!(one.iter().all(|r| r.repo == "repo-a"));

        let after = list(all[0].id, "", 10).unwrap();
        assert!(after.is_empty());
    }

    #[test]
    fn summary_groups_by_repository() {
        let (_d, _g) = db();
        record_here("mission.started", "repo-a", "one", "", "").unwrap();
        record_here("mission.merged", "repo-a", "two", "", "").unwrap();
        record_here("mission.started", "repo-b", "three", "", "").unwrap();
        let sums = summary(0).unwrap();
        assert_eq!(sums.len(), 2);
        assert_eq!(sums[0].repo, "repo-a");
        assert_eq!(sums[0].count, 2);
        assert_eq!(sums[1].repo, "repo-b");
    }

    #[test]
    fn the_read_cursor_moves_forward_only() {
        let (_d, _g) = db();
        let a = record_here("mission.started", "repo-a", "one", "", "").unwrap();
        let b = record_here("mission.started", "repo-a", "two", "", "").unwrap();
        assert_eq!(unread().unwrap(), 2);
        mark_read(a).unwrap();
        assert_eq!(unread().unwrap(), 1);
        mark_read(a).unwrap();
        mark_read(0).unwrap();
        assert_eq!(unread().unwrap(), 1);
        mark_read(b).unwrap();
        assert_eq!(unread().unwrap(), 0);
    }

    #[test]
    fn prune_keeps_the_newest_rows() {
        let (_d, _g) = db();
        for i in 0..5 {
            record_here("mission.started", "repo-a", &format!("row {i}"), "", "").unwrap();
        }
        let c = store::open().unwrap();
        assert_eq!(prune(&c, 3).unwrap(), 2);
        let left = list(0, "", 10).unwrap();
        assert_eq!(left.len(), 3);
        assert_eq!(left[0].title, "row 4");
    }

    #[test]
    fn long_bodies_are_capped() {
        let (_d, _g) = db();
        let big = "x".repeat(10_000);
        record_here("mission.blocked", "repo-a", "long", &big, "").unwrap();
        let rows = list(0, "", 1).unwrap();
        assert!(rows[0].body.chars().count() < 5_000);
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        let (_d, _g) = db();
        assert!(record_here("made.up", "repo-a", "x", "", "").is_err());
    }
}
