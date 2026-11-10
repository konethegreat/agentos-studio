//! `studio.db`: the small SQLite store for data Hermes has no concept of (workspaces,
//! nested folders, chat placement, tags, attachments, permission policies, the audit log
//! and update preferences). Hermes sessions stay in Hermes's own `state.db`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

const SCHEMA_VERSION: i64 = 11;

const MIGRATION_1: &str = "
CREATE TABLE IF NOT EXISTS workspaces (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    color      TEXT,
    sort       INTEGER NOT NULL DEFAULT 0,
    archived   INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS folders (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    parent_id    TEXT REFERENCES folders(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    path         TEXT,
    project_slug TEXT,
    pinned       INTEGER NOT NULL DEFAULT 0,
    archived     INTEGER NOT NULL DEFAULT 0,
    sort         INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_folders_parent ON folders(parent_id);
CREATE TABLE IF NOT EXISTS chat_meta (
    session_id   TEXT PRIMARY KEY,
    folder_id    TEXT REFERENCES folders(id) ON DELETE SET NULL,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE SET NULL,
    pinned       INTEGER NOT NULL DEFAULT 0,
    archived     INTEGER NOT NULL DEFAULT 0,
    updated_at   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS tags (
    name  TEXT PRIMARY KEY,
    color TEXT
);
CREATE TABLE IF NOT EXISTS item_tags (
    item_kind TEXT NOT NULL,
    item_id   TEXT NOT NULL,
    tag       TEXT NOT NULL REFERENCES tags(name) ON DELETE CASCADE,
    PRIMARY KEY (item_kind, item_id, tag)
);
CREATE TABLE IF NOT EXISTS attachments (
    id         TEXT PRIMARY KEY,
    owner_kind TEXT NOT NULL,
    owner_id   TEXT NOT NULL,
    path       TEXT NOT NULL,
    label      TEXT,
    added_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_attachments_owner ON attachments(owner_kind, owner_id);
CREATE TABLE IF NOT EXISTS permissions (
    action TEXT PRIMARY KEY,
    policy TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS audit (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    at      INTEGER NOT NULL,
    action  TEXT NOT NULL,
    target  TEXT NOT NULL,
    detail  TEXT NOT NULL,
    outcome TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// v0.4: session branches (a chat that Hermes or the user spun off from another) and the
/// skills attached to a chat. Both are Studio concepts; Hermes keeps the sessions themselves.
const MIGRATION_2: &str = "
CREATE TABLE IF NOT EXISTS session_links (
    child_session_id  TEXT PRIMARY KEY,
    parent_session_id TEXT,
    project_slug      TEXT,
    title             TEXT,
    created_at        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_session_links_parent ON session_links(parent_session_id);
CREATE TABLE IF NOT EXISTS chat_skills (
    session_id TEXT NOT NULL,
    skill      TEXT NOT NULL,
    PRIMARY KEY (session_id, skill)
);
";

/// What each chat turn read from and wrote to memory.
const MIGRATION_3: &str = "
CREATE TABLE IF NOT EXISTS memory_ledger (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    turn       INTEGER NOT NULL,
    op         TEXT NOT NULL,
    target     TEXT NOT NULL,
    path       TEXT NOT NULL DEFAULT '',
    tool       TEXT NOT NULL DEFAULT '',
    at         INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memory_ledger_session ON memory_ledger(session_id, turn);
CREATE INDEX IF NOT EXISTS idx_memory_ledger_at ON memory_ledger(at);
";

const MIGRATION_4: &str = "
CREATE TABLE IF NOT EXISTS missions (
    id             TEXT PRIMARY KEY,
    repo           TEXT NOT NULL,
    goal           TEXT NOT NULL,
    acceptance     TEXT NOT NULL DEFAULT '',
    role           TEXT NOT NULL,
    agent          TEXT NOT NULL DEFAULT 'hermes',
    status         TEXT NOT NULL DEFAULT 'queued',
    provider       TEXT NOT NULL DEFAULT '',
    model          TEXT NOT NULL DEFAULT '',
    budget_usd     REAL NOT NULL DEFAULT 0,
    spent_usd      REAL NOT NULL DEFAULT 0,
    task_id        TEXT,
    reviewer       TEXT,
    review_verdict TEXT,
    review_note    TEXT,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_missions_status ON missions(status, updated_at);
";

/// Tests that point the store or the index at a temp directory must not overlap: the paths are
/// process-wide environment variables. One lock, shared by every such test.
#[cfg(test)]
pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

const MIGRATION_5: &str = "
ALTER TABLE missions ADD COLUMN pr_url TEXT;
";

const MIGRATION_6: &str = "
CREATE TABLE IF NOT EXISTS sync_ledger (
    vault_path   TEXT NOT NULL,
    notion_id    TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    synced_at    INTEGER NOT NULL,
    direction    TEXT NOT NULL DEFAULT 'out',
    PRIMARY KEY (vault_path, direction)
);
";

const MIGRATION_7: &str = "
CREATE TABLE IF NOT EXISTS tool_policy (
    server TEXT NOT NULL,
    tool   TEXT NOT NULL,
    policy TEXT NOT NULL DEFAULT 'allow',
    PRIMARY KEY (server, tool)
);
";

/// `hermes verify --json` result recorded on a mission before review.
const MIGRATION_8: &str = "
ALTER TABLE missions ADD COLUMN verify_json TEXT;
";

/// Phase C: every exchange between agents (handoffs, asks and answers, council positions,
/// assignments) as one log the run timeline reads, plus the mission kind (`work` or `decision`).
const MIGRATION_9: &str = "
CREATE TABLE IF NOT EXISTS agent_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    at          INTEGER NOT NULL,
    mission_id  TEXT,
    kind        TEXT NOT NULL,
    from_agent  TEXT NOT NULL DEFAULT '',
    to_agent    TEXT NOT NULL DEFAULT '',
    summary     TEXT NOT NULL DEFAULT '',
    detail      TEXT NOT NULL DEFAULT '',
    duration_ms INTEGER,
    cost_usd    REAL
);
CREATE INDEX IF NOT EXISTS idx_agent_events_mission ON agent_events(mission_id, at);
CREATE INDEX IF NOT EXISTS idx_agent_events_at ON agent_events(at);
";

const MIGRATION_9_KIND: &str = "
ALTER TABLE missions ADD COLUMN kind TEXT NOT NULL DEFAULT 'work';
";

/// Phase D: goals (an outcome with runnable done checks and an autonomy level), the escalations
/// that are the only questions Studio asks the owner, and the goal a mission belongs to. Additive:
/// a 0.13 build still opens the database.
/// Phase G 10: one immutable row per context compaction an agent recorded. Additive.
const MIGRATION_11: &str = "
CREATE TABLE IF NOT EXISTS context_events (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    at             INTEGER NOT NULL,
    agent          TEXT NOT NULL,
    session_id     TEXT NOT NULL,
    source         TEXT NOT NULL,
    first_msg      INTEGER NOT NULL,
    last_msg       INTEGER NOT NULL,
    dropped_msgs   INTEGER NOT NULL DEFAULT 0,
    tokens_before  INTEGER NOT NULL DEFAULT 0,
    tokens_after   INTEGER NOT NULL DEFAULT 0,
    model          TEXT NOT NULL DEFAULT '',
    prompt_version TEXT NOT NULL DEFAULT '',
    commit_sha     TEXT NOT NULL DEFAULT '',
    note           TEXT NOT NULL DEFAULT '',
    hash           TEXT NOT NULL DEFAULT '',
    recorded_at    INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    UNIQUE (agent, session_id, first_msg, last_msg)
);
CREATE INDEX IF NOT EXISTS context_events_at ON context_events (at);
";

const MIGRATION_10: &str = "
CREATE TABLE IF NOT EXISTS goals (
    id          TEXT PRIMARY KEY,
    outcome     TEXT NOT NULL,
    repos       TEXT NOT NULL DEFAULT '[]',
    checks      TEXT NOT NULL DEFAULT '[]',
    budget_usd  REAL NOT NULL DEFAULT 0,
    deadline    TEXT,
    autonomy    TEXT NOT NULL DEFAULT 'ask',
    status      TEXT NOT NULL DEFAULT 'planning',
    note        TEXT NOT NULL DEFAULT '',
    report      TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS escalations (
    id          TEXT PRIMARY KEY,
    goal_id     TEXT,
    mission_id  TEXT,
    kind        TEXT NOT NULL,
    question    TEXT NOT NULL,
    options     TEXT NOT NULL DEFAULT '[]',
    safe_answer TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'open',
    answer      TEXT,
    answered_by TEXT,
    code_hash   TEXT,
    created_at  INTEGER NOT NULL,
    deadline_at INTEGER NOT NULL,
    answered_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_escalations_status ON escalations(status, created_at);
";

const MIGRATION_10_GOAL: &str = "
ALTER TABLE missions ADD COLUMN goal_id TEXT;
";

pub fn studio_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(crate::settings::home)
        .join("agentos-studio")
}

// Test override, thread-local so parallel tests cannot see each other's store.
#[cfg(test)]
thread_local! {
    static TEST_DB: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn set_test_db(path: Option<PathBuf>) {
    TEST_DB.with(|c| *c.borrow_mut() = path);
}

pub fn db_path() -> PathBuf {
    #[cfg(test)]
    {
        let mut found = None;
        TEST_DB.with(|c| found = c.borrow().clone());
        if let Some(p) = found {
            return p;
        }
    }
    if let Ok(p) = std::env::var("AGENTOS_STUDIO_DB") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    studio_dir().join("studio.db")
}

/// Open (and migrate) the store at the default location.
pub fn open() -> Result<Connection> {
    open_at(&db_path())
}

pub fn open_at(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let c = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    c.busy_timeout(Duration::from_secs(5))?;
    c.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
    migrate(&c)?;
    Ok(c)
}

fn migrate(c: &Connection) -> Result<()> {
    let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v < 1 {
        c.execute_batch(MIGRATION_1)?;
    }
    if v < 2 {
        c.execute_batch(MIGRATION_2)?;
    }
    if v < 3 {
        c.execute_batch(MIGRATION_3)?;
    }
    if v < 4 {
        c.execute_batch(MIGRATION_4)?;
    }
    if v < 5 {
        // SQLite has no ADD COLUMN IF NOT EXISTS; a fresh database already has the column path.
        let _ = c.execute_batch(MIGRATION_5);
    }
    if v < 6 {
        c.execute_batch(MIGRATION_6)?;
    }
    if v < 7 {
        c.execute_batch(MIGRATION_7)?;
    }
    if v < 8 {
        let _ = c.execute_batch(MIGRATION_8);
    }
    if v < 9 {
        c.execute_batch(MIGRATION_9)?;
        let _ = c.execute_batch(MIGRATION_9_KIND);
    }
    if v < 10 {
        c.execute_batch(MIGRATION_10)?;
        let _ = c.execute_batch(MIGRATION_10_GOAL);
    }
    if v < 11 {
        c.execute_batch(MIGRATION_11)?;
    }
    if v < SCHEMA_VERSION {
        c.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
    }
    Ok(())
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn kv_get(c: &Connection, key: &str) -> Result<Option<String>> {
    Ok(
        c.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()?,
    )
}

pub fn kv_set(c: &Connection, key: &str, value: &str) -> Result<()> {
    c.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

pub fn kv_delete(c: &Connection, key: &str) -> Result<()> {
    c.execute("DELETE FROM kv WHERE key = ?1", [key])?;
    Ok(())
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

#[cfg(test)]
pub fn test_conn() -> Connection {
    let c = Connection::open_in_memory().expect("memory db");
    c.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("pragma");
    migrate(&c).expect("migrate");
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_once_and_round_trips_kv() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("studio.db");
        let c = open_at(&p).unwrap();
        kv_set(&c, "a", "1").unwrap();
        kv_set(&c, "a", "2").unwrap();
        drop(c);
        let c = open_at(&p).unwrap();
        assert_eq!(kv_get(&c, "a").unwrap().as_deref(), Some("2"));
        kv_delete(&c, "a").unwrap();
        assert_eq!(kv_get(&c, "a").unwrap(), None);
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }
}
