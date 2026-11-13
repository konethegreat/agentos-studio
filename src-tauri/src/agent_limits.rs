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
/// How much of the end of a worker log is read.
const TAIL_BYTES: u64 = 16 * 1024;

fn key(agent: &str) -> String {
    format!("agent.limited.{}", agent.trim().to_lowercase())
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

/// Installed for headless work and not out of its plan right now.
pub fn usable(agent: &str) -> bool {
    crate::bus::headless_available(agent) && !limited(agent)
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
