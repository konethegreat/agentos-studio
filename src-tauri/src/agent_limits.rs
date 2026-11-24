//! Agents that hit their plan's usage limit, and the network being gone.
//!
//! A headless worker on a flat plan (Claude Code, Antigravity) stops with an error when the plan's
//! window is used up: Claude Code ends its turn with `"api_error":"usage_limit_reached"` (HTTP 429)
//! and "You've hit your 5-hour limit". Retrying the same agent then fails the same way, so the
//! stuck rule used to burn its one retry and ask the owner. Instead, Studio reads the limit from
//! the worker log, marks that agent limited until its reset, and hands the mission to another
//! agent at once. Assignment treats a limited agent as out of headroom, so new work goes elsewhere
//! too. While the network is gone, silence is expected: nothing is judged stuck and nothing new is
//! dispatched until it returns.

use std::path::Path;

use crate::store;

/// How long an agent counts as limited when its log gives no reset time Studio can read.
pub const DEFAULT_LIMIT_SECS: i64 = 60 * 60;
/// How long a revoked subscription keeps an agent out of rotation: only its owner can restore it.
pub const DISABLED_REVOKED_SECS: i64 = 7 * 24 * 3600;
/// How long a failed sign-in keeps an agent out of rotation.
pub const DISABLED_AUTH_SECS: i64 = 3600;
/// How much of the end of a worker log is read.
const TAIL_BYTES: u64 = 16 * 1024;

fn key(agent: &str) -> String {
    format!("agent.limited.{}", agent.trim().to_lowercase())
}

fn disabled_key(agent: &str) -> String {
    format!("agent.disabled.{}", agent.trim().to_lowercase())
}

/// Whether a log's tail shows the provider revoked this agent's access ("Your organization has
/// disabled Claude subscription access for Claude Code"). Retrying cannot clear it: only the owner
/// can. Pure.
pub fn access_revoked_in_log(text: &str) -> bool {
    let t = text.to_lowercase();
    (t.contains("disabled") && t.contains("subscription access"))
        || (t.contains("access for") && t.contains("is disabled"))
        || (t.contains("organization") && t.contains("disabled") && t.contains("claude"))
}

/// Whether a worker log's tail shows the agent's plan limit was reached. Pure.
pub fn limit_in_log(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("usage_limit_reached")
        || t.contains("rate_limit_error")
        || t.contains("\"api_error_status\":429")
        || (t.contains("hit your") && t.contains("limit"))
        || t.contains("quota exceeded")
        || t.contains("resource_exhausted")
}

/// The last part of a file, lossily decoded. Empty when it cannot be read.
pub fn tail(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Seconds until the limit resets, when the log says so ("Resets in 53h17m43s", "resets in
/// 45m"). Pure.
pub fn reset_secs(text: &str) -> Option<i64> {
    let t = text.to_lowercase();
    let i = t.rfind("resets in ")? + "resets in ".len();
    let mut total = 0i64;
    let mut num = String::new();
    let mut any = false;
    for ch in t[i..].chars() {
        match ch {
            '0'..='9' => num.push(ch),
            'd' | 'h' | 'm' | 's' if !num.is_empty() => {
                let n: i64 = num.parse().ok()?;
                total += n * match ch {
                    'd' => 86_400,
                    'h' => 3_600,
                    'm' => 60,
                    _ => 1,
                };
                num.clear();
                any = true;
            }
            _ => break,
        }
    }
    (any && total > 0).then_some(total)
}

/// The part of a task log written by its latest worker: after the last `worker started` marker,
/// so a limit an earlier agent hit is not blamed on the agent that took over. Pure.
pub fn latest_run(log: &str) -> &str {
    match log.rfind("worker started") {
        Some(i) => &log[i..],
        None => log,
    }
}

/// Mark an agent limited until `until` (unix seconds).
pub fn mark(agent: &str, until: i64) {
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, &key(agent), &until.to_string());
    }
}

/// When the agent's limit ends, if it is limited now.
pub fn limited_until(agent: &str) -> Option<i64> {
    let c = store::open().ok()?;
    let until: i64 = store::kv_get(&c, &key(agent)).ok()??.parse().ok()?;
    (until > store::now()).then_some(until)
}

pub fn limited(agent: &str) -> bool {
    limited_until(agent).is_some()
}

/// Mark an agent unavailable for `secs` for a reason a retry cannot fix, such as a provider that
/// revoked the subscription or a sign-in that stopped working.
pub fn mark_disabled(agent: &str, secs: i64, reason: &str) {
    if let Ok(c) = store::open() {
        let clean: String = reason
            .replace(['\n', '\r'], " ")
            .trim()
            .chars()
            .take(200)
            .collect();
        let _ = store::kv_set(
            &c,
            &disabled_key(agent),
            &format!("{}|{clean}", store::now() + secs),
        );
    }
}

/// Why the agent is marked unavailable, if the mark has not expired.
pub fn disabled_reason(agent: &str) -> Option<String> {
    let c = store::open().ok()?;
    let raw = store::kv_get(&c, &disabled_key(agent)).ok().flatten()?;
    let (until, reason) = raw.split_once('|')?;
    let until: i64 = until.trim().parse().ok()?;
    (until > store::now() && !reason.trim().is_empty()).then(|| reason.trim().to_string())
}

/// A turn from the agent succeeded: whatever was wrong with it has cleared.
pub fn clear_disabled(agent: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_delete(&c, &disabled_key(agent));
    }
}

/// Why an agent cannot take work right now, in the order that matters to a reader. Pure.
pub fn blocked_reason(installed: bool, limited: bool, disabled: Option<&str>) -> Option<String> {
    if !installed {
        return Some("is not installed on this machine".into());
    }
    if let Some(reason) = disabled {
        return Some(reason.to_string());
    }
    if limited {
        return Some(
            "hit its usage limit and new work goes to another agent until it resets".into(),
        );
    }
    None
}

/// Why this agent cannot take work right now. `None` means installed and free to run.
pub fn blocked_reason_for(agent: &str) -> Option<String> {
    blocked_reason(
        crate::bus::headless_available(agent),
        limited(agent),
        disabled_reason(agent).as_deref(),
    )
}

/// Installed for headless work, not out of its plan, and not cut off by its provider right now.
pub fn usable(agent: &str) -> bool {
    blocked_reason_for(agent).is_none()
}

/// Is the network reachable? Probed at most once a minute; tests always see a network.
pub fn online() -> bool {
    if cfg!(test) {
        return true;
    }
    use std::sync::Mutex;
    static LAST: Mutex<Option<(i64, bool)>> = Mutex::new(None);
    let now = store::now();
    let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, up)) = *last {
        if now - at < 60 {
            return up;
        }
    }
    let up = crate::offline::reachable("1.1.1.1", 443, 800)
        || crate::offline::reachable("api.github.com", 443, 800);
    *last = Some((now, up));
    up
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_claude_limit_line_is_recognised() {
        let line = r#"{"is_error":true,"api_error_status":429,"api_error":"usage_limit_reached","result":"You've hit your 5-hour limit · resets 2:40am (Africa/Johannesburg)"}"#;
        assert!(limit_in_log(line));
        assert!(limit_in_log(
            "Error: RESOURCE_EXHAUSTED: quota exceeded for model"
        ));
        assert!(!limit_in_log(
            r#"{"type":"result","is_error":false,"result":"Opened PR #4"}"#
        ));
        assert!(!limit_in_log("the depth_limit was 0"));
        let log = "===== claude-code worker started 1 =====\nusage_limit_reached\n===== antigravity worker started 2 =====\nworking";
        assert!(
            !limit_in_log(latest_run(log)),
            "the old agent's limit is not the new one's"
        );
        assert!(limit_in_log(latest_run("no marker usage_limit_reached")));
    }

    #[test]
    fn the_reset_time_is_read_when_given() {
        let agy = r#"AGY_ERROR: {"short_error":"RESOURCE_EXHAUSTED (code 429): Individual quota reached. Resets in 53h17m43s."}"#;
        assert_eq!(reset_secs(agy), Some(53 * 3600 + 17 * 60 + 43));
        assert_eq!(reset_secs("limit reached, resets in 45m"), Some(45 * 60));
        assert_eq!(
            reset_secs("You've hit your 5-hour limit · resets 2:40am"),
            None
        );
        assert!(limit_in_log(agy));
    }

    #[test]
    fn a_limit_expires() {
        let d = tempfile::tempdir().unwrap();
        let _g = store::test_env_lock();
        store::set_test_db(Some(d.path().join("studio.db")));
        assert!(!limited("claude-code"));
        mark("Claude-Code", store::now() + 600);
        assert!(limited("claude-code"));
        mark("claude-code", store::now() - 1);
        assert!(!limited("claude-code"));
        store::set_test_db(None);
    }

    #[test]
    fn a_revoked_subscription_is_recognised() {
        let real = "Your organization has disabled Claude subscription access for Claude Code.";
        assert!(access_revoked_in_log(real));
        assert!(access_revoked_in_log(
            "Claude subscription access is disabled for this workspace"
        ));
        assert!(!access_revoked_in_log(
            "You've hit your 5-hour limit · resets 2:40am"
        ));
        assert!(!access_revoked_in_log("usage_limit_reached"));
        assert!(!access_revoked_in_log("the depth_limit was 0"));
    }

    #[test]
    fn a_blocked_reason_names_the_first_blocker() {
        assert_eq!(
            blocked_reason(false, false, None).as_deref(),
            Some("is not installed on this machine")
        );
        assert_eq!(
            blocked_reason(true, true, None).as_deref(),
            Some("hit its usage limit and new work goes to another agent until it resets")
        );
        assert_eq!(
            blocked_reason(true, false, Some("access revoked")).as_deref(),
            Some("access revoked")
        );
        assert_eq!(blocked_reason(true, false, None), None);
    }

    #[test]
    fn a_disabled_agent_says_why_until_a_turn_succeeds() {
        let d = tempfile::tempdir().unwrap();
        let _g = store::test_env_lock();
        store::set_test_db(Some(d.path().join("studio.db")));
        assert!(disabled_reason("claude-code").is_none());
        mark_disabled(
            "claude-code",
            3600,
            "its organization disabled Claude Code access",
        );
        assert!(disabled_reason("claude-code")
            .unwrap()
            .contains("disabled Claude Code"));
        clear_disabled("claude-code");
        assert!(disabled_reason("claude-code").is_none());
        mark_disabled("claude-code", -1, "old reason");
        assert!(
            disabled_reason("claude-code").is_none(),
            "an expired flag is gone"
        );
        store::set_test_db(None);
    }

    #[test]
    fn tail_reads_the_end_of_a_large_log() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("w.log");
        let mut s = "x".repeat(40_000);
        s.push_str("usage_limit_reached");
        std::fs::write(&p, s).unwrap();
        assert!(limit_in_log(&tail(&p)));
        assert_eq!(tail(&d.path().join("missing.log")), "");
    }
}
