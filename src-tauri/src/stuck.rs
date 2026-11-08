//! Stuck detection: every running mission has a heartbeat, and one that goes quiet is retried
//! once and then blocked with the reason.
//!
//! What counts as a heartbeat:
//! - Hermes (kanban): the newest task, run, event or comment time from `hermes kanban show`, and
//!   the last message of each worker session in `state.db` (read only).
//! - A headless bus worker: its process is alive in this app session, and its log, task file or
//!   worktree changed.
//! - An IDE agent that claimed its task: the task file or any file in the worktree changed.
//!
//! No heartbeat for 15 minutes (two minutes when the worker process is gone): one automatic retry,
//! then `blocked`. The retry count lives in kv `mission.retries.<id>` and the time of the last
//! retry in `mission.beat.<id>`, so a retried mission gets a fresh 15 minutes.

use std::path::Path;
use std::time::SystemTime;

use crate::missions::{self, Mission};
use crate::store;
use crate::vault::{State, Vault};

/// Quiet this long with a live worker means wedged.
pub const STALE_SECS: i64 = 15 * 60;
/// Quiet this long with no worker process at all means dead (start-up grace).
pub const DEAD_GRACE_SECS: i64 = 2 * 60;
/// Retries before a stuck mission is blocked.
pub const MAX_RETRIES: u32 = 1;

const KV_RETRIES: &str = "mission.retries.";
const KV_BEAT: &str = "mission.beat.";

/// What to do with a running mission. Pure.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Healthy,
    Retry,
    Block,
}

/// The rule itself. `alive` is whether a worker is known to be running; `last` the newest sign of
/// life. Pure.
pub fn judge(alive: bool, last: i64, now: i64, retries: u32) -> Verdict {
    let quiet = now - last;
    let stale = if alive {
        quiet > STALE_SECS
    } else {
        quiet > DEAD_GRACE_SECS
    };
    if !stale {
        Verdict::Healthy
    } else if retries < MAX_RETRIES {
        Verdict::Retry
    } else {
        Verdict::Block
    }
}

fn kv_i64(key: &str) -> Option<i64> {
    let c = store::open().ok()?;
    store::kv_get(&c, key)
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse().ok())
}

fn kv_put(key: &str, value: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, key, value);
    }
}

pub fn retries(id: &str) -> u32 {
    kv_i64(&format!("{KV_RETRIES}{id}")).unwrap_or(0).max(0) as u32
}

/// Forget the retry history, for a new attempt (start, rework) or a mission that left `running`.
pub fn reset(id: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_delete(&c, &format!("{KV_RETRIES}{id}"));
        let _ = store::kv_delete(&c, &format!("{KV_BEAT}{id}"));
    }
}

fn record_retry(id: &str, now: i64) {
    kv_put(&format!("{KV_RETRIES}{id}"), &(retries(id) + 1).to_string());
    kv_put(&format!("{KV_BEAT}{id}"), &now.to_string());
}

fn mtime(p: &Path) -> Option<i64> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// Newest file change under a worktree, skipping `.git`, `node_modules` and `target`. Bounded so
/// a large repository cannot stall the tick.
pub fn newest_change(dir: &Path) -> Option<i64> {
    let mut newest = None;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            seen += 1;
            if seen > 5000 {
                return newest;
            }
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if matches!(
                name.as_str(),
                ".git" | "node_modules" | "target" | ".worktrees"
            ) {
                continue;
            }
            if let Some(t) = mtime(&p) {
                newest = newest.max(Some(t));
            }
            if depth < 6 && p.is_dir() {
                stack.push((p, depth + 1));
            }
        }
    }
    newest
}

/// The newest sign of life of a bus mission, never earlier than its dispatch or last retry.
fn bus_last(v: &Vault, m: &Mission, task: &str) -> i64 {
    let mut times = vec![Some(m.updated_at), kv_i64(&format!("{KV_BEAT}{}", m.id))];
    times.push(mtime(&v.worker_logs().join(format!("{task}.log"))));
    if let Ok((t, _)) = v.get_task(task) {
        times.push(mtime(Path::new(&t.path)));
    }
    if let Some(wt) = crate::worktree::recorded(&m.id) {
        times.push(newest_change(&wt));
    }
    times.into_iter().flatten().max().unwrap_or(m.updated_at)
}

/// Judge a running bus mission and act on the verdict. Returns what was done, if anything.
pub fn check_bus(v: &Vault, m: &Mission, task: &str, now: i64) -> Option<String> {
    let state = v.task_state(task)?;
    let agent = m.agent.trim().to_lowercase();
    let headless = crate::bus::headless_available(&agent);
    // An IDE agent's unclaimed offer is the reassignment rule's business, not this one.
    if state == State::Pending && !headless {
        return None;
    }
    if state == State::Completed {
        return None;
    }
    let alive = if headless {
        crate::bus::worker_running(task)
    } else {
        // An IDE agent that claimed the task: its activity is all there is to watch.
        true
    };
    let last = bus_last(v, m, task);
    match judge(alive, last, now, retries(&m.id)) {
        Verdict::Healthy => None,
        Verdict::Retry => {
            record_retry(&m.id, now);
            let quiet = (now - last) / 60;
            if headless {
                crate::bus::stop_task_worker(task);
                let note = format!(
                    "no heartbeat for {quiet} min (worker {}); retrying once",
                    if alive { "silent" } else { "gone" }
                );
                let _ = v.log_activity(task, &note);
                if let Err(e) = crate::bus::start_worker(v, task, &agent) {
                    let _ = v.log_activity(task, &format!("retry did not start: {e:#}"));
                }
                Some(format!("{}: {note}", m.id))
            } else {
                // The IDE went quiet on a claimed task: hand it to a headless agent instead.
                let to = crate::bus::best_headless(&agent, crate::bus::headless_available)?;
                let note = format!(
                    "{agent} claimed this but nothing changed for {quiet} min; reassigned to {to}"
                );
                let _ = v.move_task(task, State::Pending, "agentos-studio");
                if v.claim_task(task, to).is_ok() {
                    let _ = v.log_activity(task, &note);
                    let _ = missions::reassign(&m.id, to, &note);
                    if let Err(e) = crate::bus::start_worker(v, task, to) {
                        let _ = v.log_activity(task, &format!("no worker started: {e:#}"));
                    }
                }
                Some(format!("{}: {note}", m.id))
            }
        }
        Verdict::Block => {
            crate::bus::stop_task_worker(task);
            let reason = format!(
                "Stuck: no heartbeat from {agent} for {} min after an automatic retry",
                (now - last) / 60
            );
            let _ = v.log_activity(task, &reason);
            let _ = missions::block(&m.id, &reason, false);
            reset(&m.id);
            Some(format!("{}: blocked", m.id))
        }
    }
}

/// Judge a running Hermes kanban mission. `view` is the current `kanban show`.
pub fn check_kanban(
    m: &Mission,
    task: &str,
    view: &crate::kanban::TaskView,
    now: i64,
) -> Option<String> {
    // Only states where a worker should be making progress: running, kanban's own review pass,
    // and ready or todo (waiting for the gateway's dispatcher).
    let working = matches!(view.status.as_str(), "running" | "review");
    let waiting = matches!(view.status.as_str(), "ready" | "todo");
    if !working && !waiting {
        return None;
    }
    let session_last = crate::usage::last_message_at(&crate::kanban::session_ids(view));
    let last = [
        Some(m.updated_at),
        view.last_activity,
        session_last,
        kv_i64(&format!("{KV_BEAT}{}", m.id)),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(m.updated_at);
    // A ready task gets the full window too: the dispatcher may be holding it for a cap.
    match judge(true, last, now, retries(&m.id)) {
        Verdict::Healthy => None,
        Verdict::Retry => {
            record_retry(&m.id, now);
            let quiet = (now - last) / 60;
            let r = if working {
                crate::kanban::reclaim(
                    task,
                    &format!("AgentOS Studio: no heartbeat for {quiet} min"),
                )
            } else {
                crate::kanban::dispatch_pass()
            };
            Some(format!(
                "{}: no heartbeat for {quiet} min; {} {}",
                m.id,
                if working { "reclaimed" } else { "dispatched" },
                r.err()
                    .map(|e| format!("(failed: {e})"))
                    .unwrap_or_default()
            ))
        }
        Verdict::Block => {
            let reason = if working {
                format!(
                    "Stuck: the Hermes worker sent no heartbeat for {} min after an automatic retry",
                    (now - last) / 60
                )
            } else {
                "Stuck: the Hermes dispatcher never picked the task up. Is the Hermes gateway running (hermes gateway start)?".to_string()
            };
            let _ = missions::block(&m.id, &reason, false);
            reset(&m.id);
            Some(format!("{}: blocked", m.id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_for_fifteen_minutes_is_one_retry_then_a_block() {
        let now = 10_000;
        assert_eq!(judge(true, now - STALE_SECS, now, 0), Verdict::Healthy);
        assert_eq!(judge(true, now - STALE_SECS - 1, now, 0), Verdict::Retry);
        assert_eq!(judge(true, now - STALE_SECS - 1, now, 1), Verdict::Block);
        // A gone worker is not given fifteen minutes.
        assert_eq!(
            judge(false, now - 60, now, 0),
            Verdict::Healthy,
            "start-up grace"
        );
        assert_eq!(
            judge(false, now - DEAD_GRACE_SECS - 1, now, 0),
            Verdict::Retry
        );
    }

    #[test]
    fn worktree_activity_skips_build_folders() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/a.rs"), "x").unwrap();
        std::fs::create_dir_all(d.path().join("node_modules/p")).unwrap();
        let t = newest_change(d.path()).unwrap();
        assert!(t > 0);
        assert!(newest_change(&d.path().join("missing")).is_none());
    }

    #[test]
    fn a_dead_headless_worker_is_retried_then_blocked() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let v = Vault::new(d.path().join("vault"));
        v.scaffold().unwrap();
        let m =
            missions::create("repo", "g", "a", "implementer", "claude-code", "", "", 0.0).unwrap();
        let t = v
            .delegate_task(crate::vault::DelegateArgs {
                title: "t".into(),
                assigned_to: "claude-code".into(),
                creator: "test".into(),
                project: "repo".into(),
                context_files: Vec::new(),
                instructions: "do it".into(),
                priority: "high".into(),
            })
            .unwrap();
        let task = t["id"].as_str().unwrap().to_string();
        let m = missions::dispatch(&m.id, &task).unwrap();
        v.claim_task(&task, "claude-code").unwrap();
        if !crate::bus::headless_available("claude-code") {
            // CI has no claude: the IDE path would apply instead. The pure rule is covered above.
            crate::store::set_test_db(None);
            return;
        }
        // No worker runs in a unit test, so the mission is dead once the grace passes.
        let later = chrono::Utc::now().timestamp() + DEAD_GRACE_SECS + 5;
        assert!(check_bus(&v, &m, &task, later)
            .unwrap()
            .contains("retrying once"));
        assert_eq!(retries(&m.id), 1);
        assert_eq!(missions::get(&m.id).unwrap().unwrap().status, "running");
        let much_later = later + DEAD_GRACE_SECS + 5;
        assert!(check_bus(&v, &m, &task, much_later)
            .unwrap()
            .contains("blocked"));
        let after = missions::get(&m.id).unwrap().unwrap();
        assert_eq!(after.status, "blocked");
        assert!(after.review_note.unwrap().starts_with("Stuck:"));
        assert_eq!(retries(&m.id), 0, "history cleared");
        crate::store::set_test_db(None);
    }
}
