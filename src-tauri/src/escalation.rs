//! Escalations: the only questions Studio asks the owner while a goal runs.
//!
//! Six things escalate (`KINDS`); everything else goes into the digest. An escalation has a
//! question, the allowed answers, a safe answer and a deadline. It is answered in Studio, or from
//! the phone with the escalation id plus a four-digit code that only went to the phone (only the
//! code's SHA-256 is stored). When the deadline passes with no answer the safe answer is taken,
//! the same rule as Hermes approvals: doing nothing never runs anything.

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::store;

/// Budget passed, second reject, blocked or stuck after retry, a destructive approval, a merge
/// outside the allow list, a done check failing twice.
pub const KINDS: [&str; 6] = [
    "budget",
    "second_reject",
    "stuck",
    "destructive",
    "merge",
    "done_check",
];

const DEFAULT_TIMEOUT_MINUTES: i64 = 60;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Escalation {
    pub id: String,
    pub goal_id: Option<String>,
    pub mission_id: Option<String>,
    pub kind: String,
    pub question: String,
    pub options: Vec<String>,
    pub safe_answer: String,
    /// `open`, `answered` or `expired` (the safe answer was taken at the deadline).
    pub status: String,
    pub answer: Option<String>,
    /// `studio`, `phone` or `timeout`.
    pub answered_by: Option<String>,
    pub created_at: i64,
    pub deadline_at: i64,
    pub answered_at: Option<i64>,
}

/// Minutes an escalation waits before the safe answer is taken (kv `escalation.timeout_minutes`).
pub fn timeout_minutes() -> i64 {
    store::open()
        .ok()
        .and_then(|c| {
            store::kv_get(&c, "escalation.timeout_minutes")
                .ok()
                .flatten()
        })
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| (5..=7 * 24 * 60).contains(n))
        .unwrap_or(DEFAULT_TIMEOUT_MINUTES)
}

pub fn set_timeout_minutes(n: i64) -> Result<()> {
    if !(5..=7 * 24 * 60).contains(&n) {
        bail!("the escalation timeout must be between 5 minutes and 7 days");
    }
    let c = store::open()?;
    store::kv_set(&c, "escalation.timeout_minutes", &n.to_string())
}

fn hash_code(code: &str) -> String {
    let d = Sha256::digest(code.trim().as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fresh four-digit code. Not a secret worth more than the phone it goes to: it proves a reply
/// came from whoever received the message, not from an agent reading the vault.
fn new_code() -> String {
    let b = uuid::Uuid::new_v4().into_bytes();
    let n = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) % 10_000;
    format!("{n:04}")
}

fn next_id(c: &Connection) -> Result<String> {
    let mut st = c.prepare("SELECT id FROM escalations")?;
    let max = st
        .query_map([], |r| r.get::<_, String>(0))?
        .flatten()
        .filter_map(|id| id.strip_prefix("E-").and_then(|n| n.parse::<i64>().ok()))
        .max()
        .unwrap_or(0);
    Ok(format!("E-{}", max + 1))
}

const COLUMNS: &str =
    "id, goal_id, mission_id, kind, question, options, safe_answer, status, answer,
                       answered_by, created_at, deadline_at, answered_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Escalation> {
    let options: String = r.get(5)?;
    Ok(Escalation {
        id: r.get(0)?,
        goal_id: r.get(1)?,
        mission_id: r.get(2)?,
        kind: r.get(3)?,
        question: r.get(4)?,
        options: serde_json::from_str(&options).unwrap_or_default(),
        safe_answer: r.get(6)?,
        status: r.get(7)?,
        answer: r.get(8)?,
        answered_by: r.get(9)?,
        created_at: r.get(10)?,
        deadline_at: r.get(11)?,
        answered_at: r.get(12)?,
    })
}

pub fn get(id: &str) -> Result<Option<Escalation>> {
    let c = store::open()?;
    Ok(c.query_row(
        &format!("SELECT {COLUMNS} FROM escalations WHERE id = ?1"),
        params![id],
        row,
    )
    .optional()?)
}

/// Open escalations first, newest first.
pub fn list(limit: usize) -> Result<Vec<Escalation>> {
    let c = store::open()?;
    let mut st = c.prepare(&format!(
        "SELECT {COLUMNS} FROM escalations
         ORDER BY CASE status WHEN 'open' THEN 0 ELSE 1 END, created_at DESC LIMIT ?1"
    ))?;
    let rows = st.query_map(params![limit as i64], row)?;
    Ok(rows.flatten().collect())
}

pub fn open_list() -> Result<Vec<Escalation>> {
    Ok(list(500)?
        .into_iter()
        .filter(|e| e.status == "open")
        .collect())
}

/// The open escalation of this kind for a mission (or for a goal when `mission_id` is None).
pub fn find_open(
    goal_id: Option<&str>,
    mission_id: Option<&str>,
    kind: &str,
) -> Option<Escalation> {
    open_list().ok()?.into_iter().find(|e| {
        e.kind == kind && e.mission_id.as_deref() == mission_id && e.goal_id.as_deref() == goal_id
    })
}

/// The newest decided escalation of this kind for a mission or goal, answered or expired.
pub fn latest_decided(
    goal_id: Option<&str>,
    mission_id: Option<&str>,
    kind: &str,
) -> Option<Escalation> {
    list(500).ok()?.into_iter().find(|e| {
        e.status != "open"
            && e.kind == kind
            && e.mission_id.as_deref() == mission_id
            && e.goal_id.as_deref() == goal_id
    })
}

/// What the owner sees and the phone receives. Pure.
pub fn message(e: &Escalation, code: &str) -> String {
    let at = match (&e.goal_id, &e.mission_id) {
        (Some(g), Some(m)) => format!(" ({g}, {m})"),
        (Some(g), None) => format!(" ({g})"),
        (None, Some(m)) => format!(" ({m})"),
        _ => String::new(),
    };
    format!(
        "AgentOS Studio needs you{at}: {}\nReply \"{} {code} <answer>\" with one of: {}. With no answer by the deadline Studio takes \"{}\".",
        e.question,
        e.id,
        e.options.join(", "),
        e.safe_answer
    )
}

/// Ask the owner. An open escalation of the same kind for the same mission or goal is returned
/// instead of a second one. Returns the escalation and, for a new one, the phone code.
pub fn raise(
    goal_id: Option<&str>,
    mission_id: Option<&str>,
    kind: &str,
    question: &str,
    options: &[&str],
    safe_answer: &str,
) -> Result<(Escalation, Option<String>)> {
    if !KINDS.contains(&kind) {
        bail!("unknown escalation kind '{kind}'");
    }
    if !options.contains(&safe_answer) {
        bail!("the safe answer must be one of the options");
    }
    if let Some(e) = find_open(goal_id, mission_id, kind) {
        return Ok((e, None));
    }
    let code = new_code();
    let c = store::open()?;
    let id = next_id(&c)?;
    let now = store::now();
    c.execute(
        "INSERT INTO escalations (id, goal_id, mission_id, kind, question, options, safe_answer,
                                  status, code_hash, created_at, deadline_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'open', ?8, ?9, ?10)",
        params![
            id,
            goal_id,
            mission_id,
            kind,
            question.trim(),
            serde_json::to_string(options)?,
            safe_answer,
            hash_code(&code),
            now,
            now + timeout_minutes() * 60
        ],
    )?;
    let e = get(&id)?.ok_or_else(|| anyhow::anyhow!("escalation vanished after insert"))?;
    Ok((e, Some(code)))
}

/// Raise and deliver: the phone gets the message with the code when a messaging target is set.
/// Delivery is best effort; Studio always shows the escalation.
pub fn raise_and_send(
    goal_id: Option<&str>,
    mission_id: Option<&str>,
    kind: &str,
    question: &str,
    options: &[&str],
    safe_answer: &str,
) -> Result<Escalation> {
    let (e, code) = raise(goal_id, mission_id, kind, question, options, safe_answer)?;
    if let Some(code) = code {
        if !cfg!(test) {
            if let Some(target) = crate::hermes_surfaces::notify_target() {
                let msg = message(&e, &code);
                let _ = crate::hermes_surfaces::send(&target, &msg);
            }
        }
    }
    Ok(e)
}

/// Record the owner's answer. `by` is `studio` or `phone`.
pub fn answer(id: &str, choice: &str, by: &str) -> Result<Escalation> {
    let e = get(id)?.ok_or_else(|| anyhow::anyhow!("no escalation '{id}'"))?;
    if e.status != "open" {
        bail!("{id} is already {}", e.status);
    }
    let choice = choice.trim().to_lowercase();
    if !e.options.iter().any(|o| o == &choice) {
        bail!(
            "'{choice}' is not an answer to {id}; use one of: {}",
            e.options.join(", ")
        );
    }
    let c = store::open()?;
    c.execute(
        "UPDATE escalations SET status = 'answered', answer = ?1, answered_by = ?2,
                answered_at = ?3 WHERE id = ?4 AND status = 'open'",
        params![choice, by, store::now(), id],
    )?;
    get(id)?.ok_or_else(|| anyhow::anyhow!("no escalation '{id}'"))
}

/// An answer from the phone: `E-3 4821 merge`. The code must match the one sent. Pure parse.
pub fn parse_reply(text: &str) -> Option<(String, String, String)> {
    let mut words = text.split_whitespace();
    let id = words
        .next()?
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
    let id = id.to_uppercase();
    let n = id.strip_prefix("E-")?;
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let code = words.next()?.trim_matches(|c: char| !c.is_ascii_digit());
    if code.len() != 4 {
        return None;
    }
    let choice = words
        .next()?
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '-')
        .to_lowercase();
    if choice.is_empty() {
        return None;
    }
    Some((id, code.to_string(), choice))
}

/// Apply a phone reply when its code matches; a wrong code is refused.
pub fn answer_from_phone(text: &str) -> Result<Escalation> {
    let (id, code, choice) =
        parse_reply(text).ok_or_else(|| anyhow::anyhow!("not an escalation reply"))?;
    let c = store::open()?;
    let stored: Option<String> = c
        .query_row(
            "SELECT code_hash FROM escalations WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if stored.as_deref() != Some(hash_code(&code).as_str()) {
        bail!("the code in the reply to {id} does not match");
    }
    answer(&id, &choice, "phone")
}

/// Take the safe answer for every open escalation past its deadline. Returns those expired.
pub fn expire_due(now: i64) -> Result<Vec<Escalation>> {
    let due: Vec<Escalation> = open_list()?
        .into_iter()
        .filter(|e| e.deadline_at <= now)
        .collect();
    let c = store::open()?;
    for e in &due {
        c.execute(
            "UPDATE escalations SET status = 'expired', answer = safe_answer,
                    answered_by = 'timeout', answered_at = ?1 WHERE id = ?2 AND status = 'open'",
            params![now, e.id],
        )?;
    }
    let mut out = Vec::new();
    for e in due {
        if let Some(x) = get(&e.id)? {
            out.push(x);
        }
    }
    Ok(out)
}

/// Remove a goal's escalations (live tests and goal delete).
pub fn delete_for_goal(goal_id: &str) -> Result<usize> {
    let c = store::open()?;
    Ok(c.execute(
        "DELETE FROM escalations WHERE goal_id = ?1",
        params![goal_id],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_db<T>(f: impl FnOnce() -> T) -> T {
        let d = tempfile::tempdir().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        let out = f();
        store::set_test_db(None);
        out
    }

    #[test]
    fn a_reply_names_the_escalation_the_code_and_the_answer() {
        assert_eq!(
            parse_reply("E-3 4821 merge"),
            Some(("E-3".into(), "4821".into(), "merge".into()))
        );
        assert_eq!(
            parse_reply("  e-12  0042  Stop. "),
            Some(("E-12".into(), "0042".into(), "stop".into()))
        );
        assert_eq!(parse_reply("E-3 merge"), None, "no code");
        assert_eq!(parse_reply("E-x 4821 merge"), None);
        assert_eq!(parse_reply("hello there"), None);
        assert_eq!(parse_reply("E-3 482 merge"), None, "four digits");
    }

    #[test]
    fn one_open_question_per_mission_and_kind_with_a_safe_default() {
        with_db(|| {
            let (e, code) = raise(
                Some("G-001"),
                Some("M-001"),
                "merge",
                "Merge agentos-sandbox-1#9? The repository is not on the merge allow list.",
                &["merge", "skip"],
                "skip",
            )
            .unwrap();
            assert_eq!(e.id, "E-1");
            assert_eq!(e.status, "open");
            assert!(code.as_deref().is_some_and(|c| c.len() == 4));
            let (again, none) = raise(
                Some("G-001"),
                Some("M-001"),
                "merge",
                "again",
                &["merge", "skip"],
                "skip",
            )
            .unwrap();
            assert_eq!((again.id.as_str(), none), ("E-1", None));
            assert!(raise(None, None, "vibes", "q", &["a"], "a").is_err());
            assert!(raise(None, None, "merge", "q", &["merge"], "skip").is_err());
            let m = message(&e, "1234");
            assert!(m.contains("(G-001, M-001)") && m.contains("Reply \"E-1 1234 <answer>\""));
            assert!(m.contains("Studio takes \"skip\""));
        })
    }

    #[test]
    fn an_answer_must_be_an_option_and_a_phone_reply_needs_the_code() {
        with_db(|| {
            let (e, code) = raise(
                None,
                Some("M-002"),
                "stuck",
                "Retry M-002?",
                &["retry", "stop"],
                "stop",
            )
            .unwrap();
            let code = code.unwrap();
            assert!(answer(&e.id, "maybe", "studio").is_err());
            let wrong = if code == "0000" { "1111" } else { "0000" };
            assert!(answer_from_phone(&format!("{} {wrong} retry", e.id)).is_err());
            let done = answer_from_phone(&format!("{} {code} retry", e.id)).unwrap();
            assert_eq!(done.status, "answered");
            assert_eq!(done.answer.as_deref(), Some("retry"));
            assert_eq!(done.answered_by.as_deref(), Some("phone"));
            assert!(answer(&e.id, "stop", "studio").is_err(), "already answered");
            assert_eq!(
                latest_decided(None, Some("M-002"), "stuck").map(|x| x.id),
                Some(e.id.clone())
            );
            assert!(find_open(None, Some("M-002"), "stuck").is_none());
        })
    }

    #[test]
    fn no_answer_by_the_deadline_takes_the_safe_answer() {
        with_db(|| {
            let (e, _) = raise(
                Some("G-001"),
                None,
                "budget",
                "Over budget",
                &["raise", "stop"],
                "stop",
            )
            .unwrap();
            assert!(expire_due(e.deadline_at - 1).unwrap().is_empty());
            let gone = expire_due(e.deadline_at).unwrap();
            assert_eq!(gone.len(), 1);
            assert_eq!(gone[0].status, "expired");
            assert_eq!(gone[0].answer.as_deref(), Some("stop"));
            assert_eq!(gone[0].answered_by.as_deref(), Some("timeout"));
            assert_eq!(timeout_minutes(), 60);
            set_timeout_minutes(15).unwrap();
            assert_eq!(timeout_minutes(), 15);
            assert!(set_timeout_minutes(1).is_err());
            assert_eq!(delete_for_goal("G-001").unwrap(), 1);
        })
    }
}
