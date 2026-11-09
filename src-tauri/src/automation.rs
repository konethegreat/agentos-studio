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
/// Minutes an IDE-only agent has to pick up its mission before it goes to a headless agent.
pub const KV_IDE_WAIT: &str = "missions.ide_wait_minutes";
pub const DEFAULT_IDE_WAIT_MIN: i64 = 30;

pub fn ide_wait_minutes() -> i64 {
    kv(KV_IDE_WAIT)
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| (1..=1440).contains(n))
        .unwrap_or(DEFAULT_IDE_WAIT_MIN)
}

pub fn set_ide_wait_minutes(n: i64) {
    kv_set(KV_IDE_WAIT, &n.clamp(1, 1440).to_string());
}

/// A running bus mission whose agent cannot run headless here and has not claimed the task within
/// the wait goes to a headless agent. Pure.
pub fn reassign_due(
    m: &Mission,
    task_pending: bool,
    agent_runs_headless: bool,
    now: i64,
    wait_secs: i64,
) -> bool {
    m.status == "running" && task_pending && !agent_runs_headless && now - m.updated_at >= wait_secs
}

/// Offer over: move the task and the mission to the best available headless agent and start it.
/// With no headless agent installed the mission is blocked, never left running with no worker.
fn reassign(v: &Vault, m: &Mission, task: &str) -> anyhow::Result<String> {
    let from = m.agent.trim().to_lowercase();
    let Some(to) = crate::bus::best_headless(&from, crate::bus::headless_available) else {
        missions::block(
            &m.id,
            &format!("{from} did not pick this up and no headless agent is installed"),
            false,
        )?;
        anyhow::bail!("{}: no headless agent to take over", m.id);
    };
    let reason = format!(
        "{from} is IDE only and did not pick this up within {} minutes; reassigned to {to}",
        ide_wait_minutes()
    );
    v.claim_task(task, to)?;
    let _ = v.log_activity(task, &reason);
    missions::reassign(&m.id, to, &reason)?;
    if let Err(e) = crate::bus::start_worker(v, task, to) {
        let _ = v.log_activity(task, &format!("no worker started: {e:#}"));
    }
    Ok(to.to_string())
}

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

/// Which queued missions may start now, never past the global cap. Per repository: with
/// `per_repo` 1 a mission runs alone in its repository; above 1, up to `per_repo` missions share
/// it, each in its own worktree (kanban worktrees for Hermes, Studio worktrees for the rest). Pure.
pub fn startable(
    queued: &[&Mission],
    running: &[&Mission],
    max: usize,
    per_repo: usize,
) -> Vec<String> {
    let parallel = per_repo.max(1);
    // (repo, may share the repository)
    let mut busy: Vec<(String, bool)> = running
        .iter()
        .map(|m| (m.repo.to_lowercase(), parallel > 1))
        .collect();
    let mut room = max.saturating_sub(running.len());
    let mut out = Vec::new();
    for m in queued {
        if room == 0 {
            break;
        }
        let repo = m.repo.to_lowercase();
        let here: Vec<bool> = busy
            .iter()
            .filter(|(r, _)| *r == repo)
            .map(|(_, p)| *p)
            .collect();
        let safe = parallel > 1;
        let ok = if here.is_empty() {
            true
        } else {
            safe && here.iter().all(|p| *p) && here.len() < parallel
        };
        if !ok {
            continue;
        }
        busy.push((repo, safe));
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
    /// IDE missions handed to a headless agent, as `M-001 -> claude-code`.
    pub reassigned: Vec<String>,
    /// Running missions that went quiet: retried once, or blocked after the retry.
    pub stuck: Vec<String>,
    /// Missions in review whose automatic reviewer was started this pass.
    pub reviewing: Vec<String>,
    /// Approved pull requests merged this pass without a click.
    pub merged: Vec<String>,
    /// Escalations raised this pass (the only questions Studio asks the owner).
    pub escalated: Vec<String>,
    /// Goals whose done checks all passed this pass.
    pub goals_done: Vec<String>,
    /// Fix missions created for failing done checks.
    pub fix_missions: Vec<String>,
    pub kept_awake: bool,
    pub error: Option<String>,
}

/// One scheduler pass. Safe to call often and safe to call when automations are off:
/// with automations off it still advances running missions, it just starts nothing new.
pub fn tick(v: &Vault) -> TickReport {
    // The minute scheduler, the bus watcher and Run now all call this: one pass at a time, or two
    // passes could both start the same queued mission.
    static TICK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = TICK.lock().unwrap_or_else(|p| p.into_inner());
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
        if crate::council::is_council(Some(task)) {
            match crate::council::progress(
                crate::council::state(&m.id).as_deref(),
                chrono::Utc::now().timestamp(),
            ) {
                crate::council::Progress::Wait => {}
                crate::council::Progress::Review => match missions::submit_for_review(&m.id, 0.0) {
                    Ok(_) => {
                        crate::council::clear_state(&m.id);
                        report.moved_to_review.push(m.id.clone())
                    }
                    Err(e) => report.error = Some(e.to_string()),
                },
                crate::council::Progress::Fail(reason) => {
                    crate::council::clear_state(&m.id);
                    if let Err(e) = missions::block(&m.id, &format!("Council: {reason}"), false) {
                        report.error = Some(e.to_string());
                    }
                }
            }
            continue;
        }
        if crate::kanban::is_kanban(Some(task)) {
            match advance_kanban(m, task) {
                Ok(Advance::Review) => report.moved_to_review.push(m.id.clone()),
                Ok(Advance::Stuck(note)) => report.stuck.push(note),
                Ok(Advance::Nothing) => {}
                Err(e) => report.error = Some(e.to_string()),
            }
            continue;
        }
        let state = v.task_state(task);
        let agent = m.agent.trim().to_lowercase();
        if reassign_due(
            m,
            state == Some(State::Pending),
            crate::bus::headless_available(&agent),
            chrono::Utc::now().timestamp(),
            ide_wait_minutes() * 60,
        ) {
            match reassign(v, m, task) {
                Ok(to) => report.reassigned.push(format!("{} -> {to}", m.id)),
                Err(e) => report.error = Some(e.to_string()),
            }
            continue;
        }
        if state == Some(State::Completed) {
            // The worker's summary names its pull request; keep it before the body moves on.
            // Only the summary: a handoff packet above it can quote other pull requests.
            if let Some(body) = v.task_body(task) {
                if let Some(url) =
                    missions::completion_summary(&body).and_then(|s| missions::pull_url(&s))
                {
                    let _ = missions::set_pr(&m.id, &url);
                }
            }
            match missions::submit_for_review(&m.id, 0.0) {
                Ok(_) => {
                    crate::stuck::reset(&m.id);
                    report.moved_to_review.push(m.id.clone())
                }
                Err(e) => report.error = Some(e.to_string()),
            }
        } else if let Some(note) =
            crate::stuck::check_bus(v, m, task, chrono::Utc::now().timestamp())
        {
            report.stuck.push(note);
        }
    }

    // Re-read: statuses changed in step 1.
    let all = missions::list(300).unwrap_or_default();

    // 1b. Work in review gets its reviewer without a click: a different agent on another
    // provider, with the diff, CI, the verify result and the repository's conventions. Goal
    // missions always; standalone missions when Automations are on.
    let now = chrono::Utc::now().timestamp();
    for m in all.iter().filter(|m| m.status == "review") {
        let eligible = report.enabled || crate::goals::goal_of(&m.id).is_some();
        let state = crate::review_agent::auto_state(&m.id);
        if crate::review_agent::auto_review_due(m, state.as_deref(), now, eligible) {
            crate::review_agent::auto_in_background(v.root.clone(), m.id.clone());
            report.reviewing.push(m.id.clone());
        }
    }

    // 1c. Answers from the phone first, so this pass applies them.
    let _ = crate::digest::poll_phone_replies();
    // Merge what the autonomy level allows, raise the escalation list, apply answers.
    let gp = crate::goal_loop::pass(v, report.enabled);
    report.merged = gp.merged;
    report.escalated = gp.escalated;
    // 1d. A goal whose missions are all merged runs its done checks and reports.
    let co = crate::completion::pass(v);
    report.goals_done = co.done;
    report.fix_missions = co.fix_missions;
    report.escalated.extend(co.escalated);
    if let Some(e) = gp.errors.first() {
        report.error.get_or_insert_with(|| e.clone());
    }
    let all = if report.merged.is_empty() && gp.applied.is_empty() && report.fix_missions.is_empty()
    {
        all
    } else {
        missions::list(300).unwrap_or_default()
    };

    // A finished mission no longer needs its worktree; its branch stays, it is the pull request.
    for m in all
        .iter()
        .filter(|m| m.status == "done" || m.status == "failed")
    {
        if crate::worktree::recorded(&m.id).is_some() {
            if let Some(dir) = crate::review_agent::project_dir(&m.repo) {
                let _ = crate::worktree::remove(&dir, &m.id);
            }
        }
    }
    let running: Vec<&Mission> = all.iter().filter(|m| m.status == "running").collect();
    report.running = running.len();

    // 2. Start what can start. Standalone missions need Automations on. A goal's missions run
    // while their goal runs (creating a goal at Ask on risk or Full is the instruction to run it),
    // never at Watch, and only once the missions they depend on are done.
    {
        let now = chrono::Utc::now().timestamp();
        let mut goal_cache: std::collections::HashMap<String, Option<crate::goals::Goal>> =
            std::collections::HashMap::new();
        // A mission the router deferred to a quota reset waits; it does not take a slot.
        let queued: Vec<&Mission> = all
            .iter()
            .filter(|m| m.status == "queued")
            .filter(|m| crate::assign::deferred_until(&m.id).map_or(true, |t| t <= now))
            .filter(|m| !crate::goal_loop::held(&m.id))
            .filter(|m| match crate::goals::goal_of(&m.id) {
                None => report.enabled,
                Some(gid) => {
                    let g = goal_cache
                        .entry(gid.clone())
                        .or_insert_with(|| crate::goals::get(&gid).ok().flatten());
                    g.as_ref().is_some_and(|g| {
                        crate::planner::goal_mission_startable(
                            g,
                            &crate::planner::depends_on(&m.id),
                            &all,
                        )
                    })
                }
            })
            .collect();
        for id in startable(
            &queued,
            &running,
            report.max_concurrent,
            crate::kanban::parallel_per_repo(),
        ) {
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

/// What a pass did with one running Hermes mission.
enum Advance {
    Nothing,
    Review,
    Stuck(String),
}

/// A running Hermes mission: read its kanban task. Finished work is measured (metered spend of
/// every Hermes session it used), checked with `hermes verify`, and sent to Studio's reviewer;
/// kanban's own review pass is never taken as approval. A quiet worker is retried, then blocked.
fn advance_kanban(m: &Mission, task: &str) -> anyhow::Result<Advance> {
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
                return Ok(Advance::Nothing);
            }
            Ok(
                crate::stuck::check_kanban(m, task, &view, chrono::Utc::now().timestamp())
                    .map(Advance::Stuck)
                    .unwrap_or(Advance::Nothing),
            )
        }
        Next::Block(reason) => {
            missions::block(&m.id, &format!("Hermes kanban: {reason}"), false)?;
            Ok(Advance::Nothing)
        }
        Next::Review(summaries) => {
            if let Some(url) = missions::pull_url(&summaries) {
                missions::set_pr(&m.id, &url)?;
            }
            // Verify where the work was done: the task's worktree when it had one.
            let dir = view
                .workspace_path
                .as_deref()
                .map(std::path::PathBuf::from)
                .filter(|d| d.is_dir())
                .or_else(|| crate::review_agent::project_dir(&m.repo).filter(|d| d.is_dir()));
            if let Some(dir) = dir {
                missions::set_verify(&m.id, &kanban::verify(&dir))?;
            }
            missions::submit_for_review(&m.id, spent)?;
            crate::stuck::reset(&m.id);
            Ok(Advance::Review)
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
    std::thread::spawn(move || {
        let mut pass: u64 = 0;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            // An active goal is the owner's instruction to run its missions, so the pass runs
            // for it even with Automations off; the tick itself starts no standalone mission then.
            // The digest goes out at its times whatever else is on.
            let _ = crate::digest::tick(&Vault::new(&vault_root));
            let goals_active = crate::goals::any_active();
            if !enabled() && !goals_active {
                keep_awake(false);
                continue;
            }
            // Every 5 minutes, once the user has created a Notion missions board: pull proposals
            // and cancels before dispatching, so a mission proposed in Notion starts this pass.
            if enabled()
                && pass % 5 == 0
                && crate::notion::setting(crate::notion_sync::KV_DB).is_some()
                && crate::notion::token().is_some()
            {
                let _ = crate::notion_sync::sync(false);
            }
            pass += 1;
            let v = Vault::new(&vault_root);
            let _ = tick(&v);
        }
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
            startable(&queued, &[], 5, 1),
            vec!["M-1"],
            "one per repository, any agent"
        );
    }

    #[test]
    fn worktree_missions_of_any_agent_share_a_repository_up_to_the_cap() {
        let cc = |id: &str, repo: &str, status: &str| {
            let mut x = m(id, repo, status);
            x.agent = "claude-code".into();
            x
        };
        let q = [
            m("M-1", "a", "queued"),
            m("M-2", "a", "queued"),
            m("M-3", "a", "queued"),
        ];
        let queued: Vec<&Mission> = q.iter().collect();
        assert_eq!(
            startable(&queued, &[], 9, 2),
            vec!["M-1", "M-2"],
            "two worktrees in a"
        );
        assert_eq!(
            startable(&queued, &[], 9, 1),
            vec!["M-1"],
            "parallel off: one per repo"
        );
        let busy = [cc("M-9", "a", "running")];
        let r: Vec<&Mission> = busy.iter().collect();
        assert_eq!(
            startable(&queued, &r, 9, 3),
            vec!["M-1", "M-2"],
            "a Claude Code worktree counts toward the per-repo cap"
        );
        assert!(
            startable(&queued, &r, 9, 1).is_empty(),
            "parallel off: nothing beside it"
        );
        let hr = [m("M-8", "a", "running")];
        let r2: Vec<&Mission> = hr.iter().collect();
        assert_eq!(
            startable(&queued, &r2, 2, 3),
            vec!["M-1"],
            "global cap still applies"
        );
    }

    #[test]
    fn an_unclaimed_ide_mission_is_reassigned_only_after_the_wait() {
        let mut x = m("M-1", "a", "running");
        x.agent = "qoder".into();
        x.updated_at = 1_000;
        let wait = 30 * 60;
        assert!(
            !reassign_due(&x, true, false, 1_000 + wait - 1, wait),
            "still in its window"
        );
        assert!(reassign_due(&x, true, false, 1_000 + wait, wait));
        assert!(
            !reassign_due(&x, false, false, 9_999_999, wait),
            "Qoder claimed it"
        );
        assert!(
            !reassign_due(&x, true, true, 9_999_999, wait),
            "a headless agent is never offered over"
        );
        x.status = "review".into();
        assert!(!reassign_due(&x, true, false, 9_999_999, wait));
    }

    #[test]
    fn a_stale_ide_mission_moves_to_a_headless_agent_or_blocks() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let v = Vault::new(d.path().join("vault"));
        v.scaffold().unwrap();
        let mission =
            missions::create("repo", "goal", "acc", "implementer", "qoder", "", "", 0.0).unwrap();
        let task = v
            .delegate_task(crate::vault::DelegateArgs {
                title: "t".into(),
                assigned_to: "qoder".into(),
                creator: "test".into(),
                project: "repo".into(),
                context_files: Vec::new(),
                instructions: "do it".into(),
                priority: "high".into(),
            })
            .unwrap();
        let task_id = task["id"].as_str().unwrap().to_string();
        missions::dispatch(&mission.id, &task_id).unwrap();
        // Offered two hours ago and never picked up.
        let c = crate::store::open().unwrap();
        c.execute(
            "UPDATE missions SET updated_at = updated_at - 7200 WHERE id = ?1",
            [&mission.id],
        )
        .unwrap();

        let report = tick(&v);
        let after = missions::get(&mission.id).unwrap().unwrap();
        match crate::bus::best_headless("qoder", crate::bus::headless_available) {
            Some(to) => {
                assert_eq!(report.reassigned, vec![format!("{} -> {to}", mission.id)]);
                assert_eq!(after.agent, to);
                assert_eq!(after.status, "running");
                let (t, _) = v.get_task(&task_id).unwrap();
                assert_eq!(t.state, State::InProgress);
                assert_eq!(t.claimed_by.as_deref(), Some(to));
                assert!(t.body.contains("reassigned to"), "the note is on the task");
            }
            None => assert_eq!(after.status, "blocked", "never running with no worker"),
        }
        crate::store::set_test_db(None);
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
        let started = startable(&queued, &[], 3, 1);
        assert_eq!(started, vec!["M-1", "M-3", "M-4"]);

        let running = [m("M-9", "b", "running")];
        let r: Vec<&Mission> = running.iter().collect();
        let started = startable(&queued, &r, 2, 1);
        assert_eq!(started, vec!["M-1"], "b is busy and the cap is 2");

        assert!(startable(&queued, &r, 1, 1).is_empty());
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
