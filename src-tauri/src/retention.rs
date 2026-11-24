//! Time-based retention for the Studio store tables that only ever grow.
//!
//! The report feed already keeps its newest rows, but the audit log, the agent event log, the
//! context-compaction ledger, the memory ledger and answered escalations were never pruned. On a
//! fleet that runs every day those tables grow without limit, which makes every query over them
//! slower over time. Each table gets a fixed window wide enough for history and narrow enough that
//! the store stays flat: audit and answered escalations 30 days, everything else 90 days. Open
//! escalations are never touched.

use anyhow::Result;
use rusqlite::Connection;

pub const DAY: i64 = 86_400;

/// The owner's audit trail moves to the vault decision log; a month in the store is enough.
pub const AUDIT_SECS: i64 = 30 * DAY;
/// Agent events keep the run timeline meaningful for a quarter.
pub const AGENT_EVENT_SECS: i64 = 90 * DAY;
/// The context-compaction ledger is history, not current state.
pub const CONTEXT_SECS: i64 = 90 * DAY;
/// Memory reads and writes per chat turn.
pub const MEMORY_SECS: i64 = 90 * DAY;
/// Answered and expired escalations are decisions recorded elsewhere (goal, activity, reports).
pub const ESCALATION_SECS: i64 = 30 * DAY;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pruned {
    pub audit: usize,
    pub agent_events: usize,
    pub context_events: usize,
    pub memory_ledger: usize,
    pub escalations: usize,
}

impl Pruned {
    pub fn total(&self) -> usize {
        self.audit + self.agent_events + self.context_events + self.memory_ledger + self.escalations
    }
}

/// Delete rows older than each window. `now` is a unix timestamp, passed in so the windows are
/// testable. Rows at exactly the cutoff stay.
pub fn prune(c: &Connection, now: i64) -> Result<Pruned> {
    Ok(Pruned {
        audit: c.execute("DELETE FROM audit WHERE at < ?1", [now - AUDIT_SECS])?,
        agent_events: c.execute(
            "DELETE FROM agent_events WHERE at < ?1",
            [now - AGENT_EVENT_SECS],
        )?,
        context_events: c.execute(
            "DELETE FROM context_events WHERE at < ?1",
            [now - CONTEXT_SECS],
        )?,
        memory_ledger: c.execute(
            "DELETE FROM memory_ledger WHERE at < ?1",
            [now - MEMORY_SECS],
        )?,
        escalations: c.execute(
            "DELETE FROM escalations WHERE status <> 'open' AND created_at < ?1",
            [now - ESCALATION_SECS],
        )?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    fn insert_audit(c: &Connection, at: i64) {
        c.execute(
            "INSERT INTO audit (at, action, target, detail, outcome) VALUES (?1, 'perm', 'x', '', 'ok')",
            [at],
        )
        .unwrap();
    }

    fn insert_agent_event(c: &Connection, at: i64) {
        c.execute(
            "INSERT INTO agent_events (at, kind) VALUES (?1, 'handoff')",
            [at],
        )
        .unwrap();
    }

    fn insert_context_event(c: &Connection, at: i64, first_msg: i64) {
        c.execute(
            "INSERT INTO context_events (at, agent, session_id, source, first_msg, last_msg)
             VALUES (?1, 'hermes', 's-1', 'compact', ?2, ?2)",
            rusqlite::params![at, first_msg],
        )
        .unwrap();
    }

    fn insert_memory(c: &Connection, at: i64) {
        c.execute(
            "INSERT INTO memory_ledger (session_id, turn, op, target, at) VALUES ('s-1', 1, 'read', 't', ?1)",
            [at],
        )
        .unwrap();
    }

    fn insert_escalation(c: &Connection, at: i64, status: &str, id: &str) {
        c.execute(
            "INSERT INTO escalations (id, kind, question, safe_answer, status, created_at, deadline_at)
             VALUES (?1, 'stuck', 'q', 'stop', ?2, ?3, ?3)",
            rusqlite::params![id, status, at],
        )
        .unwrap();
    }

    #[test]
    fn old_rows_are_deleted_and_recent_or_open_rows_stay() {
        let c = store::test_conn();
        let now = 1_000 * DAY;

        insert_audit(&c, now - AUDIT_SECS - 1);
        insert_audit(&c, now - AUDIT_SECS);
        insert_agent_event(&c, now - AGENT_EVENT_SECS - 1);
        insert_agent_event(&c, now - AGENT_EVENT_SECS);
        insert_context_event(&c, now - CONTEXT_SECS - 1, 1);
        insert_context_event(&c, now - CONTEXT_SECS, 2);
        insert_memory(&c, now - MEMORY_SECS - 1);
        insert_memory(&c, now - MEMORY_SECS);
        insert_escalation(&c, now - ESCALATION_SECS - 1, "answered", "old-answered");
        insert_escalation(&c, now - ESCALATION_SECS - 1, "expired", "old-expired");
        insert_escalation(&c, now - ESCALATION_SECS - 1, "open", "old-open");
        insert_escalation(&c, now - ESCALATION_SECS, "answered", "recent-answered");

        let out = prune(&c, now).unwrap();
        assert_eq!(
            out,
            Pruned {
                audit: 1,
                agent_events: 1,
                context_events: 1,
                memory_ledger: 1,
                escalations: 2,
            }
        );
        assert_eq!(out.total(), 6);

        let count = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM audit"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM agent_events"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM context_events"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM memory_ledger"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM escalations"), 2);
        assert_eq!(
            count("SELECT COUNT(*) FROM escalations WHERE status = 'open'"),
            1,
            "an open question is never pruned, however old"
        );
    }

    #[test]
    fn a_second_pass_over_the_same_rows_deletes_nothing() {
        let c = store::test_conn();
        let now = 1_000 * DAY;
        insert_audit(&c, now - AUDIT_SECS - 1);
        assert_eq!(prune(&c, now).unwrap().total(), 1);
        assert_eq!(prune(&c, now).unwrap().total(), 0);
    }
}
