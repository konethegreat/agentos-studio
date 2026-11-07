//! Ranked recall over the vault.
//!
//! Markdown is the source of truth; `memory.db` is a disposable SQLite FTS5 index over it, kept in
//! step by the existing vault watcher. Deleting the file costs one reindex and nothing else.
//!
//! Ranking is `bm25` plus a slow recency decay plus wikilink in-degree, with superseded notes
//! pushed to the end: a note that has been replaced should never outrank its replacement, however
//! well its wording matches.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::vault::{Doc, MemKind, Vault};

const CHUNK_CHARS: usize = 900;
const SCAN_DIRS: [&str; 3] = ["01_Projects", "03_Memory", "02_Agent_Bus/Completed"];

/// One index per vault, so two vaults never share ranks and a test vault cannot pollute the real one.
pub fn db_path_for(vault_root: &Path) -> PathBuf {
    let digest = format!(
        "{:x}",
        md5_like(vault_root.to_string_lossy().to_lowercase().as_bytes())
    );
    // The index location must not follow AGENTOS_STUDIO_DB, which tests point at a temp store.
    // AGENTOS_STUDIO_MEMORY_DIR keeps test indexes out of the user's app data.
    let base = std::env::var("AGENTOS_STUDIO_MEMORY_DIR")
        .ok()
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(crate::store::studio_dir);
    base.join(format!("memory-{}.db", &digest[..10]))
}

/// Small stable hash. Not cryptographic; only needs to separate vault paths.
fn md5_like(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

pub fn open(vault_root: &Path) -> Result<Connection> {
    let path = db_path_for(vault_root);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).ok();
    }
    let c = Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
    c.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS files (
             path TEXT PRIMARY KEY,
             mtime INTEGER NOT NULL,
             size INTEGER NOT NULL,
             title TEXT,
             kind TEXT,
             status TEXT,
             repo TEXT,
             in_degree INTEGER NOT NULL DEFAULT 0
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
             path UNINDEXED,
             line UNINDEXED,
             text,
             tokenize = 'unicode61'
         );",
    )?;
    Ok(c)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteRow {
    pub path: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub repo: Option<String>,
    pub in_degree: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Chunk {
    pub path: String,
    pub line: usize,
    pub text: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub repo: Option<String>,
    pub score: f64,
}

fn mtime_of(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Break a note into chunks on blank lines, never splitting a paragraph, and remember the line
/// each chunk starts on so a hit can be opened at the right place.
pub fn chunk_body(body: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut start = 1usize;
    for (i, line) in body.lines().enumerate() {
        let n = i + 1;
        if buf.is_empty() {
            start = n;
        }
        if line.trim().is_empty() && !buf.trim().is_empty() {
            out.push((start, buf.trim().to_string()));
            buf.clear();
            continue;
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(line);
        if buf.len() >= CHUNK_CHARS {
            out.push((start, buf.trim().to_string()));
            buf.clear();
        }
    }
    if !buf.trim().is_empty() {
        out.push((start, buf.trim().to_string()));
    }
    out
}

/// `[[Target]]` and `[[Target|label]]` targets in a note, lowercased file stems.
pub fn links(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("[[") {
        rest = &rest[i + 2..];
        let Some(j) = rest.find("]]") else { break };
        let inner = &rest[..j];
        let target = inner.split('|').next().unwrap_or(inner).trim();
        let stem = target
            .rsplit('/')
            .next()
            .unwrap_or(target)
            .trim_end_matches(".md")
            .to_lowercase();
        if !stem.is_empty() {
            out.push(stem);
        }
        rest = &rest[j + 2..];
    }
    out
}

fn scan_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in SCAN_DIRS {
        let base = root.join(dir);
        if !base.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&base).into_iter().flatten() {
            let p = entry.path();
            if entry.file_type().is_file() && p.extension().and_then(|e| e.to_str()) == Some("md") {
                out.push(p.to_path_buf());
            }
        }
    }
    out
}

/// Index anything new or changed. Returns how many notes were read.
pub fn refresh(v: &Vault) -> Result<usize> {
    let mut c = open(&v.root)?;
    let tx = c.transaction()?;
    let mut touched = 0usize;

    let mut known: HashMap<String, (i64, i64)> = HashMap::new();
    {
        let mut st = tx.prepare("SELECT path, mtime, size FROM files")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?),
            ))
        })?;
        for row in rows.flatten() {
            known.insert(row.0, row.1);
        }
    }

    let mut degrees: HashMap<String, i64> = HashMap::new();
    let mut seen: Vec<String> = Vec::new();

    for path in scan_files(&v.root) {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(&v.root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.display().to_string());
        let mtime = mtime_of(&meta);
        let size = meta.len() as i64;
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };

        for target in links(&raw) {
            *degrees.entry(target).or_insert(0) += 1;
        }
        seen.push(rel.clone());

        if known.get(&rel) == Some(&(mtime, size)) {
            continue;
        }
        let doc = Doc::parse(&raw);
        let title = doc
            .get("title")
            .map(str::to_string)
            .or_else(|| doc.get("topic").map(str::to_string))
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().replace('_', " "))
                    .unwrap_or_default()
            });
        let kind = MemKind::parse(doc.get("type").unwrap_or(""))
            .as_str()
            .to_string();
        let status = doc.get("status").unwrap_or("active").to_string();
        let repo = doc.get("repo").map(str::to_string);

        tx.execute("DELETE FROM chunks WHERE path = ?1", params![rel])?;
        for (line, text) in chunk_body(&doc.body) {
            tx.execute(
                "INSERT INTO chunks (path, line, text) VALUES (?1, ?2, ?3)",
                params![rel, line as i64, text],
            )?;
        }
        tx.execute(
            "INSERT INTO files (path, mtime, size, title, kind, status, repo, in_degree)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)
             ON CONFLICT(path) DO UPDATE SET
               mtime = excluded.mtime, size = excluded.size, title = excluded.title,
               kind = excluded.kind, status = excluded.status, repo = excluded.repo",
            params![rel, mtime, size, title, kind, status, repo],
        )?;
        touched += 1;
    }

    // Notes deleted from disk disappear from the index with them.
    let stale: Vec<String> = known
        .keys()
        .filter(|p| !seen.contains(*p))
        .cloned()
        .collect();
    for p in stale {
        tx.execute("DELETE FROM chunks WHERE path = ?1", params![p])?;
        tx.execute("DELETE FROM files WHERE path = ?1", params![p])?;
    }

    for (stem, count) in degrees {
        tx.execute(
            "UPDATE files SET in_degree = ?1 WHERE lower(path) LIKE ?2",
            params![count, format!("%/{stem}.md")],
        )?;
    }
    tx.commit()?;
    Ok(touched)
}

/// Notes in the index with their link in-degree, newest first, for the Memory tab.
pub fn notes(v: &Vault) -> Result<Vec<NoteRow>> {
    refresh(v)?;
    let c = open(&v.root)?;
    let mut st = c.prepare(
        "SELECT path, COALESCE(title,''), COALESCE(kind,'fact'), COALESCE(status,'active'),
                repo, COALESCE(in_degree,0)
         FROM files ORDER BY mtime DESC LIMIT 500",
    )?;
    let rows = st.query_map([], |r| {
        Ok(NoteRow {
            path: r.get(0)?,
            title: r.get(1)?,
            kind: r.get(2)?,
            status: r.get(3)?,
            repo: r.get(4)?,
            in_degree: r.get(5)?,
        })
    })?;
    Ok(rows.flatten().collect())
}

/// Full rebuild, used when the database is missing or the schema moved.
pub fn rebuild(v: &Vault) -> Result<usize> {
    if let Ok(c) = open(&v.root) {
        c.execute_batch("DELETE FROM chunks; DELETE FROM files;")
            .ok();
    }
    refresh(v)
}

fn fts_query(query: &str) -> String {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|t| t.chars().count() >= 2)
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect();
    if terms.is_empty() {
        "\"\"".to_string()
    } else {
        terms.join(" OR ")
    }
}

/// Ranked chunks. `repo` tilts the ranking toward one repository without excluding the rest.
pub fn recall(
    v: &Vault,
    query: &str,
    repo: Option<&str>,
    k: usize,
    max_tokens: usize,
) -> Result<Vec<Chunk>> {
    if query.trim().is_empty() {
        anyhow::bail!("query must not be empty");
    }
    refresh(v)?;
    let c = open(&v.root)?;
    let sql = "SELECT c.path, c.line, c.text, bm25(chunks) AS rank,
                      COALESCE(f.title, ''), COALESCE(f.kind, 'fact'), COALESCE(f.status, 'active'),
                      f.repo, COALESCE(f.in_degree, 0), COALESCE(f.mtime, 0)
               FROM chunks c LEFT JOIN files f ON f.path = c.path
               WHERE chunks MATCH ?1
               ORDER BY rank LIMIT ?2";
    let mut st = c.prepare(sql)?;
    let rows = st.query_map(params![fts_query(query), (k * 6).max(12) as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, f64>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, i64>(8)?,
            r.get::<_, i64>(9)?,
        ))
    })?;

    let now = now_secs();
    let mut scored: Vec<Chunk> = Vec::new();
    for row in rows.flatten() {
        let (path, line, text, rank, title, kind, status, r, degree, mtime) = row;
        let age_days = ((now - mtime).max(0) as f64) / 86_400.0;
        let mut score = -rank;
        score += 0.35 * (-age_days / 30.0).exp();
        score += 0.12 * ((1 + degree).max(1) as f64).ln();
        if let (Some(want), Some(have)) = (repo, r.as_deref()) {
            if want.eq_ignore_ascii_case(have) {
                score += 0.2;
            }
        }
        scored.push(Chunk {
            path,
            line: line as usize,
            text,
            title,
            kind,
            status,
            repo: r,
            score,
        });
    }
    // Superseded notes always sink, whatever their wording score. Order within each band is score.
    scored.sort_by(|a, b| {
        let sa = a.status == "superseded";
        let sb = b.status == "superseded";
        sa.cmp(&sb).then_with(|| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    // Trim to a token budget, roughly four characters per token.
    let budget = max_tokens.max(200) * 4;
    let mut used = 0usize;
    let mut out = Vec::new();
    for mut ch in scored.into_iter() {
        if out.len() >= k.max(1) {
            break;
        }
        if used >= budget {
            break;
        }
        let room = budget - used;
        if ch.text.len() > room {
            ch.text = ch.text.chars().take(room).collect::<String>();
            ch.text.push_str("\n[truncated]");
        }
        used += ch.text.len();
        out.push(ch);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_vault() -> (tempfile::TempDir, Vault) {
        let d = tempfile::tempdir().unwrap();
        // Keep the FTS database inside the test directory, never in %APPDATA%.
        std::env::set_var("AGENTOS_STUDIO_MEMORY_DIR", d.path());
        let v = Vault::new(d.path());
        v.scaffold().unwrap();
        (d, v)
    }

    #[test]
    fn chunks_keep_paragraphs_and_line_numbers() {
        let body =
            "# Title\n\nFirst paragraph about ports.\n\nSecond paragraph\nstill second.\n\nThird.";
        let chunks = chunk_body(body);
        assert_eq!(
            chunks.len(),
            4,
            "heading and each paragraph is its own chunk"
        );
        assert_eq!(chunks[0], (1, "# Title".to_string()));
        assert_eq!(chunks[1].0, 3);
        assert!(chunks[2].1.contains("still second"));
    }

    #[test]
    fn links_are_stems_only() {
        assert_eq!(
            links("see [[DECISION_LOG]] and [[03_Memory/Port_Number|ports]] and [[a/b/Note.md]]"),
            vec!["decision_log", "port_number", "note"]
        );
    }

    #[test]
    fn recall_finds_written_notes_and_buries_superseded() {
        let (_d, v) = temp_vault();
        v.write_typed_memory(
            "Guard Port",
            "The single instance guard listens on 47823.",
            "ops",
            &crate::vault::MemMeta {
                kind: Some(MemKind::Fact),
                ..Default::default()
            },
        )
        .unwrap();
        v.write_typed_memory(
            "Guard Port v2",
            "Replaced: the guard now listens on 47824.",
            "ops",
            &crate::vault::MemMeta {
                kind: Some(MemKind::Decision),
                supersedes: Some("Guard Port".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let hits = recall(&v, "guard listens", None, 8, 1500).unwrap();
        for h in &hits {
            println!("{:.3} [{}] {}:{}", h.score, h.status, h.path, h.line);
        }
        assert!(!hits.is_empty(), "the note is indexed");
        assert!(
            hits.iter().any(|h| h.path.contains("Guard_Port_v2")),
            "the replacement is found"
        );
        // Ranking, not filtering: whatever matches best wins, and anything superseded sinks.
        let first_superseded = hits.iter().position(|h| h.status == "superseded");
        if let Some(i) = first_superseded {
            assert!(
                hits[i..].iter().all(|h| h.status == "superseded"),
                "every chunk after the first superseded one is also superseded"
            );
        }
    }

    #[test]
    fn recall_respects_the_token_budget() {
        let (_d, v) = temp_vault();
        let long: String = "tokens. ".repeat(400);
        v.write_typed_memory(
            "Long Note",
            &long,
            "test",
            &crate::vault::MemMeta {
                kind: Some(MemKind::Fact),
                ..Default::default()
            },
        )
        .unwrap();
        let hits = recall(&v, "tokens", None, 5, 300).unwrap();
        let total: usize = hits.iter().map(|h| h.text.len()).sum();
        assert!(total <= 300 * 4 + 64, "got {total} chars");
    }
}
