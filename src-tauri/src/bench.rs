//! The time machine (Beyond B4): every approved mission is a benchmark, and a new model is
//! measured on real work the week it appears.
//!
//! - **Benchmark set.** Approved missions with a pull request, newest first per repository,
//!   taken round-robin across repositories so no single repository decides a model's score.
//!   Studio's own repository is left out: its tests are a full Rust build, too heavy to replay
//!   on this machine alongside the fleet.
//! - **New model day.** When `model_watch` sees a new model on a flat subscription (the Claude
//!   subscription, Antigravity), the first `CASES_PER_MODEL` benchmark missions are queued for a
//!   replay on it. Metered providers are never queued: a replay there would spend money nobody
//!   approved.
//! - **Runner.** One replay at a time, at most `PER_DAY` a day, only while the fleet is not
//!   paused and the model's provider has headroom. Each replay is `eval::run_with`: a detached
//!   sandbox at the pull request's base commit that never pushes, scored on the repository's tests
//!   and a second agent's verdict. The eval note it writes is what the router's scorecards read.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::missions::Mission;
use crate::store;
use crate::vault::Vault;

pub const PER_REPO: usize = 3;
pub const MAX_CASES: usize = 12;
pub const CASES_PER_MODEL: usize = 3;
pub const PER_DAY: u32 = 6;
/// Repositories whose tests are too heavy to replay here.
pub const SKIP_REPOS: [&str; 1] = ["agentos-studio"];

const QUEUE_KEY: &str = "bench.queue";
const DAY_KEY: &str = "bench.day";
const PLANNED_KEY: &str = "bench.planned_at";
const PLAN_EVERY_SECS: i64 = 3600;

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Providers a replay may run on: flat subscriptions only. Pure.
pub fn flat_provider(provider: &str) -> bool {
    let p = provider.trim().to_lowercase();
    p.starts_with("claude-subscription") || p == "antigravity-oauth"
}

/// The benchmark set from mission history. Pure.
pub fn benchmark(missions: &[Mission]) -> Vec<&Mission> {
    let mut by: std::collections::BTreeMap<String, Vec<&Mission>> = Default::default();
    for m in missions.iter().filter(|m| {
        m.status == "done"
            && m.review_verdict.as_deref() == Some("approve")
            && m.pr_url.as_deref().is_some_and(|u| !u.trim().is_empty())
            && !SKIP_REPOS.iter().any(|r| m.repo.eq_ignore_ascii_case(r))
    }) {
        by.entry(m.repo.to_lowercase()).or_default().push(m);
    }
    for v in by.values_mut() {
        v.sort_by_key(|m| (std::cmp::Reverse(m.updated_at), m.id.clone()));
        v.truncate(PER_REPO);
    }
    let mut out = Vec::new();
    for round in 0..PER_REPO {
        for v in by.values() {
            if let Some(m) = v.get(round) {
                out.push(*m);
            }
        }
    }
    out.truncate(MAX_CASES);
    out
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub mission: String,
    pub provider: String,
    pub model: String,
    pub reason: String,
    pub queued_at: i64,
}

pub fn done_key(mission: &str, provider: &str, model: &str) -> String {
    format!("bench.done.{mission}.{provider}/{model}")
}

/// New models worth measuring: on a flat provider, and not a context-size variant of another
/// new model (`claude-fable-5-2[1m]` beside `claude-fable-5-2`). Pure.
pub fn worth_measuring(new: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = new
        .iter()
        .filter(|(p, _)| flat_provider(p))
        .filter(|(p, m)| match m.split_once('[') {
            Some((base, _)) => !new.iter().any(|(q, n)| q == p && n == base),
            None => true,
        })
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Jobs for new models: the first `CASES_PER_MODEL` cases each, skipping replays already done or
/// queued. Pure.
pub fn plan(
    models: &[(String, String)],
    cases: &[&Mission],
    queued: &[Job],
    done: &dyn Fn(&str, &str, &str) -> bool,
    now: i64,
) -> Vec<Job> {
    let mut out = Vec::new();
    for (provider, model) in models {
        for m in cases.iter().take(CASES_PER_MODEL) {
            let dup = queued
                .iter()
                .chain(out.iter())
                .any(|j| j.mission == m.id && j.provider == *provider && j.model == *model);
            if dup || done(&m.id, provider, model) {
                continue;
            }
            out.push(Job {
                mission: m.id.clone(),
                provider: provider.clone(),
                model: model.clone(),
                reason: format!("new model {model} on {provider}"),
                queued_at: now,
            });
        }
    }
    out
}

/// May the runner start another replay today? `day` is kv `bench.day`, `<date>:<count>`. Pure.
pub fn under_daily_cap(day: Option<&str>, today: &str) -> bool {
    match day.and_then(|d| d.split_once(':')) {
        Some((d, n)) if d == today => n.parse::<u32>().unwrap_or(PER_DAY) < PER_DAY,
        _ => true,
    }
}

fn kv(key: &str) -> Option<String> {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, key).ok().flatten())
}

fn kv_set(key: &str, value: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, key, value);
    }
}

pub fn queue() -> Vec<Job> {
    kv(QUEUE_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_queue(q: &[Job]) {
    if let Ok(s) = serde_json::to_string(q) {
        kv_set(QUEUE_KEY, &s);
    }
}

fn is_done(mission: &str, provider: &str, model: &str) -> bool {
    kv(&done_key(mission, provider, model)).is_some()
}

/// Queue a replay of the benchmark set on one model (the owner's "Measure this model").
pub fn enqueue_model(provider: &str, model: &str, reason: &str) -> Result<Vec<Job>> {
    if !flat_provider(provider) {
        bail!("{provider} is metered; replays run only on flat subscriptions");
    }
    if model.trim().is_empty() {
        bail!("name a model to measure");
    }
    let all = crate::missions::list(1000)?;
    let cases = benchmark(&all);
    if cases.is_empty() {
        bail!("no approved mission with a pull request to replay yet");
    }
    let mut q = queue();
    let mut add = plan(
        &[(provider.trim().to_string(), model.trim().to_string())],
        &cases,
        &q,
        &is_done,
        store::now(),
    );
    for j in &mut add {
        j.reason = reason.to_string();
    }
    q.extend(add.clone());
    save_queue(&q);
    Ok(add)
}

/// Hourly: queue the benchmark for every new model on a flat subscription.
fn plan_new_models(now: i64) -> Vec<Job> {
    if kv(PLANNED_KEY)
        .and_then(|s| s.parse::<i64>().ok())
        .is_some_and(|t| now - t < PLAN_EVERY_SECS)
    {
        return Vec::new();
    }
    kv_set(PLANNED_KEY, &now.to_string());
    let Ok(changes) = crate::model_watch::check() else {
        return Vec::new();
    };
    let new: Vec<(String, String)> = changes
        .new
        .iter()
        .map(|n| (n.provider.clone(), n.model.clone()))
        .collect();
    let models = worth_measuring(&new);
    if models.is_empty() {
        return Vec::new();
    }
    let all = crate::missions::list(1000).unwrap_or_default();
    let cases = benchmark(&all);
    let mut q = queue();
    let add = plan(&models, &cases, &q, &is_done, now);
    q.extend(add.clone());
    save_queue(&q);
    add
}

/// One scheduler step: plan new-model replays hourly, then start the next replay when allowed.
/// Returns a line for the pass log when it queued or started something.
pub fn step(v: &Vault, now: i64) -> Vec<String> {
    let mut log = Vec::new();
    if crate::pause::paused() {
        return log;
    }
    let added = plan_new_models(now);
    if !added.is_empty() {
        let line = format!(
            "Time machine: queued {} replays for {}",
            added.len(),
            added
                .iter()
                .map(|j| j.model.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ")
        );
        let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
            kind: "note",
            from_agent: "time machine",
            summary: &line,
            ..Default::default()
        });
        log.push(line);
    }
    if RUNNING.load(Ordering::SeqCst) {
        return log;
    }
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let day = kv(DAY_KEY);
    if !under_daily_cap(day.as_deref(), &today) {
        return log;
    }
    let mut q = queue();
    let Some(i) = q
        .iter()
        .position(|j| crate::agents::has_headroom(&crate::agents::quota_for(&j.provider, false)))
    else {
        return log;
    };
    let job = q.remove(i);
    save_queue(&q);
    let n = match day.as_deref().and_then(|d| d.split_once(':')) {
        Some((d, n)) if d == today => n.parse::<u32>().unwrap_or(0) + 1,
        _ => 1,
    };
    kv_set(DAY_KEY, &format!("{today}:{n}"));
    RUNNING.store(true, Ordering::SeqCst);
    log.push(format!(
        "Time machine: replaying {} on {} ({})",
        job.mission, job.model, job.reason
    ));
    let root = v.root.clone();
    std::thread::spawn(move || {
        let v = Vault::new(&root);
        let r = crate::eval::run_with(
            &v,
            &job.mission,
            "hermes",
            Some(&job.model),
            Some(&job.provider),
        );
        let summary = match &r {
            Ok(e) => format!(
                "Replayed {} on {}: {} of 100 ({}; the original scored {})",
                job.mission, job.model, e.score, e.comparison, e.original_score
            ),
            Err(err) => format!(
                "Replay of {} on {} failed: {}",
                job.mission,
                job.model,
                err.to_string().chars().take(300).collect::<String>()
            ),
        };
        kv_set(
            &done_key(&job.mission, &job.provider, &job.model),
            &store::now().to_string(),
        );
        let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
            mission_id: Some(&job.mission),
            kind: "note",
            from_agent: "time machine",
            summary: &summary,
            ..Default::default()
        });
        RUNNING.store(false, Ordering::SeqCst);
    });
    log
}

#[derive(Debug, Clone, Serialize)]
pub struct Case {
    pub mission: String,
    pub repo: String,
    pub goal: String,
    pub pr_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub cases: Vec<Case>,
    pub queue: Vec<Job>,
    pub running: bool,
    pub today: u32,
    pub per_day: u32,
}

pub fn status() -> Result<Status> {
    let all = crate::missions::list(1000)?;
    let cases = benchmark(&all)
        .into_iter()
        .map(|m| Case {
            mission: m.id.clone(),
            repo: m.repo.clone(),
            goal: m.goal.chars().take(140).collect(),
            pr_url: m.pr_url.clone(),
        })
        .collect();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let count = match kv(DAY_KEY).as_deref().and_then(|d| d.split_once(':')) {
        Some((d, n)) if d == today => n.parse().unwrap_or(0),
        _ => 0,
    };
    Ok(Status {
        cases,
        queue: queue(),
        running: RUNNING.load(Ordering::SeqCst),
        today: count,
        per_day: PER_DAY,
    })
}

/// Empty the queue (replays already running finish).
pub fn clear() -> Result<Status> {
    save_queue(&[]);
    status()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission(id: &str, repo: &str, at: i64, verdict: &str, pr: bool) -> Mission {
        Mission {
            id: id.into(),
            repo: repo.into(),
            goal: format!("goal {id}"),
            status: "done".into(),
            review_verdict: Some(verdict.into()),
            pr_url: pr.then(|| format!("https://github.com/o/{repo}/pull/1")),
            updated_at: at,
            ..Default::default()
        }
    }

    #[test]
    fn only_subscriptions_are_replayed_on() {
        assert!(flat_provider("claude-subscription-directsdk-experimental"));
        assert!(flat_provider("antigravity-oauth"));
        assert!(!flat_provider("openrouter"));
        assert!(!flat_provider("anthropic"), "the API key is metered");
        assert!(!flat_provider("openai-api"));
    }

    #[test]
    fn the_benchmark_takes_approved_work_round_robin_across_repositories() {
        let all = vec![
            mission("M-1", "site", 10, "approve", true),
            mission("M-2", "site", 20, "approve", true),
            mission("M-3", "site", 30, "approve", true),
            mission("M-4", "site", 40, "approve", true),
            mission("M-5", "profile", 15, "approve", true),
            mission("M-6", "profile", 50, "reject", true),
            mission("M-7", "profile", 60, "approve", false),
            mission("M-8", "agentos-studio", 99, "approve", true),
        ];
        let ids: Vec<&str> = benchmark(&all).iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["M-5", "M-4", "M-3", "M-2"],
            "profile then site, newest first"
        );
    }

    #[test]
    fn a_new_model_is_measured_once_without_its_context_variants() {
        let new = vec![
            (
                "claude-subscription-directsdk-experimental".to_string(),
                "claude-fable-5-2".to_string(),
            ),
            (
                "claude-subscription-directsdk-experimental".to_string(),
                "claude-fable-5-2[1m]".to_string(),
            ),
            (
                "openrouter".to_string(),
                "anthropic/claude-fable-5.2".to_string(),
            ),
            (
                "antigravity-oauth".to_string(),
                "gemini-4-hydrogen-high".to_string(),
            ),
        ];
        assert_eq!(
            worth_measuring(&new),
            vec![
                (
                    "antigravity-oauth".to_string(),
                    "gemini-4-hydrogen-high".to_string()
                ),
                (
                    "claude-subscription-directsdk-experimental".to_string(),
                    "claude-fable-5-2".to_string()
                ),
            ]
        );
    }

    #[test]
    fn planning_skips_what_is_queued_or_done() {
        let all: Vec<Mission> = (1..=5)
            .map(|i| mission(&format!("M-{i}"), &format!("r{i}"), i, "approve", true))
            .collect();
        let cases = benchmark(&all);
        let models = vec![("antigravity-oauth".to_string(), "gemini-x".to_string())];
        let queued = vec![Job {
            mission: cases[0].id.clone(),
            provider: "antigravity-oauth".into(),
            model: "gemini-x".into(),
            reason: String::new(),
            queued_at: 0,
        }];
        let done_one = cases[1].id.clone();
        let done = move |m: &str, _: &str, _: &str| m == done_one;
        let jobs = plan(&models, &cases, &queued, &done, 100);
        assert_eq!(
            jobs.len(),
            1,
            "of the first three cases, one queued and one done"
        );
        assert_eq!(jobs[0].mission, cases[2].id);
        assert!(jobs[0].reason.contains("gemini-x"));
    }

    #[test]
    fn at_most_six_replays_a_day() {
        assert!(under_daily_cap(None, "2026-11-16"));
        assert!(under_daily_cap(Some("2026-11-16:5"), "2026-11-16"));
        assert!(!under_daily_cap(Some("2026-11-16:6"), "2026-11-16"));
        assert!(
            under_daily_cap(Some("2026-11-15:6"), "2026-11-16"),
            "a new day"
        );
    }

    #[test]
    fn the_owner_can_queue_a_model_but_never_a_metered_one() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        assert!(enqueue_model("openrouter", "x", "test").is_err());
        assert!(
            enqueue_model("antigravity-oauth", "gemini-x", "test").is_err(),
            "nothing to replay yet"
        );
        assert!(clear().unwrap().queue.is_empty());
        crate::store::set_test_db(None);
    }
}
