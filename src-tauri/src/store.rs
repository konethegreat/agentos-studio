//! `studio.db`: the small SQLite store for data Hermes has no concept of (workspaces,
//! nested folders, chat placement, tags, attachments, permission policies, the audit log
//! and update preferences). Hermes sessions stay in Hermes's own `state.db`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

const SCHEMA_VERSION: i64 = 1;

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

pub fn studio_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(crate::settings::home)
        .join("agentos-studio")
}

pub fn db_path() -> PathBuf {
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
