//! The exchange log: every time one agent hands work, a question or a decision to another.
//!
//! `studio.db` table `agent_events` (schema 9). Handoffs, asks and answers, council positions and
//! agent assignments each write one row, so the run timeline can show the whole path of a mission
//! without the owner copying anything between tools. The hub process writes here too: it opens the
//! same `studio.db` the app does.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::store;

/// The kinds of exchange the log records.
pub const KINDS: [&str; 7] = [
    "handoff", "ask", "answer", "council", "assign", "defer", "note",
];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Event {
    pub id: i64,
    pub at: i64,
    pub mission_id: Option<String>,
    pub kind: String,
    pub from_agent: String,
    pub to_agent: String,
    pub summary: String,
    /// Free text or JSON: the full packet, question or answer.
    pub detail: String,
    pub duration_ms: Option<i64>,
    pub cost_usd: Option<f64>,
}

/// What a caller fills in; id and time are assigned on insert.
#[derive(Debug, Clone, Default)]
pub struct NewEvent<'a> {
    pub mission_id: Option<&'a str>,
    pub kind: &'a str,
    pub from_agent: &'a str,
    pub to_agent: &'a str,
    pub summary: &'a str,
    pub detail: &'a str,
    pub duration_ms: Option<i64>,
    pub cost_usd: Option<f64>,
}

pub fn record_in(c: &Connection, e: &NewEvent<'_>) -> Result<i64> {
    if !KINDS.contains(&e.kind) {
        anyhow::bail!("unknown agent event kind '{}'", e.kind);
    }
    c.execute(
        "INSERT INTO agent_events
         (at, mission_id, kind, from_agent, to_agent, summary, detail, duration_ms, cost_usd)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            store::now(),
            e.mission_id.map(str::trim).filter(|m| !m.is_empty()),
            e.kind,
            e.from_agent.trim(),
            e.to_agent.trim(),
            e.summary.trim(),
            e.detail,
            e.duration_ms,
            e.cost_usd,
        ],
    )?;
    Ok(c.last_insert_rowid())
}

/// Record one exchange in the real store.
pub fn record(e: &NewEvent<'_>) -> Result<i64> {
    record_in(&store::open()?, e)
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: r.get(0)?,
        at: r.get(1)?,
        mission_id: r.get(2)?,
        kind: r.get(3)?,
        from_agent: r.get(4)?,
        to_agent: r.get(5)?,
        summary: r.get(6)?,
        detail: r.get(7)?,
        duration_ms: r.get(8)?,
        cost_usd: r.get(9)?,
    })
}

const COLS: &str =
    "id, at, mission_id, kind, from_agent, to_agent, summary, detail, duration_ms, cost_usd";

/// A mission's exchanges, oldest first.
pub fn for_mission_in(c: &Connection, mission_id: &str) -> Result<Vec<Event>> {
    let mut st = c.prepare(&format!(
        "SELECT {COLS} FROM agent_events WHERE mission_id = ?1 ORDER BY at, id"
    ))?;
    let rows = st.query_map(params![mission_id], row)?;
    Ok(rows.flatten().collect())
}

pub fn for_mission(mission_id: &str) -> Result<Vec<Event>> {
    for_mission_in(&store::open()?, mission_id)
}

/// The newest exchanges across every mission, newest first.
pub fn recent_in(c: &Connection, limit: usize) -> Result<Vec<Event>> {
    let mut st = c.prepare(&format!(
        "SELECT {COLS} FROM agent_events ORDER BY at DESC, id DESC LIMIT ?1"
    ))?;
    let rows = st.query_map(params![limit as i64], row)?;
    Ok(rows.flatten().collect())
}

pub fn recent(limit: usize) -> Result<Vec<Event>> {
    recent_in(&store::open()?, limit)
}

/// Remove a mission's exchanges (live tests clean up after themselves).
pub fn delete_for_mission(mission_id: &str) -> Result<usize> {
    let c = store::open()?;
    Ok(c.execute(
        "DELETE FROM agent_events WHERE mission_id = ?1",
        params![mission_id],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchanges_round_trip_in_order_and_unknown_kinds_are_refused() {
        let c = store::test_conn();
        let base = NewEvent {
            mission_id: Some("M-001"),
            kind: "handoff",
            from_agent: "claude-code",
            to_agent: "hermes",
            summary: "tests pass, docs left",
            detail: "{\"next_step\":\"write docs\"}",
            ..Default::default()
        };
        record_in(&c, &base).unwrap();
        record_in(
            &c,
            &NewEvent {
                kind: "ask",
                summary: "which port?",
                duration_ms: Some(1200),
                ..base.clone()
            },
        )
        .unwrap();
        record_in(
            &c,
            &NewEvent {
                mission_id: Some("M-002"),
                kind: "note",
                ..base.clone()
            },
        )
        .unwrap();
        assert!(record_in(
            &c,
            &NewEvent {
                kind: "gossip",
                ..base.clone()
            }
        )
        .is_err());
        let m1 = for_mission_in(&c, "M-001").unwrap();
        assert_eq!(m1.len(), 2);
        assert_eq!(m1[0].kind, "handoff");
        assert_eq!(m1[1].duration_ms, Some(1200));
        assert_eq!(recent_in(&c, 10).unwrap().len(), 3);
        assert_eq!(
            recent_in(&c, 1).unwrap()[0].mission_id.as_deref(),
            Some("M-002")
        );
    }
}
