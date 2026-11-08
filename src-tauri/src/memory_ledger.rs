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

/// Attach a run to its session once the id is known (a new chat learns it before the turn).
pub fn rename_run(run_id: &str, session: &str) {
    if let Ok(mut m) = runs().lock() {
        if let Some(c) = m.get_mut(run_id) {
            c.session = session.to_string();
        }
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

/// Hermes' built-in `memory` tool (MEMORY.md and USER.md under the Hermes home): its action decides
/// read or write, and its `target` (`memory` or `user`) names the file.
pub fn classify_hermes_memory(input: Option<&Value>) -> (&'static str, String) {
    let action = input
        .and_then(|v| v.get("action"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let op = if matches!(
        action,
        "add" | "replace" | "remove" | "batch" | "delete" | "update"
    ) {
        "write"
    } else {
        "read"
    };
    let file = match input.and_then(|v| v.get("target")).and_then(Value::as_str) {
        Some("user") => "USER.md",
        _ => "MEMORY.md",
    };
    (op, file.to_string())
}

/// Called from the chat event loop for every tool event.
pub fn note_event(run_id: &str, tool: &str, input: Option<&Value>, output: Option<&str>) {
    let short = tool.rsplit("__").next().unwrap_or(tool);
    let hermes_memory = short.eq_ignore_ascii_case("memory");
    let (op, target, path) = if hermes_memory {
        let (op, file) = classify_hermes_memory(input);
        (op, "hermes", file)
    } else {
        let Some((op, target)) = classify(tool) else {
            return;
        };
        (op, target, path_of(input))
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
        path,
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

    #[test]
    fn hermes_memory_tool_is_ledgered_by_action_and_file() {
        let add = serde_json::json!({"action": "add", "target": "user", "content": "x"});
        assert_eq!(
            classify_hermes_memory(Some(&add)),
            ("write", "USER.md".to_string())
        );
        let batch = serde_json::json!({"action": "batch", "target": "memory", "operations": []});
        assert_eq!(
            classify_hermes_memory(Some(&batch)),
            ("write", "MEMORY.md".to_string())
        );
        let read = serde_json::json!({"action": "read"});
        assert_eq!(
            classify_hermes_memory(Some(&read)),
            ("read", "MEMORY.md".to_string())
        );
        assert_eq!(
            classify("memory"),
            None,
            "the generic classifier does not claim it"
        );
    }

    // The store location is process-global; the shared lock lives in `store`.

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
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
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
        crate::store::set_test_db(None);
    }

    #[test]
    fn rows_attach_to_session_and_turn() {
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
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
        crate::store::set_test_db(None);
    }
}
