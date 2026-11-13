//! The context ledger: what each agent session kept and dropped when its context was compacted,
//! and a durable, fixed-schema summary that points back to the transcript and the commit.
//!
//! Hermes and Claude Code keep doing their own compaction. Studio reads what they record
//! (Hermes `state.db` read-only: compacted messages and the summary message that replaced them;
//! Claude Code transcripts: `compact_boundary` records and the summary that follows) and appends
//! one immutable row per compaction to `context_events`. For each event it writes a vault note in
//! `03_Memory/context/` with frontmatter (source transcript, message range, tokens before and
//! after, model, prompt version, commit, hash) and the fixed schema: goal, constraints, key
//! knowledge, artifact trail, file state, recent actions, task state, open tasks, next step,
//! decisions, running jobs, user preferences, security constraints. Anchors (paths, commits, pull
//! requests) are extracted mechanically from the dropped range, so an identifier the agent's own
//! summary lost is still findable. The vault index (FTS5) makes the notes searchable next to the
//! transcripts' own FTS5 index.
//!
//! Workers get the same layer in their worktree: `.agentos/context/<mission>.md` with the pinned
//! facts and the next step, which they read first and update before finishing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Mask old tool output at this share of the window.
pub const MASK_AT: f64 = 0.60;
/// Compact at this share, leaving this reserve free afterwards.
pub const COMPACT_AT: f64 = 0.75;
pub const RESERVE: f64 = 0.15;

// ---------------------------------------------------------------------------------------------
// Messages and the trigger policy
// ---------------------------------------------------------------------------------------------

/// One message of a transcript, reduced to what the ledger reads.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Msg {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub tool_name: Option<String>,
    /// Tool calls an assistant message made: (name, arguments as JSON text).
    pub tool_calls: Vec<(String, String)>,
    pub at: f64,
}

/// A rough token count when the runtime did not record one: four characters per token.
pub fn estimate_tokens(text: &str) -> i64 {
    text.chars().count().div_ceil(4) as i64
}

/// What to do at `used` tokens of a `window`, given whether a tool call is still waiting for its
/// result. Never compacts in the middle of a tool call: that is deferred to the next safe point.
/// Pure.
pub fn stage(used: i64, window: i64, mid_tool_call: bool) -> &'static str {
    if window <= 0 {
        return "unknown";
    }
    let share = used as f64 / window as f64;
    if share >= COMPACT_AT {
        if mid_tool_call {
            "defer"
        } else {
            "compact"
        }
    } else if share >= MASK_AT {
        "mask"
    } else {
        "ok"
    }
}

/// Whether the transcript ends inside a tool call: an assistant message asked for tools and not
/// every call has a result after it. Pure.
pub fn mid_tool_call(msgs: &[Msg]) -> bool {
    let Some(last_call) = msgs.iter().rposition(|m| !m.tool_calls.is_empty()) else {
        return false;
    };
    let asked = msgs[last_call].tool_calls.len();
    let answered = msgs[last_call + 1..]
        .iter()
        .take_while(|m| m.role == "tool")
        .count();
    answered < asked
}

/// The tokens to keep after a compaction at `window`: up to the compact point minus the reserve.
/// Pure.
pub fn target_tokens(window: i64) -> i64 {
    ((COMPACT_AT - RESERVE) * window as f64) as i64
}

fn exit_re() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?i)(?:exit[ _-]?code|exit_code|exit status|returncode|exit)["']?\s*[:=]?\s*(-?\d+)"#)
            .expect("exit pattern")
    })
}

fn path_re() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?:[A-Za-z]:[\\/]|\.{0,2}/)?(?:[\w.\-]+[\\/])+[\w.\-]+\.[A-Za-z0-9]{1,8}"#)
            .expect("path pattern")
    })
}

/// Mask one tool output to a short preview that keeps its first line, every path, the exit
/// codes and error lines. Pure.
pub fn mask_tool_output(tool: &str, text: &str) -> String {
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let first: String = first.chars().take(160).collect();
    let mut keep: Vec<String> = Vec::new();
    for c in exit_re().captures_iter(text) {
        let s = format!("exit {}", &c[1]);
        if !keep.contains(&s) {
            keep.push(s);
        }
    }
    let mut paths: Vec<String> = path_re()
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect();
    paths.sort();
    paths.dedup();
    paths.truncate(12);
    let errors: Vec<String> = text
        .lines()
        .filter(|l| {
            let l = l.to_lowercase();
            l.contains("error") || l.contains("failed") || l.contains("panicked")
        })
        .take(3)
        .map(|l| l.trim().chars().take(160).collect())
        .collect();
    let mut out = format!(
        "[{tool} output masked, {} chars] {first}",
        text.chars().count()
    );
    if !keep.is_empty() {
        out.push_str(&format!(" | {}", keep.join(", ")));
    }
    if !paths.is_empty() {
        out.push_str(&format!(" | paths: {}", paths.join(", ")));
    }
    for e in errors {
        out.push_str(&format!(" | {e}"));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The fixed-schema summary
// ---------------------------------------------------------------------------------------------

pub const SECTIONS: [&str; 13] = [
    "Goal",
    "Constraints",
    "Key knowledge",
    "Artifact trail",
    "File state",
    "Recent actions",
    "Task state",
    "Open tasks",
    "Next step",
    "Decisions",
    "Running jobs",
    "User preferences",
    "Security constraints",
];

/// Where a heading in an agent's own summary belongs in the fixed schema. Pure.
fn section_for(heading: &str) -> Option<&'static str> {
    let h = heading.to_lowercase();
    let pick = |words: &[&str]| words.iter().any(|w| h.contains(w));
    Some(
        if pick(&["goal", "objective", "primary request", "intent"]) {
            "Goal"
        } else if pick(&["security", "secret", "credential"]) {
            "Security constraints"
        } else if pick(&["preference", "user message"]) {
            "User preferences"
        } else if pick(&["constraint", "rule"]) {
            "Constraints"
        } else if pick(&["decision", "resolved question"]) {
            "Decisions"
        } else if pick(&["relevant file", "file state", "files and code"]) {
            "File state"
        } else if pick(&["completed", "artifact", "changes made"]) {
            "Artifact trail"
        } else if pick(&["active state", "current work", "task state", "in progress"]) {
            "Task state"
        } else if pick(&["blocked", "pending", "open", "remaining", "todo"]) {
            "Open tasks"
        } else if pick(&["next step", "optional next"]) {
            "Next step"
        } else if pick(&["running", "background", "process"]) {
            "Running jobs"
        } else if pick(&["recent", "session log", "errors"]) {
            "Recent actions"
        } else if pick(&[
            "context",
            "knowledge",
            "technical",
            "concept",
            "unverified",
            "hypothes",
        ]) {
            "Key knowledge"
        } else {
            return None;
        },
    )
}

/// Split an agent's summary into the fixed schema by its own headings. Text before any heading,
/// and under headings that map to nothing, goes to Key knowledge. Pure.
pub fn map_summary(text: &str) -> BTreeMap<&'static str, String> {
    let mut out: BTreeMap<&'static str, String> = BTreeMap::new();
    let mut current: &'static str = "Key knowledge";
    for line in text.lines() {
        let t = line.trim();
        let heading = t
            .strip_prefix("## ")
            .or_else(|| t.strip_prefix("### "))
            .or_else(|| {
                t.strip_prefix("**")
                    .and_then(|r| r.strip_suffix("**"))
                    .or_else(|| {
                        t.strip_suffix(':').filter(|h| {
                            h.len() < 40 && !h.contains(' ') || h.split(' ').count() <= 4
                        })
                    })
            });
        if let Some(h) = heading {
            if let Some(s) = section_for(h.trim_end_matches(':')) {
                current = s;
                continue;
            }
        }
        if t.is_empty() {
            continue;
        }
        let e = out.entry(current).or_default();
        e.push_str(line.trim_end());
        e.push('\n');
    }
    out
}

/// Mechanical anchors from a message range: paths written, identifiers, decisions, the last
/// request and running jobs. Pure.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Anchors {
    pub goal: String,
    pub written: Vec<String>,
    pub paths: Vec<String>,
    pub ids: Vec<String>,
    pub decisions: Vec<String>,
    pub last_request: String,
    pub recent: Vec<String>,
    pub jobs: Vec<String>,
    pub preferences: Vec<String>,
    pub security: Vec<String>,
}

fn id_re() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?:#\d{2,6}\b|\b[0-9a-f]{7,12}\b|\b(?:TASK|M|G|E)-\d{1,4}\b|https?://[^\s)\]>'`]+)",
        )
        .expect("id pattern")
    })
}

fn decision_re() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)\b(decided|decision|we will|i will use|chose|chosen|instead of|rather than|went with|going with|settled on)\b")
            .expect("decision pattern")
    })
}

fn sentences(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\n', '.', '!', '?'])
        .map(str::trim)
        .filter(|s| s.len() > 12)
}

fn written_path(name: &str, args: &str) -> Option<String> {
    let n = name.to_lowercase();
    if !(n.contains("write") || n.contains("patch") || n.contains("edit") || n == "notebookedit") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    ["path", "file_path", "filePath", "notebook_path"]
        .iter()
        .find_map(|k| v.get(k).and_then(|x| x.as_str()))
        .map(String::from)
}

pub fn anchors(msgs: &[Msg]) -> Anchors {
    let mut a = Anchors::default();
    let mut ids = BTreeMap::<String, usize>::new();
    let mut paths = BTreeSet::new();
    let mut written = BTreeSet::new();
    let mut decisions = Vec::new();
    for m in msgs {
        if m.role == "user" && a.goal.is_empty() && !m.content.trim().is_empty() {
            a.goal = m.content.trim().chars().take(400).collect();
        }
        if m.role == "user" && !m.content.trim().is_empty() {
            a.last_request = m.content.trim().chars().take(400).collect();
            for s in sentences(&m.content) {
                let l = s.to_lowercase();
                if (l.contains("prefer")
                    || l.starts_with("never")
                    || l.starts_with("always")
                    || l.contains("don't")
                    || l.contains("do not"))
                    && a.preferences.len() < 12
                {
                    a.preferences.push(s.chars().take(200).collect());
                }
            }
        }
        for (name, args) in &m.tool_calls {
            if let Some(p) = written_path(name, args) {
                written.insert(p);
            }
            if name.to_lowercase().contains("terminal") || name == "Bash" {
                let cmd: String = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(String::from))
                    .unwrap_or_default();
                if args.contains("\"background\": true")
                    || args.contains("\"background\":true")
                    || args.contains("run_in_background")
                {
                    a.jobs.push(cmd.chars().take(160).collect());
                }
            }
        }
        if m.role == "tool" {
            let tool = m.tool_name.clone().unwrap_or_else(|| "tool".into());
            a.recent.push(mask_tool_output(&tool, &m.content));
        }
        let text = &m.content;
        for x in id_re().find_iter(text) {
            *ids.entry(x.as_str().trim_end_matches(['.', ',']).to_string())
                .or_default() += 1;
        }
        for p in path_re().find_iter(text) {
            paths.insert(p.as_str().to_string());
        }
        if m.role == "assistant" {
            for s in sentences(text) {
                if decision_re().is_match(s) {
                    decisions.push(s.chars().take(240).collect::<String>());
                }
            }
        }
        for s in sentences(text) {
            let l = s.to_lowercase();
            if (l.contains("secret")
                || l.contains("token")
                || l.contains("credential")
                || l.contains("password"))
                && (l.contains("never") || l.contains("do not") || l.contains("don't"))
                && a.security.len() < 8
                && !a.security.iter().any(|x: &String| x == s)
            {
                a.security.push(s.chars().take(200).collect());
            }
        }
    }
    a.written = written.into_iter().collect();
    a.paths = paths.into_iter().take(60).collect();
    let mut ids: Vec<(String, usize)> = ids.into_iter().collect();
    ids.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
    a.ids = ids.into_iter().take(60).map(|(k, _)| k).collect();
    decisions.dedup();
    let n = decisions.len();
    a.decisions = decisions.into_iter().skip(n.saturating_sub(20)).collect();
    let r = a.recent.len();
    a.recent = a.recent.split_off(r.saturating_sub(10));
    a
}

/// The fixed-schema body: the agent's own summary mapped by heading first, then the mechanical
/// anchors under each section. `focus` pins sentences that mention any of its words. Pure.
pub fn render_summary(agent_summary: &str, a: &Anchors, focus: &str) -> String {
    let mapped = map_summary(agent_summary);
    let mut s = String::new();
    let focus_words: Vec<String> = focus
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .map(str::to_lowercase)
        .collect();
    if !focus_words.is_empty() {
        s.push_str("## Pinned for this focus\n\n");
        let mut n = 0;
        for line in agent_summary
            .lines()
            .chain(a.decisions.iter().map(String::as_str))
        {
            let l = line.to_lowercase();
            if focus_words.iter().any(|w| l.contains(w)) && n < 20 {
                s.push_str(&format!("- {}\n", line.trim().trim_start_matches("- ")));
                n += 1;
            }
        }
        if n == 0 {
            s.push_str("- nothing in this range mentions the focus\n");
        }
        s.push('\n');
    }
    let list = |v: &[String]| -> String { v.iter().map(|x| format!("- {x}\n")).collect() };
    for sec in SECTIONS {
        s.push_str(&format!("## {sec}\n\n"));
        let mut body = mapped.get(sec).cloned().unwrap_or_default();
        let extra = match sec {
            "Goal" if body.trim().is_empty() => format!("{}\n", a.goal),
            "Artifact trail" if !a.written.is_empty() => {
                format!("Files written in this range:\n{}", list(&a.written))
            }
            "File state" if !a.paths.is_empty() => format!("Paths mentioned:\n{}", list(&a.paths)),
            "Key knowledge" if !a.ids.is_empty() => {
                format!("Identifiers (most mentioned first): {}\n", a.ids.join(", "))
            }
            "Recent actions" if !a.recent.is_empty() => list(&a.recent),
            "Next step" if body.trim().is_empty() && !a.last_request.is_empty() => {
                format!("The last request in this range: {}\n", a.last_request)
            }
            "Decisions" if !a.decisions.is_empty() => {
                format!("Decision sentences from the agent:\n{}", list(&a.decisions))
            }
            "Running jobs" if !a.jobs.is_empty() => list(&a.jobs),
            "User preferences" if !a.preferences.is_empty() => list(&a.preferences),
            "Security constraints" if !a.security.is_empty() => list(&a.security),
            _ => String::new(),
        };
        if !extra.is_empty() {
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str(&extra);
        }
        if body.trim().is_empty() {
            body = "Not recorded in this range.\n".into();
        }
        s.push_str(body.trim_end());
        s.push_str("\n\n");
    }
    s
}

// ---------------------------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct ProbeScore {
    pub facts: f64,
    pub artifacts: f64,
    pub continuation: f64,
    pub decisions: f64,
}

impl ProbeScore {
    pub fn passes(&self, bar: f64) -> bool {
        self.facts >= bar
            && self.artifacts >= bar
            && self.continuation >= bar
            && self.decisions >= bar
    }
}

fn recall(gold: &[String], text: &str) -> f64 {
    if gold.is_empty() {
        return 1.0;
    }
    let t = text.to_lowercase();
    gold.iter()
        .filter(|g| t.contains(&g.to_lowercase()))
        .count() as f64
        / gold.len() as f64
}

fn key_words(s: &str) -> Vec<String> {
    const STOP: [&str; 24] = [
        "the", "and", "for", "with", "that", "this", "from", "into", "will", "have", "then",
        "than", "instead", "rather", "decided", "decision", "chose", "chosen", "going", "went",
        "settled", "because", "should", "would",
    ];
    let mut w: Vec<String> = s
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.')
        .map(|x| x.trim_matches('.').to_lowercase())
        .filter(|x| x.len() > 3 && !STOP.contains(&x.as_str()))
        .collect();
    w.dedup();
    w.truncate(4);
    w
}

/// The four probes over a dropped range, scored against the text that survives it (a summary
/// plus whatever the agent kept). Lexical: each probe is the share of its gold anchors found.
/// - facts: identifiers mentioned at least twice in the range;
/// - artifacts: files written in the range;
/// - continuation: the content words of the last request;
/// - decisions: for each decision sentence, its two leading content words.
///
/// Pure.
pub fn probe(dropped: &[Msg], surviving: &str) -> ProbeScore {
    let a = anchors(dropped);
    let mut counts = BTreeMap::<String, usize>::new();
    for m in dropped {
        for x in id_re().find_iter(&m.content) {
            *counts
                .entry(x.as_str().trim_end_matches(['.', ',']).to_string())
                .or_default() += 1;
        }
    }
    let facts: Vec<String> = counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|(k, _)| k)
        .take(30)
        .collect();
    let cont = key_words(&a.last_request);
    let mut dec_score = 1.0;
    if !a.decisions.is_empty() {
        let hits = a
            .decisions
            .iter()
            .filter(|d| {
                let w = key_words(d);
                let w: Vec<String> = w.into_iter().take(2).collect();
                !w.is_empty() && recall(&w, surviving) >= 1.0
            })
            .count();
        dec_score = hits as f64 / a.decisions.len() as f64;
    }
    ProbeScore {
        facts: recall(&facts, surviving),
        artifacts: recall(&a.written, surviving),
        continuation: recall(&cont, surviving),
        decisions: dec_score,
    }
}

// ---------------------------------------------------------------------------------------------
// Events, the table, and the sources
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Event {
    pub id: i64,
    pub at: i64,
    /// `hermes` or `claude-code`.
    pub agent: String,
    pub session_id: String,
    /// The transcript the range belongs to (Hermes state.db or a .jsonl file).
    pub source: String,
    pub first_msg: i64,
    pub last_msg: i64,
    pub dropped_msgs: i64,
    pub tokens_before: i64,
    pub tokens_after: i64,
    pub model: String,
    pub prompt_version: String,
    pub commit: String,
    pub note: String,
    pub hash: String,
}

/// Append an event once; the same compaction is never recorded twice. Returns the row id and
/// whether it was new.
pub fn record(c: &Connection, e: &Event) -> Result<(i64, bool)> {
    let existing: Option<i64> = c
        .query_row(
            "SELECT id FROM context_events WHERE agent = ?1 AND session_id = ?2 AND first_msg = ?3 AND last_msg = ?4",
            params![e.agent, e.session_id, e.first_msg, e.last_msg],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok((id, false));
    }
    c.execute(
        "INSERT INTO context_events (at, agent, session_id, source, first_msg, last_msg, dropped_msgs,
           tokens_before, tokens_after, model, prompt_version, commit_sha, note, hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            e.at, e.agent, e.session_id, e.source, e.first_msg, e.last_msg, e.dropped_msgs,
            e.tokens_before, e.tokens_after, e.model, e.prompt_version, e.commit, e.note, e.hash
        ],
    )?;
    Ok((c.last_insert_rowid(), true))
}

pub fn list(c: &Connection, limit: usize) -> Result<Vec<Event>> {
    let mut st = c.prepare(
        "SELECT id, at, agent, session_id, source, first_msg, last_msg, dropped_msgs, tokens_before,
                tokens_after, model, prompt_version, commit_sha, note, hash
         FROM context_events ORDER BY at DESC, id DESC LIMIT ?1",
    )?;
    let rows = st
        .query_map([limit as i64], |r| {
            Ok(Event {
                id: r.get(0)?,
                at: r.get(1)?,
                agent: r.get(2)?,
                session_id: r.get(3)?,
                source: r.get(4)?,
                first_msg: r.get(5)?,
                last_msg: r.get(6)?,
                dropped_msgs: r.get(7)?,
                tokens_before: r.get(8)?,
                tokens_after: r.get(9)?,
                model: r.get(10)?,
                prompt_version: r.get(11)?,
                commit: r.get(12)?,
                note: r.get(13)?,
                hash: r.get(14)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// A compaction found in a source, before it is recorded.
#[derive(Clone, Debug, Default)]
pub struct Found {
    pub agent: String,
    pub session_id: String,
    pub source: String,
    pub dropped: Vec<Msg>,
    pub summary: String,
    pub kept_tokens: i64,
    pub at: i64,
    pub model: String,
    pub prompt_version: String,
    pub repo: Option<PathBuf>,
    pub tokens_before: Option<i64>,
    pub tokens_after: Option<i64>,
}

pub fn parse_calls(raw: Option<String>) -> Vec<(String, String)> {
    let Some(raw) = raw.filter(|s| !s.trim().is_empty()) else {
        return vec![];
    };
    let Ok(serde_json::Value::Array(v)) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return vec![];
    };
    v.iter()
        .filter_map(|c| {
            let f = c.get("function").unwrap_or(c);
            let name = f.get("name")?.as_str()?.to_string();
            let args = match f.get("arguments") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            Some((name, args))
        })
        .collect()
}

/// Every compaction Hermes recorded: each summary message and the compacted messages before it
/// (after the previous summary). Read-only.
pub fn hermes_compactions(state_db: &Path) -> Result<Vec<Found>> {
    let uri = format!(
        "file:{}?mode=ro",
        state_db.display().to_string().replace('\\', "/")
    );
    let c = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open {} read-only", state_db.display()))?;
    let mut st = c.prepare(
        "SELECT m.session_id, m.id, m.content, COALESCE(m.timestamp, 0), COALESCE(s.model, ''),
                COALESCE(s.system_prompt_hash, ''), s.git_repo_root
         FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE m._compressed_summary = 1 ORDER BY m.session_id, m.id",
    )?;
    type SummaryRow = (String, i64, String, f64, String, String, Option<String>);
    let summaries: Vec<SummaryRow> = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    let mut prev: BTreeMap<String, i64> = BTreeMap::new();
    for (sid, sum_id, content, ts, model, prompt, repo) in summaries {
        let from = prev.get(&sid).copied().unwrap_or(0);
        prev.insert(sid.clone(), sum_id);
        let mut q = c.prepare(
            "SELECT id, role, COALESCE(content, ''), tool_name, tool_calls, COALESCE(timestamp, 0)
             FROM messages WHERE session_id = ?1 AND id > ?2 AND id < ?3 AND compacted = 1 ORDER BY id",
        )?;
        let dropped: Vec<Msg> = q
            .query_map(params![sid, from, sum_id], |r| {
                Ok(Msg {
                    id: r.get(0)?,
                    role: r.get(1)?,
                    content: r.get(2)?,
                    tool_name: r.get(3)?,
                    tool_calls: parse_calls(r.get(4)?),
                    at: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if dropped.is_empty() {
            continue;
        }
        let kept: i64 = c.query_row(
            "SELECT COALESCE(SUM(LENGTH(COALESCE(content, ''))), 0) FROM messages WHERE session_id = ?1 AND id > ?2 AND compacted = 0 AND id <= ?3",
            params![sid, from, sum_id],
            |r| r.get(0),
        )?;
        out.push(Found {
            agent: "hermes".into(),
            session_id: sid,
            source: state_db.display().to_string(),
            summary: content,
            kept_tokens: kept / 4,
            at: ts as i64,
            model,
            prompt_version: prompt,
            repo: repo.map(PathBuf::from),
            dropped,
            tokens_before: None,
            tokens_after: None,
        });
    }
    Ok(out)
}

fn claude_text(v: &serde_json::Value) -> String {
    match v.get("message").and_then(|m| m.get("content")) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                    return Some(t.to_string());
                }
                match p.get("content") {
                    Some(serde_json::Value::String(t)) => Some(t.clone()),
                    // tool_result blocks usually carry an array of text blocks.
                    Some(serde_json::Value::Array(inner)) => {
                        let t = inner
                            .iter()
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n");
                        (!t.is_empty()).then_some(t)
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn claude_calls(v: &serde_json::Value) -> Vec<(String, String)> {
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                .filter_map(|p| {
                    Some((
                        p.get("name")?.as_str()?.to_string(),
                        p.get("input").map(|i| i.to_string()).unwrap_or_default(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Compactions in one Claude Code transcript: every `compact_boundary` record, the messages
/// before it (after the previous boundary) and the summary that follows. Read-only. Pure over the
/// file.
pub fn claude_compactions(jsonl: &Path) -> Result<Vec<Found>> {
    let text = fs::read_to_string(jsonl)?;
    let mut out = Vec::new();
    let mut buf: Vec<Msg> = Vec::new();
    let mut pending: Option<Found> = None;
    let session = jsonl
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    for (n, line) in text.lines().enumerate() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if v.get("subtype").and_then(|s| s.as_str()) == Some("compact_boundary") {
            let meta = v.get("compactMetadata").cloned().unwrap_or_default();
            let at = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.timestamp())
                .unwrap_or(0);
            pending = Some(Found {
                agent: "claude-code".into(),
                session_id: session.clone(),
                source: jsonl.display().to_string(),
                dropped: std::mem::take(&mut buf),
                at,
                tokens_before: meta.get("preTokens").and_then(|x| x.as_i64()),
                tokens_after: meta.get("postTokens").and_then(|x| x.as_i64()),
                repo: v.get("cwd").and_then(|c| c.as_str()).map(PathBuf::from),
                ..Default::default()
            });
            continue;
        }
        if kind != "user" && kind != "assistant" {
            continue;
        }
        if v.get("isCompactSummary").and_then(|b| b.as_bool()) == Some(true) {
            if let Some(mut f) = pending.take() {
                f.summary = claude_text(&v);
                if !f.dropped.is_empty() {
                    out.push(f);
                }
            }
            continue;
        }
        let content = claude_text(&v);
        let calls = claude_calls(&v);
        let is_result = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .is_some_and(|p| {
                p.iter()
                    .any(|x| x.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            });
        buf.push(Msg {
            id: n as i64 + 1,
            role: if is_result {
                "tool".into()
            } else {
                kind.into()
            },
            content,
            tool_name: None,
            tool_calls: calls,
            at: 0.0,
        });
    }
    if let Some(f) = pending.filter(|f| !f.dropped.is_empty()) {
        out.push(f);
    }
    Ok(out)
}

/// The commit a repository was at when the compaction happened: the last commit before then.
fn commit_at(repo: Option<&Path>, at: i64) -> String {
    let Some(r) = repo.filter(|r| r.join(".git").exists()) else {
        return String::new();
    };
    let Some(git) = crate::procs::find("git") else {
        return String::new();
    };
    crate::procs::command(&git)
        .arg("-C")
        .arg(r)
        .args(["log", "-1", "--format=%H", &format!("--before=@{at}")])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// A focus hint safe to put in YAML frontmatter: one line, no control characters, no secrets.
/// Pure.
pub fn clean_focus(focus: &str) -> String {
    let one: String = focus
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let one = one.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::constitution_gate::redact(&one)
        .chars()
        .take(200)
        .collect()
}

/// The note for one compaction: frontmatter plus the fixed schema. Pure apart from `commit`.
pub fn note_text(f: &Found, commit: &str, focus: &str) -> (String, Event) {
    let a = anchors(&f.dropped);
    let focus = clean_focus(focus);
    // Every line that reaches the vault goes through the gate's credential rules first.
    let body = crate::constitution_gate::redact(&render_summary(&f.summary, &a, &focus));
    let first = f.dropped.first().map(|m| m.id).unwrap_or(0);
    let last = f.dropped.last().map(|m| m.id).unwrap_or(0);
    let before = f.tokens_before.unwrap_or_else(|| {
        f.dropped
            .iter()
            .map(|m| estimate_tokens(&m.content))
            .sum::<i64>()
            + f.kept_tokens
    });
    let after = f
        .tokens_after
        .unwrap_or_else(|| estimate_tokens(&f.summary) + f.kept_tokens);
    let mut h = Sha256::new();
    h.update(body.as_bytes());
    let hash: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let short: String = f
        .session_id
        .chars()
        .rev()
        .take(10)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let note = format!(
        "03_Memory/context/{}-{}-{}-{}.md",
        f.agent, short, first, last
    );
    let fm = format!(
        "---\ntitle: Context summary {agent} {short} {first}-{last}\ntype: context-summary\nagent: {agent}\nsession: {sid}\nsource_transcript: \"{src}\"\nmessage_range: [{first}, {last}]\ndropped_messages: {n}\ntokens_before: {before}\ntokens_after: {after}\nmodel: \"{model}\"\nprompt_version: \"{pv}\"\ncommit: \"{commit}\"\nhash: {hash}\nfocus: \"{focus}\"\nauthor: AgentOS Studio (K-ONE IT SOLUTIONS, Kone Tshivhinda)\n---\n\n# Context summary\n\nWritten by AgentOS Studio from a {agent} compaction. The transcript is unchanged; this note says what the dropped range held. Messages {first} to {last} of `{sid}` in `{src}`.\n\n",
        agent = f.agent,
        sid = f.session_id,
        src = f.source.replace('\\', "/").replace('"', "'"),
        n = f.dropped.len(),
        model = f.model.replace('"', "'"),
        pv = f.prompt_version,
        focus = focus.replace('"', "'"),
    );
    let ev = Event {
        id: 0,
        at: f.at,
        agent: f.agent.clone(),
        session_id: f.session_id.clone(),
        source: f.source.clone(),
        first_msg: first,
        last_msg: last,
        dropped_msgs: f.dropped.len() as i64,
        tokens_before: before,
        tokens_after: after,
        model: f.model.clone(),
        prompt_version: f.prompt_version.clone(),
        commit: commit.to_string(),
        note: note.clone(),
        hash,
    };
    (format!("{fm}{body}"), ev)
}

/// Where Claude Code keeps its transcripts.
pub fn claude_transcripts() -> Vec<PathBuf> {
    let root = crate::settings::home().join(".claude").join("projects");
    walkdir::WalkDir::new(root)
        .max_depth(2)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl"))
        .map(|e| e.into_path())
        .collect()
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct ScanReport {
    pub found: usize,
    pub recorded: usize,
    pub notes_written: usize,
    pub errors: Vec<String>,
}

/// Scan Hermes and Claude Code for compactions, record new ones and write their notes. Notes are
/// immutable: an existing note is never rewritten by a scan.
pub fn scan(vault_root: &Path, c: &Connection) -> ScanReport {
    let mut r = ScanReport::default();
    let mut found = Vec::new();
    match hermes_compactions(&crate::procs::hermes_home().join("state.db")) {
        Ok(f) => found.extend(f),
        Err(e) => r.errors.push(format!("hermes: {e:#}")),
    }
    for t in claude_transcripts() {
        let has = fs::read_to_string(&t)
            .map(|s| s.contains("compact_boundary"))
            .unwrap_or(false);
        if !has {
            continue;
        }
        match claude_compactions(&t) {
            Ok(f) => found.extend(f),
            Err(e) => r.errors.push(format!("{}: {e:#}", t.display())),
        }
    }
    r.found = found.len();
    for f in &found {
        let commit = commit_at(f.repo.as_deref(), f.at);
        let (text, ev) = note_text(f, &commit, "");
        let path = vault_root.join(&ev.note);
        if !path.exists() {
            if let Some(p) = path.parent() {
                let _ = fs::create_dir_all(p);
            }
            match fs::write(&path, &text) {
                Ok(()) => r.notes_written += 1,
                Err(e) => r.errors.push(format!("{}: {e}", path.display())),
            }
        }
        match record(c, &ev) {
            Ok((_, true)) => r.recorded += 1,
            Ok(_) => {}
            Err(e) => r.errors.push(format!("record: {e:#}")),
        }
    }
    r
}

/// Write a fresh summary for one recorded event with a focus hint, as a new note beside the
/// original; the original note and the transcript stay as they are.
pub fn rerun(vault_root: &Path, c: &Connection, id: i64, focus: &str) -> Result<String> {
    let e = list(c, 10_000)?
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| anyhow::anyhow!("no context event {id}"))?;
    let all: Vec<Found> = if e.agent == "hermes" {
        hermes_compactions(Path::new(&e.source))?
    } else {
        claude_compactions(Path::new(&e.source))?
    };
    let f = all
        .into_iter()
        .find(|f| {
            f.session_id == e.session_id && f.dropped.first().map(|m| m.id) == Some(e.first_msg)
        })
        .ok_or_else(|| anyhow::anyhow!("the transcript no longer has that range"))?;
    let (text, _) = note_text(&f, &e.commit, focus);
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let rel = e.note.replace(".md", &format!("-focus-{stamp}.md"));
    let path = vault_root.join(&rel);
    fs::write(&path, text)?;
    Ok(rel)
}

#[derive(Serialize, Clone, Debug)]
pub struct Hit {
    pub kind: String,
    pub reference: String,
    pub text: String,
}

/// Retrieval over transcripts (Hermes' own FTS5 index, read-only) and notes (the vault's FTS5
/// index, which includes the context summaries).
pub fn search(v: &crate::vault::Vault, q: &str, k: usize) -> Result<Vec<Hit>> {
    let mut out = Vec::new();
    if let Ok(chunks) = crate::memory_index::recall(v, q, None, k, 4000) {
        for ch in chunks {
            out.push(Hit {
                kind: if ch.path.contains("03_Memory/context/") {
                    "summary".into()
                } else {
                    "note".into()
                },
                reference: format!("{}:{}", ch.path, ch.line),
                text: ch.text.chars().take(300).collect(),
            });
        }
    }
    let db = crate::procs::hermes_home().join("state.db");
    let uri = format!(
        "file:{}?mode=ro",
        db.display().to_string().replace('\\', "/")
    );
    if let Ok(c) = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) {
        let phrase = format!("\"{}\"", q.replace('"', " "));
        if let Ok(mut st) = c.prepare(
            "SELECT m.session_id, m.id, snippet(messages_fts, 0, '[', ']', ' ... ', 16)
             FROM messages_fts JOIN messages m ON m.id = messages_fts.rowid
             WHERE messages_fts MATCH ?1 LIMIT ?2",
        ) {
            if let Ok(rows) = st.query_map(params![phrase, k as i64], |r| {
                Ok(Hit {
                    kind: "transcript".into(),
                    reference: format!(
                        "hermes {} #{}",
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?
                    ),
                    text: r.get(2)?,
                })
            }) {
                out.extend(rows.flatten());
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Evaluation over real sessions
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct EvalRow {
    pub agent: String,
    pub session: String,
    /// `compaction` (a real one, baseline is the agent's own summary) or `replay` (a long session
    /// cut at 60 percent, baseline is the first and last turns only).
    pub kind: String,
    pub dropped: usize,
    pub baseline: ProbeScore,
    pub studio: ProbeScore,
    pub tokens: i64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct EvalReport {
    pub rows: Vec<EvalRow>,
    pub sessions: usize,
    pub baseline_mean: ProbeScore,
    pub studio_mean: ProbeScore,
    pub pass_bar: f64,
    pub studio_passing: usize,
    pub tokens_per_session: i64,
}

fn mean(xs: &[&ProbeScore]) -> ProbeScore {
    let n = xs.len().max(1) as f64;
    ProbeScore {
        facts: xs.iter().map(|x| x.facts).sum::<f64>() / n,
        artifacts: xs.iter().map(|x| x.artifacts).sum::<f64>() / n,
        continuation: xs.iter().map(|x| x.continuation).sum::<f64>() / n,
        decisions: xs.iter().map(|x| x.decisions).sum::<f64>() / n,
    }
}

fn hermes_long_sessions(state_db: &Path, min: usize) -> Result<Vec<(String, Vec<Msg>)>> {
    let uri = format!(
        "file:{}?mode=ro",
        state_db.display().to_string().replace('\\', "/")
    );
    let c = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let ids: Vec<String> = c
        .prepare("SELECT session_id FROM messages GROUP BY session_id HAVING COUNT(*) >= ?1")?
        .query_map([min as i64], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for sid in ids {
        let msgs: Vec<Msg> = c
            .prepare("SELECT id, role, COALESCE(content, ''), tool_name, tool_calls, COALESCE(timestamp, 0) FROM messages WHERE session_id = ?1 AND COALESCE(_compressed_summary, 0) = 0 ORDER BY id")?
            .query_map([&sid], |r| {
                Ok(Msg {
                    id: r.get(0)?,
                    role: r.get(1)?,
                    content: r.get(2)?,
                    tool_name: r.get(3)?,
                    tool_calls: parse_calls(r.get(4)?),
                    at: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        out.push((sid, msgs));
    }
    Ok(out)
}

fn claude_sessions(min: usize) -> Vec<(String, Vec<Msg>)> {
    let mut out = Vec::new();
    for t in claude_transcripts() {
        let Ok(text) = fs::read_to_string(&t) else {
            continue;
        };
        let mut msgs = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if (kind != "user" && kind != "assistant") || v.get("isCompactSummary").is_some() {
                continue;
            }
            msgs.push(Msg {
                id: n as i64 + 1,
                role: kind.into(),
                content: claude_text(&v),
                tool_name: None,
                tool_calls: claude_calls(&v),
                at: 0.0,
            });
        }
        if msgs.len() >= min {
            out.push((
                t.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
                msgs,
            ));
        }
    }
    out
}

/// Score the four probes over every real compaction and over long sessions replayed with a cut
/// at 60 percent. Read-only over the transcripts; nothing is written.
pub fn evaluate(min_messages: usize, bar: f64) -> Result<EvalReport> {
    let mut rows = Vec::new();
    let state = crate::procs::hermes_home().join("state.db");
    let mut compactions = hermes_compactions(&state).unwrap_or_default();
    for t in claude_transcripts() {
        if fs::read_to_string(&t)
            .map(|s| s.contains("compact_boundary"))
            .unwrap_or(false)
        {
            compactions.extend(claude_compactions(&t).unwrap_or_default());
        }
    }
    for f in &compactions {
        let (studio, _) = note_text(f, "", "");
        rows.push(EvalRow {
            agent: f.agent.clone(),
            session: f.session_id.clone(),
            kind: "compaction".into(),
            dropped: f.dropped.len(),
            baseline: probe(&f.dropped, &f.summary),
            studio: probe(&f.dropped, &studio),
            tokens: f.dropped.iter().map(|m| estimate_tokens(&m.content)).sum(),
        });
    }
    let mut sessions: Vec<(String, String, Vec<Msg>)> = hermes_long_sessions(&state, min_messages)
        .unwrap_or_default()
        .into_iter()
        .map(|(s, m)| ("hermes".to_string(), s, m))
        .collect();
    sessions.extend(
        claude_sessions(min_messages)
            .into_iter()
            .map(|(s, m)| ("claude-code".to_string(), s, m)),
    );
    for (agent, sid, msgs) in sessions {
        let cut = msgs.len() * 6 / 10;
        if cut < 4 {
            continue;
        }
        let dropped = &msgs[1..cut];
        let head = msgs[0].content.clone();
        let tail: String = msgs[cut..]
            .iter()
            .take(4)
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let baseline_text = format!("{head}\n{tail}");
        let f = Found {
            agent: agent.clone(),
            session_id: sid.clone(),
            dropped: dropped.to_vec(),
            ..Default::default()
        };
        let (studio, _) = note_text(&f, "", "");
        rows.push(EvalRow {
            agent,
            session: sid,
            kind: "replay".into(),
            dropped: dropped.len(),
            baseline: probe(dropped, &baseline_text),
            studio: probe(dropped, &format!("{studio}\n{tail}")),
            tokens: msgs.iter().map(|m| estimate_tokens(&m.content)).sum(),
        });
    }
    let sessions: BTreeSet<&str> = rows.iter().map(|r| r.session.as_str()).collect();
    let base: Vec<&ProbeScore> = rows.iter().map(|r| &r.baseline).collect();
    let stu: Vec<&ProbeScore> = rows.iter().map(|r| &r.studio).collect();
    Ok(EvalReport {
        sessions: sessions.len(),
        baseline_mean: mean(&base),
        studio_mean: mean(&stu),
        pass_bar: bar,
        studio_passing: rows.iter().filter(|r| r.studio.passes(bar)).count(),
        tokens_per_session: rows.iter().map(|r| r.tokens).sum::<i64>() / rows.len().max(1) as i64,
        rows,
    })
}

// ---------------------------------------------------------------------------------------------
// The worktree contract
// ---------------------------------------------------------------------------------------------

pub fn context_rel(mission: &str) -> String {
    format!(".agentos/context/{mission}.md")
}

/// The pinned context file a worker reads first. Pure.
pub fn worktree_context(m: &crate::missions::Mission, has_agents_md: bool) -> String {
    let mut s = format!(
        "# Context for {id}\n\nWritten by AgentOS Studio (K-ONE IT SOLUTIONS, Kone Tshivhinda) when the mission started. Read this before you work, and before you finish update Decisions, Next step and Open questions so a later attempt or another agent can continue. It is local to this worktree and is not committed.\n\n## Goal\n\n{goal}\n\n## Acceptance criteria\n\n{acc}\n\n## Constraints\n\n- Work only in this worktree, on its own branch. Open a pull request; never merge.\n- Follow the repository's rules: {rules}.\n- No em dashes or en dashes. Never commit secrets.\n",
        id = m.id,
        goal = m.goal.trim(),
        acc = m.acceptance.trim(),
        rules = if has_agents_md { "AGENTS.md (and CLAUDE.md or GEMINI.md for your agent); this file does not replace them" } else { "the rule files at the repository root" },
    );
    if let Some(note) = m.review_note.as_deref().filter(|n| !n.trim().is_empty()) {
        if m.review_verdict.as_deref() == Some("reject") {
            s.push_str(&format!(
                "\n## The last review rejected the previous attempt\n\n{}\n",
                note.trim()
            ));
        }
    }
    s.push_str("\n## Decisions\n\n- None yet.\n\n## Next step\n\nRead the acceptance criteria and the code they touch, then plan the change.\n\n## Open questions\n\n- None yet.\n");
    s
}

/// Write the context file into a worktree (once; a worker's own updates are never overwritten)
/// and keep it out of commits through `<git-common-dir>/info/exclude` (git reads exclude rules
/// only from the common dir, so the pattern covers the primary checkout and every linked
/// worktree). Fails when `git check-ignore` does not confirm the file is ignored.
pub fn write_worktree_context(worktree: &Path, m: &crate::missions::Mission) -> Result<PathBuf> {
    let path = worktree.join(context_rel(&m.id));
    if !path.exists() {
        fs::create_dir_all(path.parent().expect("context dir"))?;
        fs::write(
            &path,
            worktree_context(m, worktree.join("AGENTS.md").is_file()),
        )?;
    }
    if let Some(git) = crate::procs::find("git") {
        let common = crate::procs::command(&git)
            .arg("-C")
            .arg(worktree)
            .args(["rev-parse", "--git-common-dir"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        if let Some(c) = common {
            let dir = if Path::new(&c).is_absolute() {
                PathBuf::from(c)
            } else {
                worktree.join(c)
            };
            let f = dir.join("info").join("exclude");
            let text = fs::read_to_string(&f).unwrap_or_default();
            if !text.lines().any(|l| l.trim() == "/.agentos/context/") {
                let _ = fs::create_dir_all(dir.join("info"));
                let mut out = text;
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str("/.agentos/context/\n");
                fs::write(&f, out)?;
            }
        }
        // The contract is "never committed": prove git ignores the file, or report failure.
        let ignored = crate::procs::command(&git)
            .arg("-C")
            .arg(worktree)
            .args(["check-ignore", "-q", &context_rel(&m.id)])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ignored {
            anyhow::bail!(
                "{} is not ignored by git in {}; a worker could commit it",
                context_rel(&m.id),
                worktree.display()
            );
        }
        return Ok(path);
    }
    anyhow::bail!(
        "git not found, so {} cannot be excluded from commits",
        context_rel(&m.id)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: i64, role: &str, content: &str) -> Msg {
        Msg {
            id,
            role: role.into(),
            content: content.into(),
            ..Default::default()
        }
    }

    fn call(id: i64, name: &str, args: &str) -> Msg {
        Msg {
            id,
            role: "assistant".into(),
            tool_calls: vec![(name.into(), args.into())],
            ..Default::default()
        }
    }

    #[test]
    fn masking_keeps_exit_codes() {
        let out = "Compiling agentos-studio v0.15.0\n   Finished in 5m\nerror[E0425]: cannot find value `x` in src/budget.rs\n{\"output\": \"...\", \"exit_code\": 101}\nwrote C:/Users/me/agentos-studio/src-tauri/target/debug/app.exe\n".repeat(30);
        let masked = mask_tool_output("terminal", &out);
        assert!(masked.len() < out.len() / 5, "{}", masked.len());
        assert!(masked.contains("exit 101"), "{masked}");
        assert!(masked.contains("src/budget.rs"), "{masked}");
        assert!(masked.contains("C:/Users/me/agentos-studio/src-tauri/target/debug/app.exe"));
        assert!(masked.contains("error[E0425]"));
        assert!(masked.starts_with("[terminal output masked"));
        assert!(mask_tool_output("t", "done\nexit code: 0").contains("exit 0"));
    }

    #[test]
    fn notes_never_carry_credentials() {
        // Built at run time so no scanner sees a token-shaped literal in the source.
        let notion = format!("ntn_{}", "a1B2".repeat(11));
        let openai = format!("sk-{}", "Zz9y".repeat(10));
        let dropped = vec![
            m(
                1,
                "user",
                &format!("Use this token {notion} for the publish step please"),
            ),
            call(2, "read_file", r#"{"path": ".env"}"#),
            m(
                3,
                "tool",
                &format!("OPENROUTER_API_KEY={openai}\nerror: Authorization: Bearer {openai}"),
            ),
            m(
                4,
                "assistant",
                "I decided to keep the key out of the repository entirely.",
            ),
        ];
        let (note, _) = note_text(
            &Found {
                agent: "claude-code".into(),
                session_id: "s-9".into(),
                source: "t.jsonl".into(),
                dropped,
                summary: format!("## Goal\nPublish with {notion}"),
                ..Default::default()
            },
            "",
            &format!("publish\n{openai}"),
        );
        assert!(!note.contains(&notion), "{note}");
        assert!(!note.contains(&openai), "{note}");
        assert!(note.contains("[redacted:"), "{note}");
        let fm_end = note[4..].find("\n---\n").unwrap();
        assert!(
            !note[..fm_end].contains("\npublish\n"),
            "focus stays one line"
        );
        assert!(note.contains("focus: \"publish [redacted"), "{note}");
    }

    #[test]
    fn claude_tool_result_arrays_are_read() {
        let v = serde_json::json!({"message": {"content": [
            {"type": "tool_result", "content": [{"type": "text", "text": "exit code: 2 in src/a.rs"}]}
        ]}});
        assert_eq!(claude_text(&v), "exit code: 2 in src/a.rs");
    }

    #[test]
    fn worktree_context_is_ignored_by_git() {
        let Some(git) = crate::procs::find("git") else {
            return;
        };
        let t = tempfile::tempdir().unwrap();
        let ok = crate::procs::command(&git)
            .arg("-C")
            .arg(t.path())
            .args(["init", "-q"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok);
        let mission = crate::missions::Mission {
            id: "M-900".into(),
            goal: "g".into(),
            ..Default::default()
        };
        let p = write_worktree_context(t.path(), &mission).unwrap();
        assert!(p.is_file());
        // A second call keeps the worker's edits and still passes the ignore check.
        fs::write(&p, "worker edit").unwrap();
        write_worktree_context(t.path(), &mission).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "worker edit");
    }

    #[test]
    fn no_compaction_mid_tool_call() {
        let mut msgs = vec![m(1, "user", "go"), call(2, "terminal", "{}")];
        assert!(mid_tool_call(&msgs));
        assert_eq!(stage(80, 100, mid_tool_call(&msgs)), "defer");
        msgs.push(m(3, "tool", "ok"));
        assert!(!mid_tool_call(&msgs));
        assert_eq!(stage(80, 100, mid_tool_call(&msgs)), "compact");
        assert_eq!(stage(65, 100, false), "mask");
        assert_eq!(stage(10, 100, true), "ok");
        assert_eq!(
            target_tokens(1000),
            600,
            "75 percent less the 15 percent reserve"
        );
        let two = Msg {
            id: 4,
            role: "assistant".into(),
            tool_calls: vec![("a".into(), "{}".into()), ("b".into(), "{}".into())],
            ..Default::default()
        };
        assert!(mid_tool_call(&[
            two.clone(),
            m(5, "tool", "one result only")
        ]));
    }

    #[test]
    fn summary_probe_recovers_decisions() {
        let dropped = vec![
            m(1, "user", "Add a budget stop to the scheduler. Never commit secrets."),
            m(2, "assistant", "I decided to halt metered missions through kanban block instead of killing the worker process."),
            call(3, "write_file", r#"{"path": "src-tauri/src/budget.rs", "content": "x"}"#),
            m(4, "tool", "{\"exit_code\": 0}"),
            m(5, "assistant", "PR #131 is open; the squash will follow. We chose one escalation per period rather than one per mission."),
            m(6, "user", "Then merge #131 and start the merge policy item."),
            m(7, "assistant", "Merging #131 now."),
        ];
        // An agent summary that lost the decisions and the file.
        let thin = "## Goal\nBudget stop.\n";
        let base = probe(&dropped, thin);
        assert!(base.decisions < 1.0 && base.artifacts < 1.0);
        let (note, ev) = note_text(
            &Found {
                agent: "hermes".into(),
                session_id: "s-123".into(),
                source: "state.db".into(),
                dropped: dropped.clone(),
                summary: thin.into(),
                ..Default::default()
            },
            "abc1234",
            "",
        );
        let s = probe(&dropped, &note);
        assert_eq!(s.decisions, 1.0, "{note}");
        assert_eq!(s.artifacts, 1.0);
        assert_eq!(s.facts, 1.0);
        assert_eq!(s.continuation, 1.0);
        assert!(s.passes(0.99));
        for sec in SECTIONS {
            assert!(note.contains(&format!("## {sec}\n")), "missing {sec}");
        }
        assert!(note.contains("commit: \"abc1234\""));
        assert_eq!(ev.commit, "abc1234");
    }

    #[test]
    fn ledger_records_message_range() {
        let c = crate::store::test_conn();
        let f = Found {
            agent: "hermes".into(),
            session_id: "local-abc".into(),
            source: "C:/x/state.db".into(),
            dropped: vec![
                m(10, "user", "a"),
                m(11, "assistant", "b"),
                m(17, "user", "c"),
            ],
            summary: "## Goal\ng".into(),
            kept_tokens: 5,
            model: "claude-opus".into(),
            ..Default::default()
        };
        let (_, ev) = note_text(&f, "", "");
        assert_eq!((ev.first_msg, ev.last_msg, ev.dropped_msgs), (10, 17, 3));
        assert!(ev.tokens_before > ev.tokens_after || ev.tokens_before > 0);
        let (id, new) = record(&c, &ev).unwrap();
        assert!(new);
        let (id2, again) = record(&c, &ev).unwrap();
        assert!(!again, "the same compaction is recorded once");
        assert_eq!(id, id2);
        let rows = list(&c, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].first_msg, 10);
        assert_eq!(rows[0].last_msg, 17);
        assert_eq!(
            rows[0].note,
            "03_Memory/context/hermes-ocal-abc-10-17.md".replace("ocal-abc", "local-abc")
        );
    }

    #[test]
    fn claude_boundaries_are_parsed() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("sess.jsonl");
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"Fix the build in src/main.rs"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I chose to pin the toolchain instead of upgrading."},{"type":"tool_use","name":"Edit","input":{"file_path":"src/main.rs"}}]}}"#,
            r#"{"type":"system","subtype":"compact_boundary","timestamp":"2026-11-10T10:00:00Z","compactMetadata":{"preTokens":900,"postTokens":100}}"#,
            r#"{"type":"user","isCompactSummary":true,"message":{"role":"user","content":"Summary: build fixed."}}"#,
            r#"{"type":"user","message":{"role":"user","content":"next"}}"#,
        ];
        fs::write(&p, lines.join("\n")).unwrap();
        let f = claude_compactions(&p).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].dropped.len(), 2);
        assert_eq!(f[0].tokens_before, Some(900));
        assert_eq!(f[0].summary, "Summary: build fixed.");
        let a = anchors(&f[0].dropped);
        assert_eq!(a.written, vec!["src/main.rs".to_string()]);
        assert_eq!(a.decisions.len(), 1);
    }

    #[test]
    fn the_worktree_contract_points_at_agents_md() {
        let mi = crate::missions::Mission {
            id: "M-042".into(),
            goal: "Add a context ledger".into(),
            acceptance: "Four probes pass".into(),
            ..Default::default()
        };
        let s = worktree_context(&mi, true);
        assert!(s.contains("AGENTS.md"));
        assert!(s.contains("this file does not replace them"));
        for sec in [
            "## Goal",
            "## Acceptance criteria",
            "## Decisions",
            "## Next step",
        ] {
            assert!(s.contains(sec));
        }
        assert_eq!(context_rel("M-042"), ".agentos/context/M-042.md");
    }

    /// The eval over this machine's real sessions.
    /// Run: cargo test --lib context_ledger::tests::real_sessions -- --ignored --nocapture
    #[test]
    #[ignore = "reads the real Hermes and Claude Code transcripts"]
    fn real_sessions() {
        let r = evaluate(12, 0.5).unwrap();
        for row in &r.rows {
            println!(
                "{:<11} {:<10} {:<24} dropped {:>4} base f{:.2} a{:.2} c{:.2} d{:.2} | studio f{:.2} a{:.2} c{:.2} d{:.2}",
                row.agent, row.kind, row.session.chars().take(24).collect::<String>(), row.dropped,
                row.baseline.facts, row.baseline.artifacts, row.baseline.continuation, row.baseline.decisions,
                row.studio.facts, row.studio.artifacts, row.studio.continuation, row.studio.decisions
            );
        }
        println!(
            "sessions {} rows {} passing {} tokens/session {}",
            r.sessions,
            r.rows.len(),
            r.studio_passing,
            r.tokens_per_session
        );
        println!("baseline {:?}", r.baseline_mean);
        println!("studio   {:?}", r.studio_mean);
    }
}
