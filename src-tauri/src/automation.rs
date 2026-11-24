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
    let Some(to) = crate::bus::best_headless(&from, crate::agent_limits::usable) else {
        missions::block(
            &m.id,
            &format!("{from} could not take this and no other headless agent is free"),
            false,
        )?;
        anyhow::bail!("{}: no headless agent to take over", m.id);
    };
    let why = match crate::agent_limits::blocked_reason_for(&from) {
        Some(r) => format!("{from} {r}"),
        None => format!("{from} is IDE only"),
    };
    let reason = format!(
        "{why}; after {} minutes without progress this was reassigned to {to}",
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

/// What to do with a worker that reported done without a pull request: there is nothing to
/// review. The first time the mission goes back to the queue for a different agent; the second
/// time it is blocked, so its goal asks the owner. Pure.
pub fn no_pr_step(attempts: u32) -> &'static str {
    if attempts == 0 {
        "retry"
    } else {
        "block"
    }
}

pub fn no_pr_key(id: &str) -> String {
    format!("mission.nopr.{id}")
}

fn no_pr_retry(m: &Mission) -> anyhow::Result<String> {
    let attempts: u32 = kv(&no_pr_key(&m.id))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    kv_set(&no_pr_key(&m.id), &(attempts + 1).to_string());
    let from = m.agent.trim().to_lowercase();
    if no_pr_step(attempts) == "block" {
        missions::block(
            &m.id,
            &format!("{from} finished twice without opening a pull request, so there is nothing to review"),
            false,
        )?;
        return Ok(format!(
            "{}: finished again without a pull request; blocked",
            m.id
        ));
    }
    let Some(to) = crate::bus::best_headless(&from, crate::agent_limits::usable) else {
        missions::block(
            &m.id,
            &format!(
                "{from} finished without a pull request and no other usable headless agent can retry this"
            ),
            false,
        )?;
        return Ok(format!(
            "{}: finished without a pull request; no other usable headless agent",
            m.id
        ));
    };
    missions::block(
        &m.id,
        &format!("{from} finished without a pull request"),
        false,
    )?;
    missions::set_agent(&m.id, to)?;
    missions::unblock(
        &m.id,
        &format!("{from} reported done without opening a pull request; retrying with {to}"),
    )?;
    Ok(format!(
        "{}: {from} opened no pull request; queued again for {to}",
        m.id
    ))
}

/// Automatic fresh attempts for a mission whose Hermes worker stopped because of its model, not
/// its work.
pub const MODEL_RETRIES: u32 = 2;

pub fn model_retry_key(id: &str) -> String {
    format!("mission.modelfail.{id}")
}

/// A Hermes worker that stopped because its model or its runtime failed: the plan's usage or rate
/// limit, a fallback provider out of credits or entitlement, a fallback model that stopped
/// replying or sent a broken tool call, or a managed worker the gateway lost. The work itself is
/// not at fault, so it deserves a fresh attempt rather than the owner's attention. Pure.
pub fn model_failure(reason: &str) -> bool {
    let r = reason.to_lowercase();
    [
        "no reply:",
        "didn't produce a reply",
        "did not produce a reply",
        "improperly formatted function call",
        "usage_limit_reached",
        "hit your 5-hour limit",
        "rate limit",
        "http 429",
        "quota reached",
        "entitlement is exhausted",
        "billing, credits",
        "insufficient credits",
        "managed worker lost",
        "gateway connection failed",
        "without kanban_complete",
        "broken pipe",
        "brokenpipeerror",
        "connection reset",
        "provider unavailable",
        "bad gateway",
        "service unavailable",
        "overloaded",
        "http 502",
        "http 503",
        "http 504",
    ]
    .iter()
    .any(|p| r.contains(p))
}

/// What to do with the next model failure, given the fresh attempts already spent. Pure.
pub fn model_failure_step(attempts: u32) -> &'static str {
    if attempts < MODEL_RETRIES {
        "retry"
    } else {
        "block"
    }
}

/// Queue the mission again for a fresh Hermes attempt (a new kanban task, which starts on the
/// primary model again), at most `MODEL_RETRIES` times; after that it is blocked with the reason.
fn model_failure_retry(m: &Mission, reason: &str) -> anyhow::Result<String> {
    let attempts: u32 = kv(&model_retry_key(&m.id))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let short: String = reason.chars().take(300).collect();
    if model_failure_step(attempts) == "block" {
        if m.status != "blocked" {
            missions::block(
                &m.id,
                &format!(
                    "The model failed {} times in a row, so this needs a look: {short}",
                    attempts + 1
                ),
                false,
            )?;
        }
        return Ok(format!(
            "{}: the model failed again after {attempts} fresh attempts; blocked",
            m.id
        ));
    }
    kv_set(&model_retry_key(&m.id), &(attempts + 1).to_string());
    if m.status != "blocked" {
        missions::block(&m.id, &format!("Hermes kanban: {short}"), false)?;
    }
    missions::unblock(
        &m.id,
        &format!(
            "The model stopped, not the work (attempt {} of {MODEL_RETRIES}); queued again: {short}",
            attempts + 1
        ),
    )?;
    crate::stuck::reset(&m.id);
    let _ = crate::provider_health::auto_heal_if_due();
    Ok(format!(
        "{}: the model stopped replying; queued again for a fresh attempt",
        m.id
    ))
}

/// The scheduler's answer to a fleet doctor recovery request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    Retried(String),
    Held(String),
}

/// Requeue a blocked mission whose recorded failure was its model or provider, not its work.
/// Scheduler policy still decides: a paused fleet, automations off, a spent metered budget, a
/// held goal, a lane deferral or a spent retry budget all leave the mission blocked. Routes and
/// provider settings are never changed here.
pub fn recover_blocked(m: &Mission, reason: &str) -> anyhow::Result<Recovery> {
    if crate::pause::paused() {
        return Ok(Recovery::Held("the fleet is paused".into()));
    }
    if m.status != "blocked" {
        return Ok(Recovery::Held(format!(
            "the mission is {}, not blocked",
            m.status
        )));
    }
    if !model_failure(reason) {
        return Ok(Recovery::Held(
            "the recorded failure is not a model or provider failure".into(),
        ));
    }
    if !enabled() {
        return Ok(Recovery::Held("automations are off".into()));
    }
    if crate::budget::status().exhausted {
        return Ok(Recovery::Held("the global metered budget is spent".into()));
    }
    if over_budget(m, 0.0) {
        return Ok(Recovery::Held(format!(
            "the mission passed its ${:.2} budget",
            m.budget_usd
        )));
    }
    if crate::goal_loop::held(&m.id) {
        return Ok(Recovery::Held("the goal holds this mission".into()));
    }
    let now = chrono::Utc::now().timestamp();
    if crate::assign::deferred_until(&m.id).is_some_and(|t| t > now) {
        return Ok(Recovery::Held(
            "the router deferred it to a lane reset".into(),
        ));
    }
    let attempts: u32 = kv(&model_retry_key(&m.id))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if model_failure_step(attempts) == "block" {
        return Ok(Recovery::Held(format!(
            "its {MODEL_RETRIES} fresh model attempts are spent"
        )));
    }
    model_failure_retry(m, reason).map(Recovery::Retried)
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

/// Queued missions whose goal is failed or done can never start, because startable requires the
/// goal to be running. Stop them with the goal so they do not sit in the queue for ever, holding
/// a repository slot in the owner's head. Returns the ids stopped.
fn stop_orphaned_queued(all: &[Mission]) -> Vec<String> {
    let mut stopped = Vec::new();
    for m in all.iter().filter(|m| m.status == "queued") {
        let Some(gid) = crate::goals::goal_of(&m.id) else {
            continue;
        };
        let Ok(Some(g)) = crate::goals::get(&gid) else {
            continue;
        };
        if !matches!(g.status.as_str(), "failed" | "done") {
            continue;
        }
        if missions::cancel(&m.id, &format!("its goal {gid} is {}", g.status)).is_ok() {
            stopped.push(m.id.clone());
        }
    }
    stopped
}

/// Queued missions in the order they last changed, oldest first. A repository has one slot, and
/// missions::list hands queued rows back newest first; without this reorder a steady stream of new
/// work in one repository starves the mission that has waited longest. Pure.
fn dispatch_order(queued: Vec<&Mission>) -> Vec<&Mission> {
    let mut q = queued;
    q.sort_by_key(|m| (m.updated_at, m.id.clone()));
    q
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

/// Failed starts before one report row is written. Two attempts are a retry.
const START_FAIL_QUIET: u32 = 3;

fn start_fail_key(id: &str) -> String {
    format!("start.fail.{id}")
}

/// Count a failed start so a queued mission that can never start shows up in the report feed.
/// A router deferral is a wait, not a failure. The third failed start writes one row; later
/// attempts stay silent, and a start that works clears the count for the next run.
fn note_start_failure(id: &str, why: &str) {
    if why.contains("deferred until") {
        return;
    }
    let count = kv(&start_fail_key(id))
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0)
        .saturating_add(1);
    kv_set(&start_fail_key(id), &count.to_string());
    if count != START_FAIL_QUIET {
        return;
    }
    let repo = missions::get(id)
        .ok()
        .flatten()
        .map(|m| m.repo)
        .unwrap_or_default();
    let _ = crate::reports::record_here(
        "mission.start_failed",
        &repo,
        &format!("{id} could not start: {why}"),
        &format!("The scheduler tried to start {id} {count} times and was refused each time. It stays queued; nothing else changes."),
        id,
    );
}

/// A start that worked clears the failure count; the next failure starts counting again.
fn clear_start_failures(id: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_delete(&c, &start_fail_key(id));
    }
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
    /// The fleet pause switch is on: nothing new starts or merges this pass.
    pub paused: bool,
    /// The network is unreachable and no local model is pinned: nothing new starts this pass, and
    /// silence is not judged stuck.
    pub offline: bool,
    /// The global metered budget is spent: metered missions neither start nor keep running.
    pub budget_stopped: bool,
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
        paused: crate::pause::paused(),
        ..Default::default()
    };
    let all = match missions::list(300) {
        Ok(m) => m,
        Err(e) => {
            report.error = Some(e.to_string());
            return report;
        }
    };
    // The global metered budget: once it is spent, metered work stops and subscriptions go on.
    let budget = crate::budget::status();
    report.budget_stopped = budget.exhausted;
    let hermes_provider = if budget.exhausted {
        crate::agents::hermes_provider()
    } else {
        String::new()
    };
    let metered_stop =
        |m: &Mission| budget.exhausted && crate::budget::would_meter(m, &hermes_provider);

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
            match advance_kanban(m, task, metered_stop(m)) {
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
            crate::agent_limits::usable(&agent),
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
            let has_pr = missions::get(&m.id)
                .ok()
                .flatten()
                .and_then(|x| x.pr_url)
                .is_some();
            if !has_pr {
                match no_pr_retry(m) {
                    Ok(note) => report.stuck.push(note),
                    Err(e) => report.error = Some(e.to_string()),
                }
                continue;
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
        // A review needs a pull request: without one the reviewer refuses, and the mission would
        // sit in review for ever. Send it back the same way as a worker that opened none.
        if m.pr_url.is_none() && !crate::council::is_council(m.task_id.as_deref()) {
            match no_pr_retry(m) {
                Ok(note) => report.stuck.push(note),
                Err(e) => report.error = Some(e.to_string()),
            }
            continue;
        }
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
    // Paused: running work still reaches review (step 1) and escalations still expire, but
    // nothing merges, no answer or fix mission is applied and nothing starts.
    let gp = crate::goal_loop::pass(v, report.enabled);
    report.merged = gp.merged;
    report.escalated = gp.escalated;
    // 1d. A goal whose missions are all merged runs its done checks and reports.
    let co = if report.paused {
        crate::completion::Outcome::default()
    } else {
        crate::completion::pass(v)
    };
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

    // 2. Start what can start (never while the fleet is paused). Standalone missions need Automations on. A goal's missions run
    // while their goal runs (creating a goal at Ask on risk or Full is the instruction to run it),
    // never at Watch, and only once the missions they depend on are done.
    {
        let now = chrono::Utc::now().timestamp();
        // A mission whose goal was stopped can never start again; stop it with the goal.
        stop_orphaned_queued(&all);
        let mut goal_cache: std::collections::HashMap<String, Option<crate::goals::Goal>> =
            std::collections::HashMap::new();
        // A mission the router deferred to a quota reset waits; it does not take a slot.
        let queued: Vec<&Mission> = all
            .iter()
            .filter(|m| m.status == "queued")
            .filter(|m| crate::assign::deferred_until(&m.id).map_or(true, |t| t <= now))
            .filter(|m| !crate::goal_loop::held(&m.id))
            .filter(|m| !metered_stop(m))
            // Beyond B6: one heavy Studio build at a time on this machine.
            .filter(|m| {
                !crate::self_build::is_self(&m.repo)
                    || crate::self_build::may_start(m, &running, crate::self_build::cargo_busy())
            })
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
        // No network and no local pin: nothing new starts (every agent needs a provider); work
        // resumes when the network is back, or starts now on a pinned local model.
        let offline = !queued.is_empty() && !crate::offline::network_usable();
        report.offline = offline;
        let queued: Vec<&Mission> = if report.paused || offline {
            Vec::new()
        } else {
            queued
        };
        for id in startable(
            &dispatch_order(queued),
            &running,
            report.max_concurrent,
            crate::kanban::parallel_per_repo(),
        ) {
            match missions::start(v, &id) {
                Ok(_) => {
                    clear_start_failures(&id);
                    report.dispatched.push(id);
                }
                Err(e) => {
                    note_start_failure(&id, &e.to_string());
                    report.error = Some(e.to_string());
                }
            }
        }
    }

    // 3. Hold the machine awake only while there is work in flight.
    report.running = missions::list(300)
        .map(|l| l.iter().filter(|m| m.status == "running").count())
        .unwrap_or(report.running);
    keep_awake(report.running > 0);
    report.kept_awake = report.running > 0;
    // 4. Once a night, rewrite every repository's playbook from what the fleet learned.
    // 5. The time machine: measure new models on real work, one replay at a time.
    if !cfg!(test) {
        let _ = crate::bench::step(v, chrono::Utc::now().timestamp());
        // 6. Canary probes (S1): a tiny known question per flat-rate lane, in the background.
        crate::canary::step();
        crate::fleet_doctor::step();
        crate::fallback_chain::step();
        crate::fleet_report::step();
        if let Some(changed) = crate::playbook::nightly(v) {
            if !changed.is_empty() {
                let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
                    kind: "note",
                    from_agent: "agentos-studio",
                    summary: &format!("Playbooks rewritten: {}", changed.join(", ")),
                    ..Default::default()
                });
            }
        }
    }
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
fn advance_kanban(m: &Mission, task: &str, global_stop: bool) -> anyhow::Result<Advance> {
    use crate::kanban::{self, Next};
    let view = kanban::show(task)?;
    let spent = crate::usage::metered_cost(&kanban::session_ids(&view));
    match kanban::next_at(&view, crate::store::now()) {
        Next::Wait => {
            if global_stop {
                // Stop the Hermes worker too, not only Studio's record of it.
                if let Err(e) = kanban::halt(task, crate::budget::STOP_REASON) {
                    eprintln!("budget: could not halt {task}: {e:#}");
                }
                missions::block(&m.id, crate::budget::STOP_REASON, false)?;
                return Ok(Advance::Nothing);
            }
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
            if model_failure(&reason) {
                if let Some(model) = crate::quota_grid::note_failure(&reason, store::now()) {
                    let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
                        mission_id: Some(&m.id),
                        kind: "note",
                        from_agent: "quota grid",
                        summary: &format!(
                            "{model} ran out of credits or entitlement; new workers avoid it for {} h",
                            crate::quota_grid::EXHAUSTED_SECS / 3600
                        ),
                        ..Default::default()
                    });
                }
                return model_failure_retry(m, &reason).map(Advance::Stuck);
            }
            missions::block(&m.id, &format!("Hermes kanban: {reason}"), false)?;
            Ok(Advance::Nothing)
        }
        Next::Review(summaries) => {
            if let Some(url) = missions::pull_url(&summaries) {
                missions::set_pr(&m.id, &url)?;
            }
            // Verify where the work was done: the task's worktree, else the pull request's head
            // in Studio's own worktree. Never the owner's checkout (#117).
            let pr = missions::pull_url(&summaries)
                .or_else(|| m.pr_url.clone())
                .as_deref()
                .and_then(kanban::pr_number_in);
            let result = kanban::verify_mission(&m.id, &m.repo, view.workspace_path.as_deref(), pr);
            missions::set_verify(&m.id, &result)?;
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
        let mut goal_pass: u64 = 0;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            // Every 5 minutes, once the user has created a Notion goals board: pull goal proposals
            // and Decision answers, then publish progress. A goal is the owner's instruction, so
            // this runs with Automations off too; the goal loop below applies a Decision.
            if goal_pass % 5 == 0
                && crate::notion::setting(crate::notion_goals::KV_DB).is_some()
                && crate::notion::token().is_some()
            {
                let _ = crate::notion_goals::sync(&Vault::new(&vault_root), false);
            }
            goal_pass += 1;
            // An active goal is the owner's instruction to run its missions, so the pass runs
            // for it even with Automations off; the tick itself starts no standalone mission then.
            // The digest goes out at its times whatever else is on.
            let _ = crate::digest::tick(&Vault::new(&vault_root));
            let _ = crate::self_build::maybe_send_weekly_ship(crate::store::now());
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
            let _ = crate::orchestrator::pass();
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
    fn a_queued_mission_whose_goal_was_stopped_is_stopped_with_it() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let goal = crate::goals::create(&crate::goals::NewGoal {
            outcome: "Finish".into(),
            repos: vec!["demo".into()],
            checks: vec![crate::goals::Check {
                kind: "file".into(),
                target: "README.md".into(),
                repo: None,
            }],
            budget_usd: 0.0,
            deadline: None,
            autonomy: None,
        })
        .unwrap();
        crate::goals::set_status(&goal.id, &["planning"], "failed", "stopped").unwrap();
        let m = missions::create("demo", "work", "a", "implementer", "auto", "", "", 0.0).unwrap();
        crate::goals::attach(&goal.id, &m.id).unwrap();
        assert_eq!(m.status, "queued");
        let stopped = stop_orphaned_queued(&missions::list(10).unwrap());
        assert_eq!(stopped, vec![m.id.clone()], "the orphan is named");
        assert_eq!(
            missions::get(&m.id).unwrap().unwrap().status,
            "failed",
            "a mission whose goal is stopped can never start, so it must not sit queued"
        );
        crate::store::set_test_db(None);
    }

    #[test]
    fn the_oldest_queued_mission_gets_its_repository_slot_first() {
        let mut old = m("M-1", "a", "queued");
        old.updated_at = 100;
        let mut new = m("M-2", "a", "queued");
        new.updated_at = 900;
        let order = dispatch_order(vec![&new, &old]);
        assert_eq!(order[0].id, "M-1", "the wait is first in line");
        assert_eq!(
            startable(&order, &[], 3, 1),
            vec!["M-1"],
            "the repository's one slot goes to the mission that has waited longest"
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
        match crate::bus::best_headless("qoder", crate::agent_limits::usable) {
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
    fn a_queued_mission_that_cannot_start_reaches_the_report_feed_once() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let m = missions::create("demo", "g", "a", "implementer", "auto", "", "", 0.0).unwrap();
        let why = "no installed headless agent can take this mission";
        note_start_failure(&m.id, why);
        note_start_failure(&m.id, why);
        assert!(
            crate::reports::list(0, "", 10).unwrap().is_empty(),
            "two failures are still a retry"
        );
        note_start_failure(&m.id, why);
        let rows = crate::reports::list(0, "", 10).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].kind, "mission.start_failed");
        assert!(rows[0].title.contains(&m.id));
        note_start_failure(&m.id, why);
        assert_eq!(
            crate::reports::list(0, "", 10).unwrap().len(),
            1,
            "the repeat row is throttled"
        );
        note_start_failure(&m.id, "M-900: deferred until 1900000000: no quota");
        assert_eq!(
            crate::reports::list(0, "", 10).unwrap().len(),
            1,
            "a router deferral is a wait, not a failure"
        );
        clear_start_failures(&m.id);
        let c = crate::store::open().unwrap();
        assert!(crate::store::kv_get(&c, &start_fail_key(&m.id))
            .unwrap()
            .is_none());
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
    fn work_without_a_pull_request_is_retried_once_then_blocked() {
        assert_eq!(no_pr_step(0), "retry");
        assert_eq!(no_pr_step(1), "block");
        assert_eq!(no_pr_step(5), "block");
        assert_eq!(no_pr_key("M-7"), "mission.nopr.M-7");
    }

    #[test]
    fn a_retry_with_no_usable_agent_blocks_instead_of_forcing_hermes() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let m = missions::create(
            "repo",
            "goal",
            "acc",
            "implementer",
            "claude-code",
            "",
            "",
            0.0,
        )
        .unwrap();
        crate::agent_limits::mark_disabled("antigravity", 3600, "access revoked");
        crate::agent_limits::mark_disabled("hermes", 3600, "access revoked");
        let out = no_pr_retry(&m).unwrap();
        let after = missions::get(&m.id).unwrap().unwrap();
        assert_eq!(after.status, "blocked", "{out}");
        assert_eq!(after.agent, "claude-code", "no forced fallback agent");
        assert!(
            after
                .review_note
                .as_deref()
                .is_some_and(|n| n.contains("no other usable headless agent")),
            "{:?}",
            after.review_note
        );
        crate::agent_limits::clear_disabled("antigravity");
        crate::agent_limits::clear_disabled("hermes");
        crate::store::set_test_db(None);
    }

    #[test]
    fn a_model_that_stopped_replying_is_recognised_and_not_the_work() {
        // The real M-062 block: Claude's 5-hour limit, then the Gemini fallbacks gave no reply.
        let real = "worker exited cleanly (rc=0) without kanban_complete, kanban_block or kanban_request_review. Worker's last output: \"No reply: gemini-4-hydrogen-high didn't produce a reply this time, even after retries.\"";
        assert!(model_failure(real));
        assert!(model_failure(
            "Antigravity model error: Your previous response contained an improperly formatted function call"
        ));
        assert!(model_failure("You've hit your 5-hour limit, resets 3:50pm"));
        assert!(model_failure("Individual quota reached. Resets in 53h"));
        assert!(!model_failure("waiting for approval of rm -rf build"));
        assert!(!model_failure("npm test failed: 3 tests failed"));
        assert_eq!(model_failure_step(0), "retry");
        assert_eq!(model_failure_step(1), "retry");
        assert_eq!(model_failure_step(2), "block");
        assert_eq!(model_retry_key("M-62"), "mission.modelfail.M-62");
    }

    #[test]
    fn a_fallback_out_of_entitlement_or_a_lost_worker_is_not_the_work() {
        // The real M-096 block (E-16): Claude's limit, a fallback out of entitlement, the worker lost.
        let real = "pid 26144 exited with code 1 [managed worker lost: EOFError] Worker's last output: 'billing, credits, or account entitlement is exhausted for gemini-4-hydrogen-high. Add credits or update billing with that provider, then retry.' Kanban owner refused: Kanban owner silent for 230s: Gateway connection failed; no local fallback";
        assert!(model_failure(real));
        assert!(model_failure("managed worker lost: EOFError"));
        assert!(model_failure(
            "Gateway connection failed; no local fallback"
        ));
        assert!(model_failure("402: insufficient credits for this request"));
        assert!(!model_failure("cargo test: 2 failed; see output"));
        assert!(!model_failure(
            "Blocked on PR #16: still open and now conflicts with main"
        ));
    }

    #[test]
    fn a_model_failure_queues_the_mission_again_twice_then_blocks_it() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
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
        let reason = "No reply: gemini-4-hydrogen-high didn't produce a reply this time";
        for attempt in 1..=MODEL_RETRIES {
            missions::dispatch(&mission.id, "kanban:t_test").unwrap();
            model_failure_retry(&mission, reason).unwrap();
            let after = missions::get(&mission.id).unwrap().unwrap();
            assert_eq!(after.status, "queued", "attempt {attempt} is queued again");
            assert!(after
                .review_note
                .as_deref()
                .unwrap_or("")
                .contains("The model stopped, not the work"));
        }
        missions::dispatch(&mission.id, "kanban:t_test").unwrap();
        model_failure_retry(&mission, reason).unwrap();
        let after = missions::get(&mission.id).unwrap().unwrap();
        assert_eq!(after.status, "blocked", "the third failure asks the owner");
        assert!(after
            .review_note
            .as_deref()
            .unwrap_or("")
            .contains("The model failed 3 times in a row"));
        crate::store::set_test_db(None);
    }

    #[test]
    fn recovery_queues_a_blocked_model_failure_within_the_retry_budget() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        set_enabled(true);
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
        missions::dispatch(&mission.id, "kanban:t_test").unwrap();
        missions::block(&mission.id, "worker crashed", false).unwrap();
        let reason = "No reply: gemini-4-hydrogen-high didn't produce a reply this time";

        let fresh = missions::get(&mission.id).unwrap().unwrap();
        let outcome = recover_blocked(&fresh, reason).unwrap();
        assert!(matches!(outcome, Recovery::Retried(_)), "{outcome:?}");
        assert_eq!(
            missions::get(&mission.id).unwrap().unwrap().status,
            "queued",
            "the scheduler queued the mission again"
        );
        assert_eq!(
            kv(&model_retry_key(&mission.id)).as_deref(),
            Some("1"),
            "the retry budget was spent"
        );

        missions::dispatch(&mission.id, "kanban:t_test2").unwrap();
        missions::block(&mission.id, "worker crashed again", false).unwrap();
        kv_set(&model_retry_key(&mission.id), &MODEL_RETRIES.to_string());
        let spent = recover_blocked(&missions::get(&mission.id).unwrap().unwrap(), reason).unwrap();
        assert!(
            matches!(spent, Recovery::Held(ref why) if why.contains("attempts are spent")),
            "{spent:?}"
        );
        assert_eq!(
            missions::get(&mission.id).unwrap().unwrap().status,
            "blocked",
            "a spent retry budget leaves the mission blocked"
        );
        crate::store::set_test_db(None);
    }

    #[test]
    fn recovery_holds_anything_the_scheduler_would_not_start() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        set_enabled(true);
        let reason = "No reply: gemini-4-hydrogen-high didn't produce a reply this time";
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

        let queued = recover_blocked(&mission, reason).unwrap();
        assert!(
            matches!(queued, Recovery::Held(ref why) if why.contains("not blocked")),
            "{queued:?}"
        );

        missions::dispatch(&mission.id, "kanban:t_test").unwrap();
        missions::block(&mission.id, "worker crashed", false).unwrap();
        let not_model = recover_blocked(
            &missions::get(&mission.id).unwrap().unwrap(),
            "cargo test failed: 3 tests failed",
        )
        .unwrap();
        assert!(
            matches!(not_model, Recovery::Held(ref why) if why.contains("not a model or provider failure")),
            "{not_model:?}"
        );

        set_enabled(false);
        let off = recover_blocked(&missions::get(&mission.id).unwrap().unwrap(), reason).unwrap();
        assert!(
            matches!(off, Recovery::Held(ref why) if why.contains("automations are off")),
            "{off:?}"
        );
        set_enabled(true);

        crate::pause::pause("test hold").unwrap();
        let paused =
            recover_blocked(&missions::get(&mission.id).unwrap().unwrap(), reason).unwrap();
        assert!(
            matches!(paused, Recovery::Held(ref why) if why.contains("paused")),
            "{paused:?}"
        );
        crate::pause::resume().unwrap();
        assert_eq!(
            missions::get(&mission.id).unwrap().unwrap().status,
            "blocked",
            "a held recovery never changes the board"
        );
        crate::store::set_test_db(None);
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
        v.complete_task(
            &task_id,
            "hermes",
            "Opened https://github.com/konethegreat/repo/pull/3",
        )
        .unwrap();

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

    #[test]
    fn done_without_a_pull_request_goes_back_to_the_queue_not_to_review() {
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
        v.complete_task(&task_id, "hermes", "I could not run gh, so I stopped")
            .unwrap();
        let report = tick(&v);
        assert!(report.moved_to_review.is_empty(), "nothing to review");
        let after = missions::get(&mission.id).unwrap().unwrap();
        assert_eq!(after.status, "queued", "queued again for another agent");
        assert_eq!(kv(&no_pr_key(&mission.id)).as_deref(), Some("1"));
        missions::delete(&mission.id).unwrap();
        assert!(
            kv(&no_pr_key(&mission.id)).is_none(),
            "the marker goes with the row"
        );
        crate::store::set_test_db(None);
    }
}
