//! The fleet pause switch: one click stops all new work.
//!
//! Paused, Studio starts no mission, merges nothing and creates no fix mission; work already
//! running finishes and still goes to review, and escalations still time out to their safe answer.
//! Hermes is paused too with `hermes pause`, its global stop (the `ESTOP` file in its home), which
//! halts cron dispatch, kanban dispatch and new gateway work. Resume reverses both. The state is
//! kv `fleet.paused` (`<epoch>|<reason>`).

use anyhow::Result;
use serde::Serialize;

use crate::store;

const KEY: &str = "fleet.paused";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct State {
    pub paused: bool,
    pub since: Option<i64>,
    pub reason: String,
    /// Hermes' own stop is engaged (its `ESTOP` file exists).
    pub hermes_paused: bool,
}

/// `<epoch>|<reason>`. Pure.
pub fn parse(raw: Option<&str>) -> (bool, Option<i64>, String) {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return (false, None, String::new());
    };
    let (at, reason) = raw.split_once('|').unwrap_or((raw, ""));
    (true, at.parse().ok(), reason.to_string())
}

fn raw() -> Option<String> {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, KEY).ok().flatten())
}

/// Is the fleet paused? Cheap: read every scheduler pass.
pub fn paused() -> bool {
    parse(raw().as_deref()).0
}

fn hermes_estop() -> std::path::PathBuf {
    crate::procs::hermes_home().join("ESTOP")
}

pub fn state() -> State {
    let (paused, since, reason) = parse(raw().as_deref());
    State {
        paused,
        since,
        reason,
        hermes_paused: hermes_estop().exists(),
    }
}

fn hermes(args: &[&str]) -> Result<()> {
    let (code, _, err) = crate::procs::run_with_timeout(
        "hermes",
        args,
        None,
        None,
        std::time::Duration::from_secs(60),
    )?;
    if code != Some(0) {
        anyhow::bail!("hermes {} failed: {}", args.join(" "), err.trim());
    }
    Ok(())
}

/// Pause Studio and Hermes. Studio's pause is recorded first, so it holds even if Hermes fails.
pub fn pause(reason: &str) -> Result<State> {
    let reason = if reason.trim().is_empty() {
        "paused from AgentOS Studio"
    } else {
        reason.trim()
    };
    let c = store::open()?;
    store::kv_set(&c, KEY, &format!("{}|{reason}", store::now()))?;
    if !cfg!(test) {
        hermes(&["pause", "--reason", reason])?;
    }
    Ok(state())
}

/// Resume Studio and Hermes.
pub fn resume() -> Result<State> {
    let c = store::open()?;
    store::kv_delete(&c, KEY)?;
    if !cfg!(test) && hermes_estop().exists() {
        hermes(&["resume"])?;
    }
    Ok(state())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pause_carries_its_time_and_reason() {
        assert_eq!(parse(None), (false, None, String::new()));
        assert_eq!(parse(Some("")), (false, None, String::new()));
        assert_eq!(
            parse(Some("1794300000|going offline")),
            (true, Some(1794300000), "going offline".into())
        );
        assert_eq!(
            parse(Some("1794300000")),
            (true, Some(1794300000), String::new())
        );
    }

    #[test]
    fn pause_and_resume_flip_the_switch() {
        let d = tempfile::tempdir().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        assert!(!paused());
        let s = pause("").unwrap();
        assert!(s.paused && paused());
        assert_eq!(s.reason, "paused from AgentOS Studio");
        assert!(!resume().unwrap().paused);
        assert!(!paused());
        store::set_test_db(None);
    }
}
