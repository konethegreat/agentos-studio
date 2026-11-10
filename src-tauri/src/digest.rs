//! The digest, morning and evening: what finished, what was spent, what is running and what
//! waits on the owner. Written to `04_Agent_Engine/digests/`, published to Notion and sent to the
//! messaging target through `hermes send` when one is set.
//!
//! Phone replies: the owner answers an escalation by replying `E-3 4821 merge` on the paired
//! messaging platform. The Hermes gateway records every incoming message in its `state.db`; Studio
//! reads new user messages from messaging sessions (read only, never writes `state.db`) and applies
//! a reply whose code matches.

use std::path::PathBuf;

use anyhow::Result;
use chrono::{NaiveDateTime, NaiveTime, Timelike};
use rusqlite::{params, OpenFlags};
use serde::Serialize;

use crate::store;
use crate::vault::Vault;

pub const DEFAULT_TIMES: &str = "07:30,18:00";
/// A slot missed by more than this (the app was closed) is skipped rather than sent late.
const LATE_LIMIT_MINUTES: i64 = 6 * 60;
/// Messaging platforms whose sessions can carry an owner's reply.
pub const PLATFORMS: [&str; 8] = [
    "whatsapp", "telegram", "signal", "discord", "slack", "sms", "matrix", "email",
];

fn kv_get(key: &str) -> Option<String> {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, key).ok().flatten())
}

fn kv_set(key: &str, value: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, key, value);
    }
}

/// `HH:MM` times, comma separated, sorted. Invalid entries are dropped. Pure.
pub fn parse_times(s: &str) -> Vec<NaiveTime> {
    let mut out: Vec<NaiveTime> = s
        .split(',')
        .filter_map(|t| NaiveTime::parse_from_str(t.trim(), "%H:%M").ok())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// kv `digest.times`, default morning 07:30 and evening 18:00.
pub fn times() -> Vec<NaiveTime> {
    let t = parse_times(&kv_get("digest.times").unwrap_or_else(|| DEFAULT_TIMES.into()));
    if t.is_empty() {
        parse_times(DEFAULT_TIMES)
    } else {
        t
    }
}

pub fn set_times(s: &str) -> Result<()> {
    let t = parse_times(s);
    if t.is_empty() {
        anyhow::bail!("give the digest times as HH:MM, comma separated, for example 07:30,18:00");
    }
    let joined: Vec<String> = t.iter().map(|x| x.format("%H:%M").to_string()).collect();
    kv_set("digest.times", &joined.join(","));
    Ok(())
}

/// The slot due now, as `YYYY-MM-DD HH:MM`: the latest digest time today at or before `now`, not
/// yet sent and not missed by more than six hours. Pure.
pub fn due_slot(
    times: &[NaiveTime],
    now: NaiveDateTime,
    last_sent: Option<&str>,
) -> Option<String> {
    let today = now.date();
    let t = times.iter().rev().find(|t| **t <= now.time())?;
    let at = today.and_time(*t);
    if (now - at).num_minutes() > LATE_LIMIT_MINUTES {
        return None;
    }
    let key = at.format("%Y-%m-%d %H:%M").to_string();
    (last_sent != Some(key.as_str())).then_some(key)
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Digest {
    pub title: String,
    pub finished: Vec<String>,
    pub spent_usd: f64,
    pub running: Vec<String>,
    pub waiting: Vec<String>,
    pub flaky: Vec<String>,
    pub late: Vec<String>,
    /// Spend against budget: the global metered budget, then each active goal with a budget.
    pub budget: Vec<String>,
}

/// One line per budget: global first, then each goal that is still in flight. Pure.
pub fn budget_lines(global: &crate::budget::Status, goals: &[(String, f64, f64)]) -> Vec<String> {
    let mut out = Vec::new();
    if global.limit_usd > 0.0 {
        out.push(format!(
            "${:.2} of the ${:.2} global metered budget this {}{}",
            global.spent_usd,
            global.limit_usd,
            global.period,
            if global.exhausted {
                ": spent, metered missions are stopped"
            } else {
                ""
            }
        ));
    }
    for (id, spent, budget) in goals {
        out.push(format!(
            "goal {id}: ${spent:.2} of its ${budget:.2} budget{}",
            if spent > budget { ", over" } else { "" }
        ));
    }
    out
}

/// What happened since `since`, and what stands now.
pub fn build(title: &str, since: i64, today: &str) -> Digest {
    let mut d = Digest {
        title: title.into(),
        ..Default::default()
    };
    let missions = crate::missions::list(500).unwrap_or_default();
    for m in &missions {
        let first = m
            .goal
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(90)
            .collect::<String>();
        if m.updated_at >= since && (m.status == "done" || m.status == "failed") {
            d.finished.push(format!(
                "{} {} ({}, {}){}",
                m.id,
                m.status,
                m.repo,
                m.agent,
                m.pr_url
                    .as_deref()
                    .map(|u| format!(" {u}"))
                    .unwrap_or_default()
            ));
        }
        if m.updated_at >= since {
            d.spent_usd += m.spent_usd;
        }
        if m.status == "running" {
            d.running
                .push(format!("{} {} ({}): {first}", m.id, m.agent, m.repo));
        }
    }
    let mut goal_budgets = Vec::new();
    for g in crate::goals::list(200).unwrap_or_default() {
        if g.budget_usd > 0.0 && !matches!(g.status.as_str(), "done" | "failed") {
            let ms = crate::goals::missions_of(&g.id).unwrap_or_default();
            goal_budgets.push((g.id.clone(), crate::goals::spent(&ms), g.budget_usd));
        }
        if g.updated_at >= since && (g.status == "done" || g.status == "failed") {
            d.finished
                .push(format!("goal {} {}: {}", g.id, g.status, g.outcome));
        }
        if g.status == "awaiting" {
            d.waiting
                .push(format!("goal {}: approve its plan (Watch)", g.id));
        }
        if let Some(dl) = g.deadline.as_deref() {
            if dl < today && g.status != "done" && g.status != "failed" {
                d.late
                    .push(format!("goal {} was due {dl}: {}", g.id, g.outcome));
            }
        }
    }
    for e in crate::escalation::open_list().unwrap_or_default() {
        d.waiting.push(format!(
            "{}: {} ({})",
            e.id,
            e.question,
            e.options.join(" or ")
        ));
    }
    d.budget = budget_lines(&crate::budget::status(), &goal_budgets);
    for e in crate::agent_log::recent(500).unwrap_or_default() {
        if e.at >= since && e.summary.starts_with("flaky test:") {
            d.flaky.push(format!(
                "{}: {}",
                e.mission_id.as_deref().unwrap_or("?"),
                e.summary.trim_start_matches("flaky test: ")
            ));
        }
    }
    d
}

/// The digest as Markdown. Pure.
pub fn render(d: &Digest) -> String {
    let mut s = format!("# {}\n\n", d.title);
    let section = |s: &mut String, head: &str, items: &[String], empty: &str| {
        s.push_str(&format!("## {head}\n\n"));
        if items.is_empty() {
            s.push_str(&format!("- {empty}\n"));
        }
        for i in items {
            s.push_str(&format!("- {i}\n"));
        }
        s.push('\n');
    };
    section(&mut s, "Waiting on you", &d.waiting, "Nothing.");
    section(
        &mut s,
        "Finished",
        &d.finished,
        "Nothing finished since the last digest.",
    );
    s.push_str(&format!(
        "## Spent\n\n- ${:.2} metered since the last digest (flat subscriptions are not counted).\n",
        d.spent_usd
    ));
    for b in &d.budget {
        s.push_str(&format!("- {b}\n"));
    }
    s.push('\n');
    section(&mut s, "Running", &d.running, "Nothing running.");
    if !d.flaky.is_empty() {
        section(&mut s, "Flaky tests", &d.flaky, "");
    }
    if !d.late.is_empty() {
        section(&mut s, "Late", &d.late, "");
    }
    s
}

/// The short phone version: counts and what waits on the owner. Pure.
pub fn phone_text(d: &Digest) -> String {
    let mut s = format!(
        "{}: {} finished, {} running, ${:.2} metered.",
        d.title,
        d.finished.len(),
        d.running.len(),
        d.spent_usd
    );
    if let Some(b) = d.budget.first() {
        s.push_str(&format!(" Budget: {b}."));
    }
    if d.waiting.is_empty() {
        s.push_str(" Nothing waits on you.");
    } else {
        s.push_str(" Waiting on you:");
        for w in d.waiting.iter().take(5) {
            s.push_str(&format!("\n- {w}"));
        }
    }
    s
}

fn digest_dir(v: &Vault) -> PathBuf {
    v.root.join("04_Agent_Engine").join("digests")
}

/// The newest digest note, for the Notion page.
pub fn latest(v: &Vault) -> Option<String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(digest_dir(v))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.sort();
    std::fs::read_to_string(files.last()?).ok()
}

/// Write, publish and send one digest now. Returns the vault-relative path of the note.
pub fn send_now(v: &Vault) -> Result<String> {
    let now = chrono::Local::now();
    let since = kv_get("digest.last_at")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or_else(|| store::now() - 12 * 3600);
    let part = if now.hour() < 12 {
        "morning"
    } else {
        "evening"
    };
    let title = format!("AgentOS {part} digest, {}", now.format("%a %d %b %Y %H:%M"));
    let d = build(&title, since, &now.format("%Y-%m-%d").to_string());
    let text = render(&d);
    let rel = format!(
        "04_Agent_Engine/digests/{}-{:02}{:02}-{}.md",
        now.format("%Y-%m-%d"),
        now.hour(),
        now.minute(),
        if now.hour() < 12 { "am" } else { "pm" }
    );
    v.write_note(&rel, &text)?;
    kv_set("digest.last_at", &store::now().to_string());
    if !cfg!(test) {
        let _ = crate::notion::publish(v, false);
        crate::notion_goals::refresh(v);
        if let Some(target) = crate::hermes_surfaces::notify_target() {
            let _ = crate::hermes_surfaces::send(&target, &phone_text(&d));
        }
    }
    Ok(rel)
}

/// Called every scheduler pass: sends the digest when a slot is due.
pub fn tick(v: &Vault) -> Option<String> {
    let now = chrono::Local::now().naive_local();
    let last = kv_get("digest.last_slot");
    let slot = due_slot(&times(), now, last.as_deref())?;
    kv_set("digest.last_slot", &slot);
    send_now(v).ok()
}

/// A user message in a messaging session of the Hermes gateway. Pure input to `replies`.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    pub at: f64,
    pub text: String,
}

/// New user messages from messaging platforms since `after` (epoch seconds), oldest first.
pub fn incoming_since(after: f64) -> Vec<Incoming> {
    let path = crate::hermes_engine::state_db();
    let Ok(c) = rusqlite::Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    let marks: Vec<String> = PLATFORMS.iter().map(|p| format!("'{p}'")).collect();
    let sql = format!(
        "SELECT m.timestamp, m.content FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.role = 'user' AND m.timestamp > ?1 AND s.source IN ({})
         ORDER BY m.timestamp LIMIT 200",
        marks.join(",")
    );
    let Ok(mut st) = c.prepare(&sql) else {
        return Vec::new();
    };
    st.query_map(params![after], |r| {
        Ok(Incoming {
            at: r.get::<_, f64>(0)?,
            text: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        })
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

/// Apply escalation replies among new phone messages. The first call only records where to start,
/// so old messages are never read as answers. Returns the escalations answered.
pub fn poll_phone_replies() -> Vec<String> {
    let Some(seen) = kv_get("escalation.phone.seen").and_then(|s| s.parse::<f64>().ok()) else {
        kv_set("escalation.phone.seen", &(store::now() as f64).to_string());
        return Vec::new();
    };
    let msgs = incoming_since(seen);
    let mut answered = Vec::new();
    let mut last = seen;
    for m in &msgs {
        last = last.max(m.at);
        if crate::escalation::parse_reply(&m.text).is_some() {
            if let Ok(e) = crate::escalation::answer_from_phone(&m.text) {
                answered.push(e.id);
            }
        }
    }
    if last > seen {
        kv_set("escalation.phone.seen", &last.to_string());
    }
    answered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn times_parse_and_the_due_slot_is_sent_once() {
        let t = parse_times("18:00, 07:30,bad,07:30");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].format("%H:%M").to_string(), "07:30");
        assert_eq!(
            due_slot(&t, at("2026-11-09 07:00"), None),
            None,
            "before the first"
        );
        assert_eq!(
            due_slot(&t, at("2026-11-09 07:31"), None).as_deref(),
            Some("2026-11-09 07:30")
        );
        assert_eq!(
            due_slot(&t, at("2026-11-09 07:45"), Some("2026-11-09 07:30")),
            None,
            "already sent"
        );
        assert_eq!(
            due_slot(&t, at("2026-11-09 18:05"), Some("2026-11-09 07:30")).as_deref(),
            Some("2026-11-09 18:00")
        );
        assert_eq!(
            due_slot(&t, at("2026-11-09 17:00"), Some("2026-11-08 18:00")),
            None,
            "07:30 was missed by more than six hours"
        );
    }

    #[test]
    fn the_digest_leads_with_what_waits_on_the_owner() {
        let d = Digest {
            title: "AgentOS morning digest".into(),
            finished: vec!["M-001 done (agentos-sandbox-1, claude-code)".into()],
            spent_usd: 0.0,
            running: vec![],
            waiting: vec!["E-1: Merge M-002? (merge or skip)".into()],
            flaky: vec![],
            late: vec![],
            budget: budget_lines(
                &crate::budget::evaluate(10.0, "month", 0, 12.5),
                &[("G-001".into(), 1.5, 1.0)],
            ),
        };
        let r = render(&d);
        assert!(r.contains("- $12.50 of the $10.00 global metered budget this month: spent, metered missions are stopped"));
        assert!(r.contains("- goal G-001: $1.50 of its $1.00 budget, over"));
        assert!(budget_lines(&crate::budget::evaluate(0.0, "month", 0, 3.0), &[]).is_empty());
        assert!(r.find("## Waiting on you").unwrap() < r.find("## Finished").unwrap());
        assert!(r.contains("- E-1: Merge M-002? (merge or skip)"));
        assert!(r.contains("- Nothing running."));
        assert!(!r.contains("Flaky"), "empty optional sections are left out");
        let p = phone_text(&d);
        assert!(p.starts_with("AgentOS morning digest: 1 finished, 0 running, $0.00 metered."));
        assert!(p.contains("\n- E-1: Merge M-002?"));
    }

    #[test]
    fn a_digest_note_is_written_and_read_back() {
        let d = tempfile::tempdir().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        let vdir = tempfile::tempdir().unwrap();
        let v = Vault::new(vdir.path());
        let rel = send_now(&v).unwrap();
        assert!(rel.starts_with("04_Agent_Engine/digests/"));
        assert!(latest(&v).unwrap().contains("## Waiting on you"));
        store::set_test_db(None);
    }
}
