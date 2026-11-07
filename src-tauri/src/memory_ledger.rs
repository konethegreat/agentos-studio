//! What a chat turn read from, and wrote to, memory.
//!
//! The events are the ones `chat_stream` already parses: every `tool_use` block names a tool and
//! carries its input. Recording is derived from that, never from the model's prose, so the strip
//! under a turn is evidence rather than a claim.
//!
//! Rows live in `studio.db` (`memory_ledger`), keyed by session and turn, which is what the UI
//! needs to attach them to the right message.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;

use crate::store;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub session_id: String,
    pub turn: u32,
    pub op: String,
    pub target: String,
    pub path: String,
    pub tool: String,
    pub at: i64,
}

/// One run maps to one user turn, so the turn number travels with the run id.
struct RunCtx {
    session: String,
    turn: u32,
}

fn runs() -> &'static Mutex<HashMap<String, RunCtx>> {
    static RUNS: OnceLock<Mutex<HashMap<String, RunCtx>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn begin_run(run_id: &str, session: &str, turn: u32) {
    if let Ok(mut m) = runs().lock() {
        m.insert(
            run_id.to_string(),
            RunCtx {
                session: session.to_string(),
                turn,
            },
        );
    }
}

pub fn end_run(run_id: &str) {
    if let Ok(mut m) = runs().lock() {
        m.remove(run_id);
    }
}

/// Read or write, from the tool's own name.
pub fn classify(tool: &str) -> Option<(&'static str, &'static str)> {
    let name = tool.rsplit("__").next().unwrap_or(tool).to_lowercase();
    let notion = name.contains("notion");
    if notion {
        // Notion tool names are verbs with a `notion_` prefix: notion_create_pages, notion_search.
        let write = [
            "create", "update", "delete", "append", "move", "upload", "replace",
        ]
        .iter()
        .any(|k| name.contains(k));
        return Some((if write { "write" } else { "read" }, "notion"));
    }
    let read = matches!(
        name.as_str(),
        "recall"
            | "search_shared_memory"
            | "project_brief"
            | "read_file"
            | "read"
            | "view"
            | "list_dir"
            | "glob"
            | "grep"
            | "search_files"
            | "fetch"
            | "query_data_sources"
            | "search"
    );
    let write = matches!(
        name.as_str(),
        "remember"
            | "write_shared_memory"
            | "write_file"
            | "edit_file"
            | "str_replace"
            | "apply_patch"
            | "create_page"
            | "create_pages"
            | "update_page"
            | "create_database"
            | "append"
    );
    if !(read || write) {
        return None;
    }
    let target = if matches!(
        name.as_str(),
        "recall" | "remember" | "write_shared_memory" | "search_shared_memory" | "project_brief"
    ) {
        "vault"
    } else {
        "file"
    };
    Some((if write { "write" } else { "read" }, target))
}

/// The most useful path-ish field in a tool input.
pub fn path_of(input: Option<&Value>) -> String {
    let Some(v) = input else { return String::new() };
    for key in [
        "path",
        "file_path",
        "filePath",
        "topic",
        "repo",
        "query",
        "page_id",
        "database_id",
        "url",
    ] {
        if let Some(s) = v.get(key).and_then(Value::as_str) {
            let s = s.trim();
            if !s.is_empty() {
                return s.chars().take(200).collect();
            }
        }
    }
    String::new()
}

fn record(entry: &Entry) -> Result<()> {
    let c = store::open()?;
    insert(&c, entry)
}

fn insert(c: &Connection, e: &Entry) -> Result<()> {
    c.execute(
        "INSERT INTO memory_ledger (session_id, turn, op, target, path, tool, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![e.session_id, e.turn, e.op, e.target, e.path, e.tool, e.at],
    )?;
    Ok(())
}

/// Called from the chat event loop for every tool event.
pub fn note_event(run_id: &str, tool: &str, input: Option<&Value>, output: Option<&str>) {
    let Some((op, target)) = classify(tool) else {
        return;
    };
    // A tool that errored did not read or write anything worth claiming.
    if let Some(out) = output {
        let low = out.to_lowercase();
        if low.contains("error") && (low.contains("not found") || low.contains("failed")) {
            return;
        }
    }
    let ctx = {
        let Ok(m) = runs().lock() else { return };
        let Some(c) = m.get(run_id) else { return };
        (c.session.clone(), c.turn)
    };
    let entry = Entry {
        session_id: ctx.0,
        turn: ctx.1,
        op: op.to_string(),
        target: target.to_string(),
        path: path_of(input),
        tool: tool.rsplit("__").next().unwrap_or(tool).to_string(),
        at: chrono::Utc::now().timestamp(),
    };
    let _ = record(&entry);
}

pub fn list(session_id: &str, limit: usize) -> Result<Vec<Entry>> {
    let c = store::open()?;
    let mut st = c.prepare(
        "SELECT session_id, turn, op, target, path, tool, at
         FROM memory_ledger WHERE session_id = ?1 ORDER BY turn ASC, at ASC, rowid ASC LIMIT ?2",
    )?;
    let rows = st.query_map(params![session_id, limit as i64], |r| {
        Ok(Entry {
            session_id: r.get(0)?,
            turn: r.get::<_, i64>(1)? as u32,
            op: r.get(2)?,
            target: r.get(3)?,
            path: r.get(4)?,
            tool: r.get(5)?,
            at: r.get(6)?,
        })
    })?;
    Ok(rows.flatten().collect())
}

/// Recent activity across all sessions, for the Memory tab.
pub fn recent(limit: usize) -> Result<Vec<Entry>> {
    let c = store::open()?;
    let mut st = c.prepare(
        "SELECT session_id, turn, op, target, path, tool, at
         FROM memory_ledger ORDER BY at DESC, rowid DESC LIMIT ?1",
    )?;
    let rows = st.query_map(params![limit as i64], |r| {
        Ok(Entry {
            session_id: r.get(0)?,
            turn: r.get::<_, i64>(1)? as u32,
            op: r.get(2)?,
            target: r.get(3)?,
            path: r.get(4)?,
            tool: r.get(5)?,
            at: r.get(6)?,
        })
    })?;
    Ok(rows.flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store location is process-global, so DB tests must not overlap.
    static DB_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn classification_is_by_tool_not_by_prose() {
        assert_eq!(
            classify("mcp__agentos_hub__recall"),
            Some(("read", "vault"))
        );
        assert_eq!(classify("remember"), Some(("write", "vault")));
        assert_eq!(classify("project_brief"), Some(("read", "vault")));
        assert_eq!(classify("read_file"), Some(("read", "file")));
        assert_eq!(classify("write_file"), Some(("write", "file")));
        assert_eq!(
            classify("mcp__notion__notion_create_pages"),
            Some(("write", "notion"))
        );
        assert_eq!(
            classify("mcp__notion__notion_search"),
            Some(("read", "notion"))
        );
        assert_eq!(classify("bash"), None, "a shell is not memory");
        assert_eq!(classify("web_search"), None);
    }

    #[test]
    fn path_prefers_the_most_specific_field() {
        let v = serde_json::json!({"path": "03_Memory/Port.md", "query": "ports"});
        assert_eq!(path_of(Some(&v)), "03_Memory/Port.md");
        let v = serde_json::json!({"topic": "Guard Port"});
        assert_eq!(path_of(Some(&v)), "Guard Port");
        assert_eq!(path_of(None), "");
    }

    #[test]
    fn events_become_rows_for_the_right_turn() {
        let _g = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("AGENTOS_STUDIO_DB", d.path().join("studio.db"));
        begin_run("run-a", "sess-a", 3);
        note_event(
            "run-a",
            "mcp__agentos_hub__recall",
            Some(&serde_json::json!({"query": "ports"})),
            Some("3 chunks"),
        );
        note_event(
            "run-a",
            "mcp__agentos_hub__remember",
            Some(&serde_json::json!({"topic": "Guard Port"})),
            Some("Memory note created"),
        );
        // A shell command is not memory, and an unknown run records nothing.
        note_event(
            "run-a",
            "bash",
            Some(&serde_json::json!({"command": "ls"})),
            None,
        );
        note_event("run-missing", "recall", None, None);

        let rows = list("sess-a", 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].turn, 3);
        assert_eq!(rows[0].op, "read");
        assert_eq!(rows[0].path, "ports");
        assert_eq!(rows[1].op, "write");
        assert_eq!(rows[1].target, "vault");
        end_run("run-a");
        std::env::remove_var("AGENTOS_STUDIO_DB");
    }

    #[test]
    fn rows_attach_to_session_and_turn() {
        let _g = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("AGENTOS_STUDIO_DB", d.path().join("studio.db"));
        let c = store::open().unwrap();
        let e = Entry {
            session_id: "s1".into(),
            turn: 2,
            op: "read".into(),
            target: "vault".into(),
            path: "03_Memory/X.md".into(),
            tool: "recall".into(),
            at: 1,
        };
        insert(&c, &e).unwrap();
        let got = list("s1", 10).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].turn, 2);
        assert!(list("other", 10).unwrap().is_empty());
        std::env::remove_var("AGENTOS_STUDIO_DB");
    }
}
