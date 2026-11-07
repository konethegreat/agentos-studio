//! The automation tick: what actually makes missions run without a human.
//!
//! One pass does three things, in this order:
//!
//! 1. Missions whose bus task is completed move to review.
//! 2. Queued missions are dispatched while there is room, one per repository.
//! 3. The machine is held awake while anything is running, and released when nothing is.
//!
//! Every decision is a pure function so it can be tested without a clock or a bus.

use crate::missions::{self, Mission};
use crate::store;
use crate::vault::{State, Vault};

pub const KV_ENABLED: &str = "automation.enabled";
pub const KV_MAX: &str = "automation.max_concurrent";
pub const DEFAULT_MAX: usize = 3;

fn kv(key: &str) -> Option<String> {
    let c = store::open().ok()?;
    store::kv_get(&c, key).ok().flatten()
}

fn kv_set(key: &str, value: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, key, value);
    }
}

pub fn enabled() -> bool {
    kv(KV_ENABLED).as_deref() == Some("1")
}

pub fn set_enabled(on: bool) {
    kv_set(KV_ENABLED, if on { "1" } else { "0" });
}

pub fn max_concurrent() -> usize {
    kv(KV_MAX)
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0 && *n <= 12)
        .unwrap_or(DEFAULT_MAX)
}

pub fn set_max_concurrent(n: usize) {
    kv_set(KV_MAX, &n.clamp(1, 12).to_string());
}

/// Which queued missions may start now: never two in the same repository, never past the cap.
pub fn startable(queued: &[&Mission], running: &[&Mission], max: usize) -> Vec<String> {
    let mut busy: Vec<String> = running.iter().map(|m| m.repo.to_lowercase()).collect();
    let mut room = max.saturating_sub(running.len());
    let mut out = Vec::new();
    for m in queued {
        if room == 0 {
            break;
        }
        let repo = m.repo.to_lowercase();
        if busy.contains(&repo) {
            continue;
        }
        busy.push(repo);
        out.push(m.id.clone());
        room -= 1;
    }
    out
}

#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct TickReport {
    pub enabled: bool,
    pub running: usize,
    pub max_concurrent: usize,
    pub moved_to_review: Vec<String>,
    pub dispatched: Vec<String>,
    pub kept_awake: bool,
    pub error: Option<String>,
}

/// One scheduler pass. Safe to call often and safe to call when automations are off:
/// with automations off it still advances running missions, it just starts nothing new.
pub fn tick(v: &Vault) -> TickReport {
    let mut report = TickReport {
        enabled: enabled(),
        max_concurrent: max_concurrent(),
        ..Default::default()
    };
    let all = match missions::list(300) {
        Ok(m) => m,
        Err(e) => {
            report.error = Some(e.to_string());
            return report;
        }
    };

    // 1. Finished work goes to the reviewer, never straight to done.
    for m in all.iter().filter(|m| m.status == "running") {
        let Some(task) = m.task_id.as_deref() else {
            continue;
        };
        if crate::kanban::is_kanban(Some(task)) {
            match advance_kanban(m, task) {
                Ok(true) => report.moved_to_review.push(m.id.clone()),
                Ok(false) => {}
                Err(e) => report.error = Some(e.to_string()),
            }
            continue;
        }
        if v.task_state(task) == Some(State::Completed) {
            // The worker's summary names its pull request; keep it before the body moves on.
            if let Some(body) = v.task_body(task) {
                if let Some(url) = missions::pull_url(&body) {
                    let _ = missions::set_pr(&m.id, &url);
                }
            }
            match missions::submit_for_review(&m.id, 0.0) {
                Ok(_) => report.moved_to_review.push(m.id.clone()),
                Err(e) => report.error = Some(e.to_string()),
            }
        }
    }

    // Re-read: statuses changed in step 1.
    let all = missions::list(300).unwrap_or_default();
    let running: Vec<&Mission> = all.iter().filter(|m| m.status == "running").collect();
    report.running = running.len();

    // 2. Start what can start.
    if report.enabled {
        let queued: Vec<&Mission> = all.iter().filter(|m| m.status == "queued").collect();
        for id in startable(&queued, &running, report.max_concurrent) {
            match missions::start(v, &id) {
                Ok(_) => report.dispatched.push(id),
                Err(e) => report.error = Some(e.to_string()),
            }
        }
    }

    // 3. Hold the machine awake only while there is work in flight.
    report.running = missions::list(300)
        .map(|l| l.iter().filter(|m| m.status == "running").count())
        .unwrap_or(report.running);
    keep_awake(report.running > 0);
    report.kept_awake = report.running > 0;
    report
}

/// A running Hermes mission: read its kanban task. Finished work is measured (metered spend of
/// every Hermes session it used), checked with `hermes verify`, and sent to Studio's reviewer;
/// kanban's own review pass is never taken as approval. Returns true when it moved to review.
fn advance_kanban(m: &Mission, task: &str) -> anyhow::Result<bool> {
    use crate::kanban::{self, Next};
    let view = kanban::show(task)?;
    let spent = crate::usage::metered_cost(&kanban::session_ids(&view));
    match kanban::next(&view) {
        Next::Wait => {
            if over_budget(m, spent) {
                missions::block(
                    &m.id,
                    &format!(
                        "Stopped: metered spend ${spent:.2} passed the ${:.2} budget",
                        m.budget_usd
                    ),
                    false,
                )?;
            }
            Ok(false)
        }
        Next::Block(reason) => {
            missions::block(&m.id, &format!("Hermes kanban: {reason}"), false)?;
            Ok(false)
        }
        Next::Review(summaries) => {
            if let Some(url) = missions::pull_url(&summaries) {
                missions::set_pr(&m.id, &url)?;
            }
            if let Some(dir) = crate::review_agent::project_dir(&m.repo).filter(|d| d.is_dir()) {
                missions::set_verify(&m.id, &kanban::verify(&dir))?;
            }
            missions::submit_for_review(&m.id, spent)?;
            Ok(true)
        }
    }
}

/// A metered budget that this attempt alone has passed. Subscriptions never count. Pure.
pub fn over_budget(m: &Mission, attempt_spend: f64) -> bool {
    m.budget_usd > 0.0 && m.spent_usd + attempt_spend > m.budget_usd
}

/// Prevent the machine from sleeping while missions are in flight.
/// On non-Windows targets this is a no-op, because the equivalent call differs per platform.
#[cfg(windows)]
pub fn keep_awake(on: bool) {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    #[link(name = "kernel32")]
    extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }
    let flags = if on {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    // SAFETY: the call only sets a process-wide execution state flag and takes no pointers.
    unsafe {
        SetThreadExecutionState(flags);
    }
}

#[cfg(not(windows))]
pub fn keep_awake(_on: bool) {}

/// The scheduler thread: one pass a minute while the app is open.
pub fn start_scheduler(vault_root: std::path::PathBuf) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
        if !enabled() {
            keep_awake(false);
            continue;
        }
        let v = Vault::new(&vault_root);
        let _ = tick(&v);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, repo: &str, status: &str) -> Mission {
        Mission {
            id: id.into(),
            repo: repo.into(),
            goal: "g".into(),
            acceptance: "a".into(),
            role: "implementer".into(),
            agent: "hermes".into(),
            status: status.into(),
            provider: "claude-subscription".into(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: None,
            reviewer: None,
            review_verdict: None,
            review_note: None,
            pr_url: None,
            created_at: 0,
            updated_at: 0,
            verify: None,
        }
    }

    #[test]
    fn hermes_missions_use_kanban_and_the_rest_keep_the_bus() {
        assert!(missions::uses_kanban(&m("M-1", "a", "queued")));
        for agent in ["claude-code", "antigravity", "qoder"] {
            let mut x = m("M-2", "a", "queued");
            x.agent = agent.into();
            assert!(
                !missions::uses_kanban(&x),
                "{agent} keeps the Agent Bus path"
            );
        }
        // Both paths share the scheduler's caps: startable() does not look at the agent.
        let q = [m("M-1", "a", "queued"), {
            let mut x = m("M-2", "a", "queued");
            x.agent = "claude-code".into();
            x
        }];
        let queued: Vec<&Mission> = q.iter().collect();
        assert_eq!(
            startable(&queued, &[], 5),
            vec!["M-1"],
            "one per repository, any agent"
        );
    }

    #[test]
    fn only_a_metered_overrun_stops_a_mission() {
        let mut x = m("M-1", "a", "running");
        assert!(!over_budget(&x, 50.0), "no budget set means no ceiling");
        x.budget_usd = 1.0;
        x.spent_usd = 0.4;
        assert!(!over_budget(&x, 0.5));
        assert!(over_budget(&x, 0.7));
    }

    #[test]
    fn one_mission_per_repository_and_never_past_the_cap() {
        let q = [
            m("M-1", "a", "queued"),
            m("M-2", "a", "queued"),
            m("M-3", "b", "queued"),
            m("M-4", "c", "queued"),
        ];
        let queued: Vec<&Mission> = q.iter().collect();
        let started = startable(&queued, &[], 3);
        assert_eq!(started, vec!["M-1", "M-3", "M-4"]);

        let running = [m("M-9", "b", "running")];
        let r: Vec<&Mission> = running.iter().collect();
        let started = startable(&queued, &r, 2);
        assert_eq!(started, vec!["M-1"], "b is busy and the cap is 2");

        assert!(startable(&queued, &r, 1).is_empty());
    }

    #[test]
    fn a_finished_task_moves_the_mission_to_review_not_done() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let v = Vault::new(d.path().join("vault"));
        v.scaffold().unwrap();

        let mission = missions::create(
            "repo",
            "goal",
            "acceptance",
            "implementer",
            "hermes",
            "claude-subscription",
            "",
            0.0,
        )
        .unwrap();
        let task = v
            .delegate_task(crate::vault::DelegateArgs {
                title: "t".into(),
                assigned_to: "hermes".into(),
                creator: "test".into(),
                project: "repo".into(),
                context_files: Vec::new(),
                instructions: "do it".into(),
                priority: "high".into(),
            })
            .unwrap();
        let task_id = task["id"].as_str().unwrap().to_string();
        missions::dispatch(&mission.id, &task_id).unwrap();
        v.claim_task(&task_id, "hermes").unwrap();
        v.complete_task(&task_id, "hermes", "done").unwrap();

        let report = tick(&v);
        assert_eq!(report.moved_to_review, vec![mission.id.clone()]);
        let after = missions::get(&mission.id).unwrap().unwrap();
        assert_eq!(after.status, "review");
        assert_ne!(
            after.status, "done",
            "the scheduler never approves its own work"
        );
        crate::store::set_test_db(None);
    }
}
