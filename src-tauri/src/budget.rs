//! The global metered budget: one ceiling on metered spend per day, week or month across the
//! whole fleet, next to the per-goal and per-mission budgets.
//!
//! Spend is read from Hermes' `state.db`: every session that started in the period, at its
//! actual cost when Hermes has one and its estimate otherwise, skipping flat subscriptions
//! (`usage::is_flat`). That is the money the owner's metered key is actually charged, chats
//! included, not a sum Studio keeps on the side.
//!
//! When spend reaches the limit the stop is hard for metered work and only for metered work:
//! no queued mission that would run on a metered provider starts, a running metered Hermes
//! mission is stopped (blocked with the reason), and the owner gets one escalation per period to
//! raise the limit or keep the stop. Claude Code, Antigravity, Qoder and Hermes on a
//! subscription keep working, because they cost nothing at the margin.

use anyhow::{bail, Result};
use chrono::{Datelike, Duration, Local, TimeZone};
use serde::Serialize;

use crate::missions::Mission;
use crate::store;

pub const KV_LIMIT: &str = "budget.global_usd";
pub const KV_PERIOD: &str = "budget.period";
/// The period start the last over-budget escalation was raised for.
const KV_ASKED: &str = "budget.asked";
pub const PERIODS: [&str; 3] = ["day", "week", "month"];
pub const DEFAULT_PERIOD: &str = "month";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    /// 0 means no global limit.
    pub limit_usd: f64,
    pub period: String,
    /// Unix time the current period started (local midnight; Monday for a week; the 1st for a month).
    pub since: i64,
    pub spent_usd: f64,
    pub exhausted: bool,
}

fn kv(key: &str) -> Option<String> {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, key).ok().flatten())
}

pub fn limit_usd() -> f64 {
    kv(KV_LIMIT)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n >= 0.0)
        .unwrap_or(0.0)
}

pub fn period() -> String {
    kv(KV_PERIOD)
        .map(|s| s.trim().to_lowercase())
        .filter(|p| PERIODS.contains(&p.as_str()))
        .unwrap_or_else(|| DEFAULT_PERIOD.into())
}

pub fn set(limit_usd: f64, period: &str) -> Result<Status> {
    if !limit_usd.is_finite() || limit_usd < 0.0 {
        bail!("the global budget must be 0 (no limit) or a positive amount");
    }
    let p = period.trim().to_lowercase();
    if !PERIODS.contains(&p.as_str()) {
        bail!("the budget period must be day, week or month, got '{period}'");
    }
    let c = store::open()?;
    store::kv_set(&c, KV_LIMIT, &format!("{:.2}", limit_usd))?;
    store::kv_set(&c, KV_PERIOD, &p)?;
    Ok(status())
}

/// Local start of the period that contains `now`. Pure.
pub fn period_start(period: &str, now: chrono::DateTime<Local>) -> i64 {
    let day = now.date_naive();
    let first = match period {
        "day" => day,
        "week" => day - Duration::days(now.weekday().num_days_from_monday() as i64),
        _ => day.with_day(1).unwrap_or(day),
    };
    let midnight = first.and_hms_opt(0, 0, 0).unwrap_or_default();
    Local
        .from_local_datetime(&midnight)
        .earliest()
        .map(|t| t.timestamp())
        .unwrap_or_else(|| now.timestamp())
}

/// Metered spend of Hermes sessions started at or after `since`. Pure over the connection.
pub fn spent_since(c: &rusqlite::Connection, since: i64) -> f64 {
    let Ok(mut st) = c.prepare(
        "SELECT billing_provider, estimated_cost_usd, actual_cost_usd FROM sessions
         WHERE started_at >= ?1",
    ) else {
        return 0.0;
    };
    let rows = st.query_map([since as f64], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<f64>>(1)?,
            r.get::<_, Option<f64>>(2)?,
        ))
    });
    let mut total = 0.0;
    if let Ok(rows) = rows {
        for (provider, est, actual) in rows.flatten() {
            if provider.as_deref().is_some_and(crate::usage::is_flat) {
                continue;
            }
            total += actual.filter(|a| *a > 0.0).or(est).unwrap_or(0.0).max(0.0);
        }
    }
    total
}

/// The budget as it stands now. Pure over its inputs.
pub fn evaluate(limit_usd: f64, period: &str, since: i64, spent_usd: f64) -> Status {
    Status {
        limit_usd,
        period: period.into(),
        since,
        spent_usd,
        exhausted: limit_usd > 0.0 && spent_usd >= limit_usd,
    }
}

pub fn status() -> Status {
    let p = period();
    let since = period_start(&p, Local::now());
    let spent = crate::hermes_engine::open_ro()
        .map(|c| spent_since(&c, since))
        .unwrap_or(0.0);
    evaluate(limit_usd(), &p, since, spent)
}

/// Would this mission spend metered money? Claude Code, Antigravity and Qoder run on
/// subscriptions; Hermes (and `auto`, which may become Hermes) costs what its provider costs.
/// Pure over the Hermes default provider.
pub fn would_meter(m: &Mission, hermes_provider: &str) -> bool {
    match m.agent.trim() {
        "claude-code" | "antigravity" | "qoder" => false,
        _ => {
            let p = if m.provider.trim().is_empty() {
                hermes_provider
            } else {
                m.provider.trim()
            };
            !p.is_empty() && !crate::usage::is_flat(p)
        }
    }
}

/// The question the owner gets once per period when the limit is reached. Pure.
pub fn question(s: &Status) -> String {
    format!(
        "Metered spend this {} is ${:.2}, at or over the ${:.2} global budget. Metered missions are stopped; subscriptions keep working. Double the budget, or keep the stop until the next {}?",
        s.period, s.spent_usd, s.limit_usd, s.period
    )
}

/// One scheduler step: when the limit is reached, ask once for this period. Returns the
/// escalation id when one is open or was raised.
pub fn step(s: &Status) -> Result<Option<String>> {
    if !s.exhausted {
        return Ok(None);
    }
    if let Some(e) = crate::escalation::find_open(None, None, "budget") {
        return Ok(Some(e.id));
    }
    if kv(KV_ASKED).as_deref() == Some(s.since.to_string().as_str()) {
        return Ok(None);
    }
    let e = crate::escalation::raise_and_send(
        None,
        None,
        "budget",
        &question(s),
        &["raise", "stop"],
        "stop",
    )?;
    let c = store::open()?;
    store::kv_set(&c, KV_ASKED, &s.since.to_string())?;
    Ok(Some(e.id))
}

/// The reason `automation` blocks a running metered mission with when the budget is spent.
pub const STOP_REASON: &str = "Stopped: the global metered budget is spent (Mission Control, Automations); it runs again after a raise or in the next period";

/// After a raise, release the missions the stop blocked. Returns their ids.
pub fn release_stopped(why: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for m in crate::missions::list(500)? {
        if m.status == "blocked" && m.review_note.as_deref() == Some(STOP_REASON) {
            crate::missions::unblock(&m.id, &format!("Budget raised: {why}"))?;
            out.push(m.id);
        }
    }
    Ok(out)
}

/// The owner answered raise: double the limit for the rest of the period.
pub fn raise() -> Result<Status> {
    let l = limit_usd();
    if l <= 0.0 {
        return Ok(status());
    }
    set(l * 2.0, &period())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission(agent: &str, provider: &str) -> Mission {
        Mission {
            id: "M-001".into(),
            repo: "demo".into(),
            goal: "g".into(),
            acceptance: "a".into(),
            role: "implementer".into(),
            agent: agent.into(),
            status: "queued".into(),
            provider: provider.into(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: None,
            reviewer: None,
            review_verdict: None,
            pr_url: None,
            review_note: None,
            created_at: 0,
            updated_at: 0,
            verify: None,
        }
    }

    #[test]
    fn periods_start_at_local_midnight_monday_and_the_first() {
        let t = Local.with_ymd_and_hms(2026, 11, 12, 15, 30, 0).unwrap(); // a Thursday
        let day = Local
            .with_ymd_and_hms(2026, 11, 12, 0, 0, 0)
            .unwrap()
            .timestamp();
        let monday = Local
            .with_ymd_and_hms(2026, 11, 9, 0, 0, 0)
            .unwrap()
            .timestamp();
        let first = Local
            .with_ymd_and_hms(2026, 11, 1, 0, 0, 0)
            .unwrap()
            .timestamp();
        assert_eq!(period_start("day", t), day);
        assert_eq!(period_start("week", t), monday);
        assert_eq!(period_start("month", t), first);
    }

    #[test]
    fn spend_counts_metered_sessions_in_the_period_only() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE sessions (id TEXT, started_at REAL, billing_provider TEXT,
                                    estimated_cost_usd REAL, actual_cost_usd REAL);
             INSERT INTO sessions VALUES ('old', 50, 'openrouter', 9.0, NULL);
             INSERT INTO sessions VALUES ('a', 150, 'openrouter', 0.25, NULL);
             INSERT INTO sessions VALUES ('b', 160, 'openrouter', 0.10, 0.30);
             INSERT INTO sessions VALUES ('sub', 170, 'claude-subscription-directsdk-experimental', 4.0, NULL);
             INSERT INTO sessions VALUES ('none', 180, NULL, NULL, NULL);",
        )
        .unwrap();
        let s = spent_since(&c, 100);
        assert!((s - 0.55).abs() < 1e-9, "{s}");
    }

    #[test]
    fn the_stop_is_for_metered_work_only() {
        assert!(
            !evaluate(0.0, "month", 0, 99.0).exhausted,
            "no limit, no stop"
        );
        assert!(!evaluate(5.0, "month", 0, 4.99).exhausted);
        assert!(evaluate(5.0, "month", 0, 5.0).exhausted);
        let or = "openrouter";
        assert!(would_meter(&mission("hermes", ""), or));
        assert!(would_meter(&mission("auto", ""), or));
        assert!(!would_meter(
            &mission("hermes", "claude-subscription-directsdk-experimental"),
            or
        ));
        assert!(!would_meter(&mission("hermes", ""), "antigravity-oauth"));
        assert!(!would_meter(&mission("claude-code", "openrouter"), or));
        assert!(!would_meter(&mission("antigravity", ""), or));
        assert!(would_meter(
            &mission("hermes", "openrouter"),
            "claude-subscription-directsdk-experimental"
        ));
    }

    #[test]
    fn settings_validate_and_one_question_is_asked_per_period() {
        let d = tempfile::tempdir().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        assert!(set(-1.0, "month").is_err());
        assert!(set(5.0, "year").is_err());
        set(5.0, "Week").unwrap();
        assert_eq!((limit_usd(), period().as_str()), (5.0, "week"));
        let s = evaluate(5.0, "week", 1000, 6.0);
        assert!(question(&s).contains("$6.00") && question(&s).contains("$5.00"));
        let first = step(&s).unwrap().expect("asked");
        assert_eq!(
            step(&s).unwrap(),
            Some(first.clone()),
            "the open one is reused"
        );
        crate::escalation::answer(&first, "stop", "studio").unwrap();
        assert_eq!(step(&s).unwrap(), None, "asked once for this period");
        let next = evaluate(5.0, "week", 2000, 6.0);
        assert!(step(&next).unwrap().is_some(), "a new period asks again");
        raise().unwrap();
        assert_eq!(limit_usd(), 10.0);
        store::set_test_db(None);
    }
}
