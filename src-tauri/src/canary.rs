//! Canary probes (docs/SELF-HEALING.md, S1): know a provider is broken before a mission does.
//!
//! Every lane in the quota grid (Hermes' primary, then its fallback chain) gets one tiny fixed
//! question with a known answer, through `hermes -z` with the lane's provider and model pinned and
//! a 60 second cap. Only flat-rate lanes are probed unless the owner turns on metered probes
//! (kv `canary.metered` = 1).
//!
//! Hermes answers a pinned turn from its fallback chain when the pinned model fails, and still
//! exits 0, so a reply alone proves nothing: the usage report (`--usage-file`) names the model
//! and provider that actually served the turn, and a turn served by another lane is a failure.
//!
//! Two failures in a row close the lane in the quota grid with the reason; one success reopens
//! it. A failure that points at the provider (auth, quota, no reply) runs the S0.2 auto-heal for
//! that provider at once, outside its cooldown. Each probe's Hermes session is deleted
//! afterwards, so probes never fill the owner's session list.
//!
//! Owner: Kone Tshivhinda, K-ONE IT SOLUTIONS.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::model_layer::Route;
use crate::store;

/// How often an open lane is probed.
pub const EVERY_SECS: i64 = 30 * 60;
/// A lane that failed once is probed again this soon, so a broken lane closes in minutes
/// instead of waiting a full interval for its second failure.
pub const CONFIRM_SECS: i64 = 2 * 60;
/// A closed lane is probed this often, so it reopens soon after the fault is gone.
pub const CLOSED_EVERY_SECS: i64 = 10 * 60;
/// The cap on one probe.
pub const TIMEOUT_SECS: u64 = 60;
/// A correct answer slower than this is reported as slow (still a pass).
pub const SLOW_SECS: f64 = 30.0;
/// Failures in a row that close a lane.
pub const CLOSE_AFTER: u32 = 2;

pub const QUESTION: &str = "What is 17 plus 25? Reply with the number only.";
pub const ANSWER: &str = "42";

/// Set to 1 to probe metered lanes too (OpenRouter and other API-key providers).
pub const METERED_KEY: &str = "canary.metered";

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Outcomes, in the words of docs/SELF-HEALING.md S1.2, plus `fell_back`: Hermes answered from
/// another lane.
pub const OUTCOMES: [&str; 8] = [
    "ok",
    "slow",
    "wrong_answer",
    "malformed_tool_call",
    "no_reply",
    "auth",
    "quota",
    "fell_back",
];

pub fn is_failure(outcome: &str) -> bool {
    !matches!(outcome, "ok" | "slow")
}

/// Outcomes that point at the provider rather than one model: these run the auto-heal.
pub fn provider_level(outcome: &str) -> bool {
    matches!(outcome, "auth" | "quota" | "no_reply")
}

pub fn lane_key(provider: &str, model: &str) -> String {
    format!(
        "canary.lane.{}|{}",
        provider.trim().to_lowercase(),
        model.trim().to_lowercase()
    )
}

/// What the canary last saw on one lane.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LaneState {
    pub provider: String,
    pub model: String,
    /// One of `OUTCOMES`, or empty when the lane was never probed.
    pub outcome: String,
    pub detail: String,
    /// Epoch seconds of the last probe.
    pub at: i64,
    pub secs: f64,
    /// Failures in a row.
    pub fails: u32,
    /// Closed in the quota grid until a probe passes.
    pub closed: bool,
    /// The lane is not probed (metered and not opted in).
    pub skipped: bool,
}

/// The raw result of one `hermes -z` probe.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub timed_out: bool,
    pub exit: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// The `--usage-file` report, when Hermes wrote one.
    pub usage: Option<serde_json::Value>,
    pub secs: f64,
}

fn short(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    one.chars().take(160).collect()
}

/// The answer passes when the only number in it is the expected one. Pure.
pub fn answer_matches(text: &str) -> bool {
    let nums: Vec<&str> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .collect();
    nums == [ANSWER]
}

const QUOTA_MARKERS: [&str; 9] = [
    "entitlement is exhausted",
    "usage_limit",
    "usage limit",
    "rate limit",
    "rate_limit",
    "429",
    "quota",
    "out of credits",
    "refused for billing",
];

const AUTH_MARKERS: [&str; 8] = [
    "401",
    "unauthorized",
    "unauthenticated",
    "not logged in",
    "signed out",
    "invalid api key",
    "authentication",
    "no credentials",
];

const TOOL_MARKERS: [&str; 4] = [
    "malformed",
    "invalid tool call",
    "tool call could not be parsed",
    "failed to parse tool",
];

/// What one probe means for its lane: (outcome, detail). Pure.
pub fn classify(lane: &Route, p: &Probe) -> (&'static str, String) {
    if p.timed_out {
        return ("no_reply", format!("no reply within {TIMEOUT_SECS} s"));
    }
    let usage = p.usage.as_ref();
    let field = |k: &str| {
        usage
            .and_then(|u| u.get(k))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let served_model = field("model");
    let served_provider = field("provider");
    let exit_reason = field("turn_exit_reason");
    let failed = usage
        .and_then(|u| u.get("failed"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let text = format!("{}\n{}\n{}", p.stdout, p.stderr, exit_reason).to_lowercase();
    let mismatch = (!served_model.is_empty()
        && !served_model.eq_ignore_ascii_case(lane.model.trim()))
        || (!served_provider.is_empty()
            && !served_provider.eq_ignore_ascii_case(lane.provider.trim()));
    let errorish =
        failed || mismatch || p.exit.is_some_and(|c| c != 0) || p.stdout.trim().is_empty();
    if errorish {
        if let Some(m) = AUTH_MARKERS.iter().find(|m| text.contains(*m)) {
            return (
                "auth",
                format!("sign-in problem ({m}): {}", short(&p.stderr)),
            );
        }
        if let Some(m) = QUOTA_MARKERS.iter().find(|m| text.contains(*m)) {
            return ("quota", format!("out of quota ({m}): {}", short(&p.stderr)));
        }
    }
    if mismatch {
        return (
            "fell_back",
            format!(
                "Hermes answered from {} via {} instead of this lane",
                if served_model.is_empty() {
                    "?"
                } else {
                    &served_model
                },
                if served_provider.is_empty() {
                    "?"
                } else {
                    &served_provider
                },
            ),
        );
    }
    if TOOL_MARKERS.iter().any(|m| text.contains(m))
        || (!exit_reason.is_empty()
            && !exit_reason.starts_with("text_response")
            && exit_reason.contains("tool"))
    {
        return (
            "malformed_tool_call",
            format!("malformed tool call: {}", short(&exit_reason)),
        );
    }
    if p.stdout.trim().is_empty() || failed {
        let why = if p.stderr.trim().is_empty() {
            exit_reason.clone()
        } else {
            short(&p.stderr)
        };
        return ("no_reply", format!("empty reply: {}", short(&why)));
    }
    if !answer_matches(&p.stdout) {
        return (
            "wrong_answer",
            format!("asked 17 plus 25, got \"{}\"", short(&p.stdout)),
        );
    }
    if p.secs > SLOW_SECS {
        return ("slow", format!("correct, in {:.0} s", p.secs));
    }
    ("ok", format!("correct, in {:.0} s", p.secs))
}

/// The lane's state after one probe. Pure.
pub fn next_state(
    prev: Option<&LaneState>,
    lane: &Route,
    outcome: &str,
    detail: &str,
    at: i64,
    secs: f64,
) -> LaneState {
    let fail = is_failure(outcome);
    let fails = if fail {
        prev.map_or(0, |p| p.fails) + 1
    } else {
        0
    };
    LaneState {
        provider: lane.provider.clone(),
        model: lane.model.clone(),
        outcome: outcome.to_string(),
        detail: detail.to_string(),
        at,
        secs,
        fails,
        closed: fail && (fails >= CLOSE_AFTER || prev.is_some_and(|p| p.closed)),
        skipped: false,
    }
}

/// Whether a lane is due for a probe. Pure.
pub fn due(prev: Option<&LaneState>, now: i64) -> bool {
    let Some(p) = prev.filter(|p| p.at > 0) else {
        return true;
    };
    let wait = if p.closed {
        CLOSED_EVERY_SECS
    } else if p.fails > 0 {
        CONFIRM_SECS
    } else {
        EVERY_SECS
    };
    now - p.at >= wait
}

/// Whether a lane may be probed: flat-rate always, metered only when the owner opted in. Pure.
pub fn probe_allowed(provider: &str, metered_on: bool) -> bool {
    crate::bench::flat_provider(provider) || metered_on
}

fn read_state_in(c: &rusqlite::Connection, provider: &str, model: &str) -> Option<LaneState> {
    store::kv_get(c, &lane_key(provider, model))
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn write_state_in(c: &rusqlite::Connection, s: &LaneState) {
    let _ = store::kv_set(
        c,
        &lane_key(&s.provider, &s.model),
        &serde_json::to_string(s).unwrap_or_default(),
    );
}

/// The reason a lane is closed by the canary, for the quota grid. Reads the store.
pub fn closed_reason_in(c: &rusqlite::Connection, provider: &str, model: &str) -> Option<String> {
    read_state_in(c, provider, model)
        .filter(|s| s.closed)
        .map(|s| {
            format!(
                "the canary failed {} times in a row ({}: {})",
                s.fails, s.outcome, s.detail
            )
        })
}

fn metered_on_in(c: &rusqlite::Connection) -> bool {
    store::kv_get(c, METERED_KEY)
        .ok()
        .flatten()
        .is_some_and(|v| v.trim() == "1")
}

/// Every lane with what the canary last saw on it (never probes). Reads Hermes' routes.
pub fn status() -> Vec<LaneState> {
    let Ok(c) = store::open() else {
        return Vec::new();
    };
    let metered = metered_on_in(&c);
    crate::quota_grid::routes()
        .into_iter()
        .map(|r| {
            let mut s = read_state_in(&c, &r.provider, &r.model).unwrap_or_else(|| LaneState {
                provider: r.provider.clone(),
                model: r.model.clone(),
                ..Default::default()
            });
            s.skipped = !probe_allowed(&r.provider, metered);
            s
        })
        .collect()
}

fn work_dir() -> PathBuf {
    // An empty folder: no AGENTS.md or repository rules reach the probe.
    let d = store::studio_dir().join("canary");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Run one probe through Hermes. The probe's session is deleted afterwards.
pub fn run_probe(lane: &Route, dir: &Path) -> Probe {
    run_probe_in(lane, dir, None)
}

/// Run one probe through Hermes, optionally against a named profile (`hermes -p <profile>`).
/// The probe's session is deleted afterwards.
pub fn run_probe_in(lane: &Route, dir: &Path, profile: Option<&str>) -> Probe {
    let usage_path = dir.join(format!("usage-{}.json", uuid::Uuid::new_v4().simple()));
    let usage_arg = usage_path.display().to_string();
    let started = std::time::Instant::now();
    let mut args: Vec<&str> = Vec::new();
    let prof = profile.map(str::trim).filter(|p| !p.is_empty());
    if let Some(p) = prof {
        args.push("-p");
        args.push(p);
    }
    args.extend_from_slice(&[
        "-z",
        QUESTION,
        "--provider",
        lane.provider.trim(),
        "-m",
        lane.model.trim(),
        "-t",
        "",
        "--usage-file",
        &usage_arg,
    ]);
    let res = crate::procs::run_with_timeout(
        "hermes",
        &args,
        None,
        Some(dir),
        std::time::Duration::from_secs(TIMEOUT_SECS),
    );
    let secs = started.elapsed().as_secs_f64();
    let usage = std::fs::read_to_string(&usage_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let _ = std::fs::remove_file(&usage_path);
    if let Some(sid) = usage
        .as_ref()
        .and_then(|u| u.get("session_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        let mut del_args: Vec<&str> = Vec::new();
        if let Some(p) = prof {
            del_args.push("-p");
            del_args.push(p);
        }
        del_args.extend_from_slice(&["sessions", "delete", "--yes", sid]);
        let _ = crate::procs::run_with_timeout(
            "hermes",
            &del_args,
            None,
            None,
            std::time::Duration::from_secs(30),
        );
    }
    match res {
        Ok((exit, stdout, stderr)) => Probe {
            timed_out: false,
            exit,
            stdout,
            stderr,
            usage,
            secs,
        },
        Err(e) => {
            let msg = format!("{e:#}");
            Probe {
                timed_out: msg.contains("did not finish within"),
                exit: None,
                stdout: String::new(),
                stderr: msg,
                usage,
                secs,
            }
        }
    }
}

/// The S0.2 auto-heal for one provider, told what the canary saw. Bypasses the cooldown.
fn heal(provider: &str, outcome: &str, detail: &str, now: i64) {
    let mut report = crate::provider_health::check(now);
    if let Some(h) = report.providers.iter_mut().find(|h| h.id == provider) {
        if matches!(h.state.as_str(), "ok" | "not_set_up") {
            h.state = if outcome == "quota" {
                "rate_limited".into()
            } else {
                "failing".into()
            };
            h.detail = format!("canary: {detail}");
            h.last_failure = Some(crate::provider_health::Failure {
                at: now,
                provider: provider.to_string(),
                kind: match outcome {
                    "quota" => "rate_limit",
                    "auth" => "auth",
                    "no_reply" => "timeout",
                    _ => "other",
                }
                .into(),
                summary: detail.to_string(),
                resets: None,
            });
        }
    } else {
        return;
    }
    if let Ok(c) = store::open() {
        let _ = crate::provider_health::auto_heal_one_in(&c, &report, provider, now);
    }
}

fn log(summary: &str, detail: &str) {
    let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
        kind: "note",
        from_agent: "canary",
        summary,
        detail,
        ..Default::default()
    });
}

/// Record one probe result for a lane, log a close or reopen, and heal on a provider failure.
/// `probe` is injected so tests and the live check can drive it.
pub fn record_in(
    c: &rusqlite::Connection,
    lane: &Route,
    p: &Probe,
    now: i64,
    heal_on_failure: bool,
) -> LaneState {
    let prev = read_state_in(c, &lane.provider, &lane.model);
    let (outcome, detail) = classify(lane, p);
    let next = next_state(prev.as_ref(), lane, outcome, &detail, now, p.secs);
    write_state_in(c, &next);
    let was_closed = prev.as_ref().is_some_and(|s| s.closed);
    if next.closed && !was_closed {
        log(
            &format!(
                "Canary closed {} via {}: {outcome}",
                lane.model, lane.provider
            ),
            &detail,
        );
    } else if was_closed && !next.closed {
        log(
            &format!("Canary reopened {} via {}", lane.model, lane.provider),
            &detail,
        );
    }
    if heal_on_failure && provider_level(outcome) && !cfg!(test) {
        heal(&lane.provider, outcome, &detail, now);
    }
    next
}

/// Probe the given lanes now, one at a time. Returns the new states.
pub fn probe_lanes(lanes: &[Route], force: bool) -> Vec<LaneState> {
    let Ok(c) = store::open() else {
        return Vec::new();
    };
    let metered = metered_on_in(&c);
    let dir = work_dir();
    let mut out = Vec::new();
    for lane in lanes {
        if !probe_allowed(&lane.provider, metered) {
            continue;
        }
        let now = store::now();
        if !force && !due(read_state_in(&c, &lane.provider, &lane.model).as_ref(), now) {
            continue;
        }
        let p = run_probe(lane, &dir);
        out.push(record_in(&c, lane, &p, store::now(), true));
    }
    out
}

/// Probe every lane of the quota grid now, whether due or not.
pub fn run_now() -> Vec<LaneState> {
    let routes = crate::quota_grid::routes();
    probe_lanes(&routes, true);
    status()
}

/// One scheduler pass: probe the lanes that are due, in a background thread so the minute pass
/// never waits on a 60 second probe. Skipped while the machine is offline.
pub fn step() {
    if cfg!(test) || !crate::agent_limits::online() {
        return;
    }
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let routes = crate::quota_grid::routes();
        probe_lanes(&routes, false);
        RUNNING.store(false, Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(p: &str, m: &str) -> Route {
        Route {
            provider: p.into(),
            model: m.into(),
            base_url: None,
        }
    }

    fn usage(model: &str, provider: &str, reason: &str, failed: bool) -> serde_json::Value {
        serde_json::json!({
            "model": model,
            "provider": provider,
            "turn_exit_reason": reason,
            "failed": failed,
            "session_id": "local-x",
        })
    }

    const AG: &str = "antigravity-subscription-directsdk";

    fn probe(stdout: &str, u: Option<serde_json::Value>, secs: f64) -> Probe {
        Probe {
            timed_out: false,
            exit: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
            usage: u,
            secs,
        }
    }

    #[test]
    fn a_correct_answer_from_the_pinned_lane_passes() {
        let l = lane(AG, "gemini-4-hydrogen-medium");
        let u = usage(
            "gemini-4-hydrogen-medium",
            AG,
            "text_response(finish_reason=stop)",
            false,
        );
        assert_eq!(
            classify(&l, &probe("42\r\n", Some(u.clone()), 12.0)).0,
            "ok"
        );
        assert_eq!(
            classify(&l, &probe("The answer is 42.", Some(u.clone()), 45.0)).0,
            "slow"
        );
        assert_eq!(
            classify(&l, &probe("43", Some(u.clone()), 3.0)).0,
            "wrong_answer"
        );
        assert_eq!(
            classify(&l, &probe("17 + 25 = 42", Some(u), 3.0)).0,
            "wrong_answer"
        );
    }

    /// Seen live on 2026-11-18: `-m no-such-model-xyz` was answered by the fallback with exit 0.
    #[test]
    fn a_turn_answered_by_another_lane_is_a_failure() {
        let l = lane(AG, "no-such-model-xyz");
        let u = usage(
            "gemini-4-hydrogen-high",
            AG,
            "text_response(finish_reason=stop)",
            false,
        );
        let (o, d) = classify(&l, &probe("42", Some(u), 78.0));
        assert_eq!(o, "fell_back");
        assert!(d.contains("gemini-4-hydrogen-high"), "{d}");
        assert!(is_failure(o) && !provider_level(o));
    }

    #[test]
    fn timeouts_auth_quota_and_tool_errors_are_named() {
        let l = lane(
            "claude-subscription-directsdk-experimental",
            "claude-sonnet-5-5",
        );
        let t = Probe {
            timed_out: true,
            ..Default::default()
        };
        assert_eq!(classify(&l, &t).0, "no_reply");
        let mut q = probe("", None, 4.0);
        q.exit = Some(1);
        q.stderr = "API call failed: 429 usage_limit_reached, resets 11:50pm".into();
        assert_eq!(classify(&l, &q).0, "quota");
        let mut a = probe("", None, 4.0);
        a.exit = Some(1);
        a.stderr = "No claude credentials in the pool: not logged in".into();
        assert_eq!(classify(&l, &a).0, "auth");
        let u = usage(
            &l.model,
            &l.provider,
            "max_iterations(tool_call_error)",
            false,
        );
        assert_eq!(
            classify(&l, &probe("ok", Some(u), 4.0)).0,
            "malformed_tool_call"
        );
        let e = usage(&l.model, &l.provider, "error", true);
        assert_eq!(classify(&l, &probe("", Some(e), 4.0)).0, "no_reply");
        // An answer that mentions a quota word is still judged on the answer.
        let ok = usage(
            &l.model,
            &l.provider,
            "text_response(finish_reason=stop)",
            false,
        );
        assert_eq!(
            classify(&l, &probe("42 (quota fine)", Some(ok), 2.0)).0,
            "ok"
        );
    }

    #[test]
    fn two_failures_close_a_lane_and_one_success_reopens_it() {
        let l = lane(AG, "m");
        let s1 = next_state(None, &l, "no_reply", "x", 100, 60.0);
        assert_eq!((s1.fails, s1.closed), (1, false));
        let s2 = next_state(Some(&s1), &l, "fell_back", "y", 220, 60.0);
        assert_eq!((s2.fails, s2.closed), (2, true));
        let s3 = next_state(Some(&s2), &l, "wrong_answer", "z", 900, 3.0);
        assert!(s3.closed, "stays closed while failing");
        let s4 = next_state(Some(&s3), &l, "slow", "ok", 1500, 40.0);
        assert_eq!((s4.fails, s4.closed), (0, false));
    }

    #[test]
    fn probes_are_spaced_by_state() {
        let l = lane(AG, "m");
        assert!(due(None, 0));
        let ok = next_state(None, &l, "ok", "", 1000, 2.0);
        assert!(!due(Some(&ok), 1000 + EVERY_SECS - 1));
        assert!(due(Some(&ok), 1000 + EVERY_SECS));
        let once = next_state(None, &l, "no_reply", "", 1000, 60.0);
        assert!(due(Some(&once), 1000 + CONFIRM_SECS));
        let closed = next_state(Some(&once), &l, "no_reply", "", 1000, 60.0);
        assert!(!due(Some(&closed), 1000 + CLOSED_EVERY_SECS - 1));
        assert!(due(Some(&closed), 1000 + CLOSED_EVERY_SECS));
    }

    #[test]
    fn metered_lanes_are_probed_only_when_opted_in() {
        assert!(probe_allowed(AG, false));
        assert!(probe_allowed(
            "claude-subscription-directsdk-experimental",
            false
        ));
        assert!(!probe_allowed("openrouter", false));
        assert!(probe_allowed("openrouter", true));
    }

    #[test]
    fn a_closed_lane_is_closed_in_the_quota_grid_and_reopens() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let c = store::open().unwrap();
        let l = lane(AG, "gemini-x");
        let bad = Probe {
            timed_out: true,
            ..Default::default()
        };
        record_in(&c, &l, &bad, 100, false);
        assert!(
            closed_reason_in(&c, AG, "gemini-x").is_none(),
            "one failure"
        );
        let s = record_in(&c, &l, &bad, 300, false);
        assert!(s.closed);
        let why = closed_reason_in(&c, AG, "GEMINI-X").unwrap();
        assert!(why.contains("no_reply"), "{why}");
        let good = probe(
            "42",
            Some(usage(
                "gemini-x",
                AG,
                "text_response(finish_reason=stop)",
                false,
            )),
            5.0,
        );
        let s = record_in(&c, &l, &good, 900, false);
        assert!(!s.closed && s.fails == 0);
        assert!(closed_reason_in(&c, AG, "gemini-x").is_none());
        crate::store::set_test_db(None);
    }
}
