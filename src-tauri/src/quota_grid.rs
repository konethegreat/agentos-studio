//! The quota grid (Beyond B2): every flat subscription used, and no Hermes worker sent to a model
//! that has run out.
//!
//! The lanes are the owner's own Hermes routes, in his order: the primary model, then the
//! fallback chain (`hermes config get model`, `fallback_providers`). Studio adds no model of its
//! own. A lane is closed when its provider has no headroom (`hermes usage`, through the agent
//! cards' cache) or when a worker reported that its model ran out of credits or entitlement
//! (remembered in kv `quota.exhausted.<model>` until it is likely to have reset).
//!
//! When a Hermes mission with no pinned model starts, the grid steers it: the primary when its
//! lane is open, otherwise the first open fallback (pinned for that attempt only), otherwise the
//! mission waits for the soonest reset instead of starting on a dead lane.

use anyhow::Result;
use serde::Serialize;

use crate::agents::{self, Quota};
use crate::missions::Mission;
use crate::model_layer::Route;
use crate::store;

/// How long a model reported out of credits or entitlement stays closed when the error names no
/// reset: the length of the shortest subscription window.
pub const EXHAUSTED_SECS: i64 = 5 * 3600;

/// The shortest wait when every lane is closed and no reset time is known.
pub const MIN_HOLD_SECS: i64 = 15 * 60;

pub fn exhausted_key(model: &str) -> String {
    format!("quota.exhausted.{}", model.trim().to_lowercase())
}

/// The model a provider error names as out of credits or entitlement. Pure.
pub fn exhausted_model(reason: &str) -> Option<String> {
    let re =
        regex::Regex::new(r"(?i)entitlement is exhausted for ([A-Za-z0-9._:/\-\[\]]+)").ok()?;
    let m = re
        .captures(reason)?
        .get(1)?
        .as_str()
        .trim_end_matches(['.', ',', ';', ':', '\'', '"']);
    (!m.is_empty()).then(|| m.to_string())
}

fn kv(key: &str) -> Option<String> {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, key).ok().flatten())
}

/// Remember a model a worker reported as spent. Returns the model, if the reason named one.
pub fn note_failure(reason: &str, now: i64) -> Option<String> {
    let model = exhausted_model(reason)?;
    if let Ok(c) = store::open() {
        let _ = store::kv_set(
            &c,
            &exhausted_key(&model),
            &(now + EXHAUSTED_SECS).to_string(),
        );
    }
    Some(model)
}

/// Hermes logs every model its provider refused (`<model> via <provider> refused for
/// billing/credits`), including ones a fallback then covered, which no mission error shows.
/// Returns (model, epoch) for each refusal. Pure.
pub fn parse_refusals(log: &str) -> Vec<(String, i64)> {
    let Ok(re) = regex::Regex::new(
        r"^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})\S* WARNING .*?: ([A-Za-z0-9._:/\-\[\]]+) via \S+ refused for billing/credits",
    ) else {
        return Vec::new();
    };
    log.lines()
        .filter_map(|l| {
            let c = re.captures(l)?;
            let t = chrono::NaiveDateTime::parse_from_str(c.get(1)?.as_str(), "%Y-%m-%d %H:%M:%S")
                .ok()?
                .and_local_timezone(chrono::Local)
                .earliest()?
                .timestamp();
            Some((c.get(2)?.as_str().to_string(), t))
        })
        .collect()
}

/// The end of Hermes' agent log: enough for the last several hours.
fn agent_log_tail() -> String {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 4 * 1024 * 1024;
    let path = crate::procs::hermes_home().join("logs").join("agent.log");
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(TAIL)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Refusals Hermes logged since `since` (epoch).
pub fn refusals_since(since: i64) -> Vec<(String, i64)> {
    parse_refusals(&agent_log_tail())
        .into_iter()
        .filter(|(_, t)| *t >= since)
        .collect()
}

/// Close every model Hermes saw refused in the last `EXHAUSTED_SECS`, until 5 hours after its
/// latest refusal. Returns the models closed.
pub fn learn_from_log(now: i64) -> Vec<String> {
    let mut latest: std::collections::BTreeMap<String, i64> = Default::default();
    for (m, t) in refusals_since(now - EXHAUSTED_SECS) {
        let e = latest.entry(m.to_lowercase()).or_insert(t);
        *e = (*e).max(t);
    }
    let Ok(c) = store::open() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (m, t) in latest {
        let until = t + EXHAUSTED_SECS;
        if exhausted_until(&m, now).map_or(true, |u| u < until) {
            let _ = store::kv_set(&c, &exhausted_key(&m), &until.to_string());
        }
        out.push(m);
    }
    out
}

/// Until when a model is closed, if it still is.
pub fn exhausted_until(model: &str, now: i64) -> Option<i64> {
    kv(&exhausted_key(model))
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|t| *t > now)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Lane {
    pub provider: String,
    pub model: String,
    /// The Hermes primary; the rest are the fallback chain in order.
    pub primary: bool,
    pub headroom_pct: Option<f64>,
    pub resets_at: Option<String>,
    pub exhausted_until: Option<i64>,
    pub open: bool,
    pub why: String,
}

fn hhmm(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_else(|| epoch.to_string())
}

/// One lane from its route, its provider's quota and the exhaustion memory. Pure.
pub fn lane(route: &Route, primary: bool, quota: &Quota, exhausted: Option<i64>) -> Lane {
    let room = agents::has_headroom(quota);
    let why = match exhausted {
        Some(t) => format!(
            "{} ran out of credits or entitlement; closed until about {}",
            route.model,
            hhmm(t)
        ),
        None if !room => format!("{}: {}", route.provider, quota.summary),
        None => match quota.headroom_pct {
            Some(h) => format!("{h:.0}% left in the tightest window"),
            None => "room to run (the provider reports no limits)".into(),
        },
    };
    Lane {
        provider: route.provider.clone(),
        model: route.model.clone(),
        primary,
        headroom_pct: quota.headroom_pct,
        resets_at: quota.resets_at.clone(),
        exhausted_until: exhausted,
        open: exhausted.is_none() && room,
        why,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Steer {
    /// The primary has room: Hermes runs as configured.
    Primary,
    /// The primary is closed: pin this attempt to the first open fallback.
    Pin {
        provider: String,
        model: String,
        why: String,
    },
    /// Every lane is closed: wait for the soonest reset.
    Hold { until: i64, why: String },
}

/// Where a new Hermes mission should run. Pure.
pub fn steer(lanes: &[Lane], now: i64) -> Steer {
    let Some(first) = lanes.first() else {
        return Steer::Primary;
    };
    if first.open {
        return Steer::Primary;
    }
    if let Some(l) = lanes.iter().skip(1).find(|l| l.open) {
        return Steer::Pin {
            provider: l.provider.clone(),
            model: l.model.clone(),
            why: format!(
                "the primary {} is closed ({}); this attempt runs on {} ({})",
                first.model, first.why, l.model, l.why
            ),
        };
    }
    let soonest = lanes
        .iter()
        .filter_map(|l| {
            l.exhausted_until.or_else(|| {
                l.resets_at
                    .as_deref()
                    .and_then(|r| chrono::DateTime::parse_from_rfc3339(r).ok())
                    .map(|d| d.timestamp())
            })
        })
        .filter(|t| *t > now)
        .min()
        .unwrap_or(now + MIN_HOLD_SECS)
        .max(now + 60);
    Steer::Hold {
        until: soonest,
        why: format!(
            "every Hermes route is closed ({}); waiting until about {}",
            lanes
                .iter()
                .map(|l| format!("{}: {}", l.model, l.why))
                .collect::<Vec<_>>()
                .join("; "),
            hhmm(soonest)
        ),
    }
}

/// When one lane frees up (Beyond B2.7).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Forecast {
    pub provider: String,
    pub model: String,
    pub open_now: bool,
    /// Epoch seconds when the lane is expected to be usable; `None` when it is open now or unknown.
    pub opens_at: Option<i64>,
    /// Plain words for the owner.
    pub summary: String,
    /// True when a heavy mission should wait for this lane rather than run on a thinner one.
    pub heavy_ready: bool,
}

/// Per-lane reset forecast. A heavy mission (the primary route) is "ready" only when the primary
/// is open; otherwise the summary says when it frees up. Pure.
pub fn forecast(lanes: &[Lane], now: i64) -> Vec<Forecast> {
    lanes
        .iter()
        .map(|l| {
            let opens = l.exhausted_until.or_else(|| {
                l.resets_at
                    .as_deref()
                    .and_then(|r| chrono::DateTime::parse_from_rfc3339(r).ok())
                    .map(|d| d.timestamp())
            });
            let opens = opens.filter(|t| *t > now);
            let summary = if l.open {
                "open now".to_string()
            } else {
                match opens {
                    Some(t) => format!("frees up about {}", hhmm(t)),
                    None => "closed, no reset time reported".to_string(),
                }
            };
            Forecast {
                provider: l.provider.clone(),
                model: l.model.clone(),
                open_now: l.open,
                opens_at: if l.open { None } else { opens },
                summary,
                heavy_ready: l.primary && l.open,
            }
        })
        .collect()
}

fn config_route(v: &serde_yaml::Value) -> Option<Route> {
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    Some(Route {
        provider: get("provider")?,
        model: get("default").or_else(|| get("model"))?,
        base_url: get("base_url"),
    })
}

/// The owner's Hermes routes right now: the primary, then the fallback chain.
pub fn routes() -> Vec<Route> {
    let primary = crate::procs::run("hermes", &["config", "get", "model"])
        .filter(|o| o.status.success())
        .and_then(|o| serde_yaml::from_slice::<serde_yaml::Value>(&o.stdout).ok())
        .and_then(|v| config_route(&v));
    let mut out: Vec<Route> = primary.into_iter().collect();
    for r in crate::model_layer::fallback_chain() {
        if !out
            .iter()
            .any(|o| o.provider == r.provider && o.model == r.model)
        {
            out.push(r);
        }
    }
    out
}

/// Every lane with its live state. `refresh` forces new quota readings.
pub fn lanes(refresh: bool) -> Vec<Lane> {
    let now = store::now();
    learn_from_log(now);
    let mut quotas: std::collections::HashMap<String, Quota> = std::collections::HashMap::new();
    routes()
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let q = quotas
                .entry(r.provider.clone())
                .or_insert_with(|| agents::quota_for(&r.provider, refresh))
                .clone();
            lane(r, i == 0, &q, exhausted_until(&r.model, now))
        })
        .collect()
}

/// Steer a Hermes mission about to start. `Ok(None)`: run it as configured. `Ok(Some(m))`: run
/// this attempt on the pinned fallback. `Err`: every lane is closed and the mission now waits
/// (recorded as a deferral the scheduler honours).
pub fn steer_mission(m: &Mission) -> Result<Option<Mission>> {
    if !m.model.trim().is_empty() {
        return Ok(None);
    }
    let now = store::now();
    match steer(&lanes(false), now) {
        Steer::Primary => Ok(None),
        Steer::Pin {
            provider,
            model,
            why,
        } => {
            let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
                mission_id: Some(&m.id),
                kind: "note",
                from_agent: "quota grid",
                summary: &why,
                ..Default::default()
            });
            let mut pinned = m.clone();
            pinned.provider = provider;
            pinned.model = model;
            Ok(Some(pinned))
        }
        Steer::Hold { until, why } => {
            if crate::assign::deferred_until(&m.id) != Some(until) {
                crate::assign::set_deferred(&m.id, until)?;
                let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
                    mission_id: Some(&m.id),
                    kind: "defer",
                    from_agent: "quota grid",
                    summary: &why,
                    detail: &format!("until {until}"),
                    ..Default::default()
                });
            }
            anyhow::bail!("{}: waiting for a quota reset: {why}", m.id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(p: &str, m: &str) -> Route {
        Route {
            provider: p.into(),
            model: m.into(),
            base_url: None,
        }
    }

    fn quota(headroom: Option<f64>, resets: Option<&str>) -> Quota {
        Quota {
            source: "hermes usage".into(),
            headroom_pct: headroom,
            resets_at: resets.map(Into::into),
            summary: match headroom {
                Some(h) => format!("{h:.0}% left"),
                None => "unknown".into(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn the_real_e16_error_names_the_spent_model() {
        let e16 = "Worker's last output: 'billing, credits, or account entitlement is exhausted for gemini-4-hydrogen-high. Add credits or update billing with that provider, then retry.'";
        assert_eq!(
            exhausted_model(e16).as_deref(),
            Some("gemini-4-hydrogen-high")
        );
        assert_eq!(exhausted_model("npm test failed"), None);
        assert_eq!(
            exhausted_key("Gemini-4-Hydrogen-High"),
            "quota.exhausted.gemini-4-hydrogen-high"
        );
    }

    #[test]
    fn a_lane_is_open_only_with_headroom_and_an_unspent_model() {
        let r = route("antigravity-oauth", "gemini-4-hydrogen-high");
        assert!(lane(&r, false, &quota(Some(98.0), None), None).open);
        assert!(
            lane(&r, false, &quota(None, None), None).open,
            "unknown counts as room"
        );
        assert!(!lane(&r, false, &quota(Some(2.0), None), None).open);
        let spent = lane(&r, false, &quota(Some(98.0), None), Some(2_000_000_000));
        assert!(!spent.open);
        assert!(spent.why.contains("ran out of credits"), "{}", spent.why);
    }

    #[test]
    fn the_grid_keeps_the_primary_steers_past_a_spent_fallback_or_holds() {
        let now = 1_000_000;
        let primary = lane(
            &route("claude", "claude-opus"),
            true,
            &quota(Some(50.0), None),
            None,
        );
        let medium = lane(
            &route("antigravity-oauth", "gemini-4-hydrogen-medium"),
            false,
            &quota(Some(97.0), None),
            None,
        );
        let high = lane(
            &route("antigravity-oauth", "gemini-4-hydrogen-high"),
            false,
            &quota(Some(97.0), None),
            Some(now + 3600),
        );
        assert_eq!(
            steer(&[primary.clone(), high.clone(), medium.clone()], now),
            Steer::Primary
        );

        // E-16: Claude spent, Hydrogen High out of entitlement. The grid skips both.
        let spent_primary = lane(
            &route("claude", "claude-opus"),
            true,
            &quota(Some(1.0), Some("1970-01-13T00:00:00+00:00")),
            None,
        );
        match steer(&[spent_primary.clone(), high.clone(), medium.clone()], now) {
            Steer::Pin { model, why, .. } => {
                assert_eq!(model, "gemini-4-hydrogen-medium");
                assert!(why.contains("claude-opus"), "{why}");
            }
            other => panic!("expected a pin, got {other:?}"),
        }

        // Nothing open: wait for the soonest reset (Hydrogen High's memory, then Claude's window).
        let spent_medium = lane(
            &route("antigravity-oauth", "gemini-4-hydrogen-medium"),
            false,
            &quota(Some(1.0), None),
            None,
        );
        match steer(&[spent_primary, high, spent_medium], now) {
            Steer::Hold { until, .. } => assert_eq!(until, now + 3600),
            other => panic!("expected a hold, got {other:?}"),
        }
        match steer(
            &[lane(&route("p", "m"), true, &quota(Some(0.0), None), None)],
            now,
        ) {
            Steer::Hold { until, .. } => assert_eq!(until, now + MIN_HOLD_SECS),
            other => panic!("expected a hold, got {other:?}"),
        }
        assert_eq!(
            steer(&[], now),
            Steer::Primary,
            "no routes known: leave it to Hermes"
        );
    }

    #[test]
    fn forecast_says_when_a_closed_lane_frees_up_and_gates_heavy_work() {
        let now = 1_000_000;
        let closed = Lane {
            provider: "p".into(),
            model: "m1".into(),
            primary: true,
            headroom_pct: Some(0.0),
            resets_at: None,
            exhausted_until: Some(now + 3600),
            open: false,
            why: "spent".into(),
        };
        let open = Lane {
            primary: false,
            model: "m2".into(),
            exhausted_until: None,
            open: true,
            ..closed.clone()
        };
        let f = forecast(&[closed, open], now);
        assert!(!f[0].heavy_ready && f[0].opens_at == Some(now + 3600));
        assert!(f[0].summary.starts_with("frees up about"));
        assert!(f[1].open_now && f[1].opens_at.is_none() && !f[1].heavy_ready);
    }

    #[test]
    fn a_spent_model_is_remembered_until_it_resets() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let now = 1_000_000;
        assert_eq!(
            note_failure("entitlement is exhausted for gemini-4-hydrogen-high.", now).as_deref(),
            Some("gemini-4-hydrogen-high")
        );
        assert_eq!(
            exhausted_until("gemini-4-hydrogen-high", now),
            Some(now + EXHAUSTED_SECS)
        );
        assert_eq!(
            exhausted_until("gemini-4-hydrogen-high", now + EXHAUSTED_SECS),
            None
        );
        assert_eq!(note_failure("a test failed", now), None);
        crate::store::set_test_db(None);
    }

    #[test]
    fn refusals_are_read_from_hermes_own_log() {
        let log = "2026-11-16 21:52:57,294 WARNING [local-32ea] agent.chat_completion_helpers: Profile default: gemini-4-argon-medium via antigravity-oauth refused for billing/credits \u{2014} using fallback gemini-4-hydrogen-medium via antigravity-oauth. To\n\
2026-11-16 21:53:00,000 INFO [local-32ea] agent.conversation_loop: API call #2\n\
2026-11-16 21:55:49,493 WARNING [local-32ea] agent.chat_completion_helpers: Profile default: gemini-4-hydrogen-medium via antigravity-oauth refused for billing/credits \u{2014} using fallback gemini-4-hydrogen-high\n";
        let r = parse_refusals(log);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "gemini-4-argon-medium");
        assert_eq!(r[1].0, "gemini-4-hydrogen-medium");
        assert_eq!(r[1].1 - r[0].1, 2 * 60 + 52);
        assert!(parse_refusals("no refusals here\n").is_empty());
    }

    #[test]
    fn the_primary_route_reads_hermes_config_get_model() {
        let v: serde_yaml::Value = serde_yaml::from_str(
            "default: gemini-4-argon-medium\nprovider: antigravity-oauth\nbase_url: agy://local\n",
        )
        .unwrap();
        let r = config_route(&v).unwrap();
        assert_eq!(
            (r.provider.as_str(), r.model.as_str()),
            ("antigravity-oauth", "gemini-4-argon-medium")
        );
        assert!(config_route(&serde_yaml::from_str("provider: x\n").unwrap()).is_none());
    }
}
