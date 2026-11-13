//! Repositories that keep themselves busy (docs/AUTONOMOUS-REPOS.md).
//!
//! An opted-in repository carries its own plan: `docs/agentos/PHASES.md` and
//! `docs/agentos/TASKS.md`, read from the default branch on origin (never from the owner's
//! checkout). Every pass, for each opted-in repository with nothing in flight, Studio starts one
//! goal: write the plan when there is none, finish the first phase that has open tasks, or write
//! the next phase when every task is ticked. The goal machinery does the rest: the planner turns
//! it into missions, the scheduler starts them, the reviewer judges each pull request, the gate
//! merges it, the done check runs, and the goal reports. Agents tick tasks and append to
//! `docs/agentos/REPORT.md` in their own pull requests, so the plan in the repository is always
//! the truth; Studio writes only the vault report.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

use crate::goals::{self, Check, NewGoal};
use crate::store;
use crate::vault::Vault;

pub const KV_ENABLED: &str = "autonomy.enabled";
pub const KV_MAX_ACTIVE: &str = "autonomy.max_active";
pub const PHASES: &str = "docs/agentos/PHASES.md";
pub const TASKS: &str = "docs/agentos/TASKS.md";
pub const REPORT: &str = "docs/agentos/REPORT.md";
/// No more than this many autonomy goals at once across the machine unless the owner raises it.
pub const DEFAULT_MAX_ACTIVE: usize = 3;
/// A repository starts at most one goal in this window, so a failing loop cannot spin.
const MIN_GAP_SECS: i64 = 30 * 60;
/// Writing a new phase happens at most once a day per repository.
const NEXT_PHASE_GAP_SECS: i64 = 24 * 3600;
/// A goal stuck this long (failed or paused) is reported, and the repository waits for the owner.
const LOG_LIMIT: usize = 60;

/// The `autonomy` block of `.agentos/routing.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub enabled: bool,
    /// watch, ask or full (the goal autonomy level). Empty: full when allow_merge is on, else ask.
    #[serde(default)]
    pub level: String,
    /// Metered budget per goal in USD (0 for flat subscriptions).
    #[serde(default)]
    pub budget_usd: f64,
    /// The owner's standing direction for this repository ("keep the design, update the
    /// content"). Added to every goal, so the planner, the workers and the reviewer all see it.
    #[serde(default)]
    pub brief: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Phase {
    pub title: String,
    pub open: Vec<String>,
    pub done: usize,
}

/// One thing that happened for a repository, newest last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub at: i64,
    pub kind: String,
    pub goal: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoState {
    pub goal: Option<String>,
    pub goal_kind: String,
    pub last_start: i64,
    pub last_next_phase: i64,
    pub goals_done: usize,
    pub log: Vec<Event>,
    /// When a failed goal was followed by a fresh one (unix seconds), for the daily cap.
    #[serde(default)]
    pub restarts: Vec<i64>,
}

/// Failed goals restarted per repository per day before Studio stops and waits for the owner.
pub const MAX_RESTARTS_PER_DAY: usize = 2;

/// Restarts in the last 24 hours. Pure.
pub fn recent_restarts(st: &RepoState, now: i64) -> usize {
    st.restarts.iter().filter(|t| now - **t < 24 * 3600).count()
}

/// What the Autonomy screen shows for one repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoStatus {
    pub slug: String,
    pub path: String,
    /// A Hermes project already; Opt in creates one when it is not (missions need it).
    #[serde(default)]
    pub hermes_project: bool,
    pub enabled: bool,
    /// Turned off on this machine only (the committed opt-in stays).
    pub paused_here: bool,
    pub level: String,
    /// The owner's direction for the repository, from the committed opt-in.
    #[serde(default)]
    pub brief: String,
    pub allow_merge: bool,
    pub has_plan: bool,
    pub phases: Vec<Phase>,
    pub goal: Option<String>,
    pub goal_status: Option<String>,
    pub next: String,
    pub log: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Off(String),
    Wait(String),
    /// A goal of this kind ("plan", "phase", "next_phase") with this outcome text.
    Start(String, String),
}

// ---------------------------------------------------------------------------------------------
// Pure parts

/// Phases and tasks from a TASKS.md: `## ` headings are phases, `- [ ]` and `- [x]` are tasks.
/// Tasks above any heading form a phase named "Tasks". Pure.
pub fn parse_tasks(md: &str) -> Vec<Phase> {
    let mut out: Vec<Phase> = Vec::new();
    for l in md.lines() {
        let t = l.trim();
        if let Some(h) = t.strip_prefix("## ") {
            out.push(Phase {
                title: h.trim().to_string(),
                open: vec![],
                done: 0,
            });
            continue;
        }
        let open = t.strip_prefix("- [ ]").map(str::trim);
        let done = t.strip_prefix("- [x]").or_else(|| t.strip_prefix("- [X]"));
        if open.is_none() && done.is_none() {
            continue;
        }
        if out.is_empty() {
            out.push(Phase {
                title: "Tasks".into(),
                open: vec![],
                done: 0,
            });
        }
        let p = out.last_mut().expect("a phase exists");
        match open {
            Some(task) if !task.is_empty() => p.open.push(task.to_string()),
            Some(_) => {}
            None => p.done += 1,
        }
    }
    out.retain(|p| !p.open.is_empty() || p.done > 0);
    out
}

/// The effective level: what the repository asks for, capped by its merge permission. Full
/// without `allow_merge` would ask on every merge anyway, so it reads as ask. Pure.
pub fn level(cfg: &Config, allow_merge: bool) -> String {
    let l = cfg.level.trim();
    let wanted = if goals::LEVELS.contains(&l) {
        l
    } else if allow_merge {
        "full"
    } else {
        "ask"
    };
    if wanted == "full" && !allow_merge {
        "ask".into()
    } else {
        wanted.into()
    }
}

fn bullets(items: &[String]) -> String {
    items
        .iter()
        .map(|i| format!("- {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

const SHARED_RULES: &str = "Rules for every pull request: follow AGENTS.md and the repository's own conventions; one pull request per task, on its own branch, never pushed to main; tests for any code change; the repository's checks pass; no secrets; no em dashes or en dashes anywhere (use hyphens, colons or periods).";

/// The goal text for writing the first plan. Pure.
pub fn plan_outcome(slug: &str) -> String {
    format!(
        "Write the working plan for {slug} so agents always have work. Read the code, README, AGENTS.md, open issues and recent history first. In one pull request add:\n\
         1. {PHASES}: 3 to 6 phases in order, each `## Phase N: <title>` with a short goal and a `Done when:` line. Start from what the project needs next (failing CI, missing tests, open issues, unfinished features, stale docs), not from a rewrite.\n\
         2. {TASKS}: the same `## Phase N: <title>` headings, each with its tasks as `- [ ] <task>` checkboxes, in order, each small enough for one pull request and concrete enough to verify.\n\
         3. {REPORT}: a `# Report` heading; every later pull request appends one line to it.\n\n{SHARED_RULES}"
    )
}

/// The goal text for one phase: its open tasks, in order, one pull request each. Pure.
pub fn phase_outcome(slug: &str, p: &Phase, checks: &[String]) -> String {
    let checks = if checks.is_empty() {
        String::new()
    } else {
        format!("\nThe checks that must pass: {}.", checks.join(", "))
    };
    format!(
        "Finish \"{}\" in {slug}. Its open tasks, in this order, one pull request each:\n{}\n\n\
         Each pull request implements its task, ticks that task's box in {TASKS} (`- [ ]` becomes `- [x]`), and appends one line to {REPORT}: the date, the task, what changed and how it was verified. Do not start tasks from later phases.{checks}\n\n{SHARED_RULES}",
        p.title,
        bullets(&p.open)
    )
}

/// The goal text for writing the next phase when every task is done. Pure.
pub fn next_phase_outcome(slug: &str, phases: &[Phase]) -> String {
    let done: Vec<String> = phases.iter().map(|p| p.title.clone()).collect();
    format!(
        "Every task in {TASKS} for {slug} is done ({}). Review what shipped ({REPORT}, the code, open issues, CI), then in one pull request add the next phase to {PHASES} and its tasks to {TASKS} in the same format. If the project truly needs nothing more right now, add a phase named `Maintenance` with dependency updates, test gaps and documentation fixes you found.\n\n{SHARED_RULES}",
        done.join(", ")
    )
}

/// Decide what one repository does this pass. `active` is its goal in flight and that goal's
/// status. Pure.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    slug: &str,
    enabled: bool,
    has_plan: bool,
    phases: &[Phase],
    active: Option<(&str, &str)>,
    st: &RepoState,
    room: bool,
    checks: &[String],
    now: i64,
) -> Action {
    if !enabled {
        return Action::Off("not opted in".into());
    }
    // A failed goal (a worker out of quota, a stopped retry) is followed by a fresh goal for the
    // same phase: the repository's task list still holds what is left. Twice a day at most, so a
    // goal that keeps failing waits for the owner. Pausing the repository stops this.
    let restart = active.is_some_and(|(_, s)| s == "failed")
        && recent_restarts(st, now) < MAX_RESTARTS_PER_DAY;
    if let Some((id, status)) = active.filter(|_| !restart) {
        return match status {
            "failed" | "paused" => Action::Wait(format!(
                "{id} is {status}: it needs you before more work starts"
            )),
            "done" => Action::Wait(format!(
                "{id} finished; the next goal starts on the next pass"
            )),
            s => Action::Wait(format!("{id} is {s}")),
        };
    }
    if now - st.last_start < MIN_GAP_SECS {
        return Action::Wait("started a goal less than 30 minutes ago".into());
    }
    if !room {
        return Action::Wait("the machine-wide limit of active repositories is reached".into());
    }
    if !has_plan {
        return Action::Start("plan".into(), plan_outcome(slug));
    }
    if let Some(p) = phases.iter().find(|p| !p.open.is_empty()) {
        return Action::Start("phase".into(), phase_outcome(slug, p, checks));
    }
    if now - st.last_next_phase < NEXT_PHASE_GAP_SECS {
        return Action::Wait(
            "every task is done; the next phase is written at most once a day".into(),
        );
    }
    Action::Start("next_phase".into(), next_phase_outcome(slug, phases))
}

/// The done check for a goal: the repository's test command for phase work, else the plan file.
/// Pure.
pub fn done_check(kind: &str, checks: &[String]) -> Check {
    let test = checks
        .iter()
        .find(|c| c.contains("test"))
        .filter(|_| kind == "phase");
    match test {
        Some(t) => Check {
            kind: "command".into(),
            target: t.clone(),
            repo: None,
        },
        None => Check {
            kind: "file".into(),
            target: TASKS.into(),
            repo: None,
        },
    }
}

pub fn push_event(st: &mut RepoState, e: Event) {
    st.log.push(e);
    if st.log.len() > LOG_LIMIT {
        let cut = st.log.len() - LOG_LIMIT;
        st.log.drain(..cut);
    }
}

/// The vault report for one repository. Pure.
pub fn report_note(s: &RepoStatus, st: &RepoState) -> String {
    let mut out = format!(
        "---\ngenerated_by: agentos-studio\ntype: autonomy-report\nrepo: {}\n---\n\n# {}: autonomous work\n\n- Opted in: {}{}\n- Level: {} (allow_merge {})\n- Goals finished: {}\n- Now: {}\n\n",
        s.slug,
        s.slug,
        if s.enabled { "yes" } else { "no" },
        if s.paused_here { " (paused on this machine)" } else { "" },
        s.level,
        if s.allow_merge { "on" } else { "off" },
        st.goals_done,
        s.next
    );
    if !s.phases.is_empty() {
        out.push_str("## Plan\n\n");
        for p in &s.phases {
            out.push_str(&format!(
                "- {}: {} done, {} open\n",
                p.title,
                p.done,
                p.open.len()
            ));
        }
        out.push('\n');
    }
    out.push_str("## Log\n\n");
    for e in st.log.iter().rev() {
        let when = chrono::DateTime::from_timestamp(e.at, 0)
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let goal = e
            .goal
            .as_deref()
            .map(|g| format!(" [[01_Projects/Goals/{g}|{g}]]"))
            .unwrap_or_default();
        out.push_str(&format!("- {when} {}{goal}: {}\n", e.kind, e.text));
    }
    out
}

/// Merge the opt-in into an existing routing.json value, keeping every other field. Pure.
/// A goal's text with the owner's direction for the repository, which wins over anything else in
/// the goal. Pure.
pub fn with_direction(outcome: &str, brief: &str) -> String {
    let b = brief.trim();
    if b.is_empty() {
        return outcome.to_string();
    }
    format!(
        "{outcome}\n\nThe owner's standing direction for this repository, which overrides anything above it and which the reviewer must enforce (request changes on any pull request that breaks it):\n{b}"
    )
}

pub fn opted_in_routing(existing: &str, level: &str, allow_merge: bool) -> Result<String> {
    opted_in_routing_with(existing, level, allow_merge, "")
}

/// As `opted_in_routing`, also setting the direction; an empty one keeps any existing direction.
pub fn opted_in_routing_with(
    existing: &str,
    level: &str,
    allow_merge: bool,
    brief: &str,
) -> Result<String> {
    opted_in_routing_full(existing, level, allow_merge, brief, &[])
}

/// The tool rules a worker needs to run a repository's own checks (and install its packages
/// first), from the checks Studio found. Pure.
pub fn tools_for_checks(checks: &[String], has_package_json: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if has_package_json {
        out.push("Bash(npm ci*)".into());
        out.push("Bash(npm install*)".into());
    }
    for c in checks {
        let c = c.trim().trim_start_matches("cd src-tauri && ");
        if !c.is_empty() {
            let rule = format!("Bash({c}*)");
            if !out.contains(&rule) {
                out.push(rule);
            }
        }
    }
    out
}

/// As `opted_in_routing_with`, also adding tool rules to `allowed_tools` (kept, deduplicated).
pub fn opted_in_routing_full(
    existing: &str,
    level: &str,
    allow_merge: bool,
    brief: &str,
    tools: &[String],
) -> Result<String> {
    let mut v: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(existing)
            .map_err(|e| anyhow!("routing.json is not valid JSON: {e}"))?
    };
    let o = v
        .as_object_mut()
        .ok_or_else(|| anyhow!("routing.json is not a JSON object"))?;
    if allow_merge {
        o.insert("allow_merge".into(), serde_json::Value::Bool(true));
    }
    let kept = o
        .get("autonomy")
        .and_then(|a| a.get("brief"))
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();
    let brief = if brief.trim().is_empty() {
        kept
    } else {
        brief.trim().to_string()
    };
    let mut a = serde_json::json!({ "enabled": true, "level": level });
    if !brief.is_empty() {
        a["brief"] = serde_json::Value::String(brief);
    }
    o.insert("autonomy".into(), a);
    if !tools.is_empty() {
        let mut list: Vec<serde_json::Value> = o
            .get("allowed_tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        for t in tools {
            let v = serde_json::Value::String(t.clone());
            if !list.contains(&v) {
                list.push(v);
            }
        }
        o.insert("allowed_tools".into(), serde_json::Value::Array(list));
    }
    // Validate with the real parser so a broken policy is never committed.
    let text = serde_json::to_string_pretty(&v)? + "\n";
    crate::routing::parse(&text)?;
    Ok(text)
}

// ---------------------------------------------------------------------------------------------
// The machine

fn state_key(slug: &str) -> String {
    format!("autonomy.state.{}", slug.to_lowercase())
}

fn paused_key(slug: &str) -> String {
    format!("autonomy.off.{}", slug.to_lowercase())
}

pub fn load_state(c: &rusqlite::Connection, slug: &str) -> RepoState {
    store::kv_get(c, &state_key(slug))
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_state(c: &rusqlite::Connection, slug: &str, st: &RepoState) -> Result<()> {
    store::kv_set(c, &state_key(slug), &serde_json::to_string(st)?)
}

pub fn enabled_globally(c: &rusqlite::Connection) -> bool {
    store::kv_get(c, KV_ENABLED).ok().flatten().as_deref() == Some("1")
}

pub fn max_active(c: &rusqlite::Connection) -> usize {
    store::kv_get(c, KV_MAX_ACTIVE)
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(DEFAULT_MAX_ACTIVE)
}

/// The `autonomy` block of a routing.json text. Pure.
pub fn config_in(text: &str) -> Config {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("autonomy").cloned())
        .and_then(|a| serde_json::from_value(a).ok())
        .unwrap_or_default()
}

/// The opt-in as committed on origin's default branch (where Opt in merges it), else the local
/// copy for a repository without a remote.
pub fn config_of(dir: &Path) -> Config {
    on_origin(dir, ".agentos/routing.json")
        .or_else(|| std::fs::read_to_string(crate::routing::path_in(dir)).ok())
        .map(|t| config_in(&t))
        .unwrap_or_default()
}

/// Bring the owner's checkout up to origin when it is safe: on the default branch, nothing
/// uncommitted, and a fast-forward only. The merge gate reads `allow_merge` from the checkout, so
/// an opt-in merged on GitHub must reach it; a checkout on another branch or with changes is left
/// alone. Returns whether it moved.
pub fn sync_checkout(dir: &Path) -> bool {
    let base = default_branch(dir);
    let here = crate::fleet::git(dir, &["branch", "--show-current"]);
    let dirty = !crate::fleet::git(dir, &["status", "--porcelain"])
        .trim()
        .is_empty();
    if !may_fast_forward(&here, &base, dirty) {
        return false;
    }
    let before = crate::fleet::git(dir, &["rev-parse", "HEAD"]);
    let _ = crate::fleet::git(
        dir,
        &["merge", "--ff-only", "-q", &format!("origin/{base}")],
    );
    crate::fleet::git(dir, &["rev-parse", "HEAD"]) != before
}

/// Only a clean checkout of the default branch is fast-forwarded. Pure.
pub fn may_fast_forward(current: &str, base: &str, dirty: bool) -> bool {
    !dirty && !current.trim().is_empty() && current.trim() == base
}

/// The default branch on origin (origin/HEAD, else main, else master).
fn default_branch(dir: &Path) -> String {
    let head = crate::fleet::git(
        dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    );
    if let Some(b) = head.trim().strip_prefix("origin/") {
        return b.to_string();
    }
    for b in ["main", "master"] {
        if !crate::fleet::git(dir, &["rev-parse", "--verify", &format!("origin/{b}")]).is_empty() {
            return b.into();
        }
    }
    "main".into()
}

/// A file as it is on origin's default branch, never the working copy.
pub(crate) fn on_origin(dir: &Path, rel: &str) -> Option<String> {
    let b = default_branch(dir);
    let out = crate::fleet::git(dir, &["show", &format!("origin/{b}:{rel}")]);
    (!out.trim().is_empty()).then_some(out)
}

fn norm(p: &Path) -> String {
    let s = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    s.display()
        .to_string()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// Every local git repository Studio can see: Hermes projects first (as their slug), then every
/// repository under the scan roots that no project covers yet (as its folder name). Pure.
pub fn merge_repos(
    projects: &[(String, PathBuf)],
    discovered: &[PathBuf],
    norm: impl Fn(&Path) -> String,
) -> Vec<(String, PathBuf, bool)> {
    let mut out: Vec<(String, PathBuf, bool)> = projects
        .iter()
        .map(|(s, d)| (s.clone(), d.clone(), true))
        .collect();
    let known: Vec<String> = projects.iter().map(|(_, d)| norm(d)).collect();
    for d in discovered {
        if known.contains(&norm(d)) {
            continue;
        }
        let name = d
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.is_empty() || out.iter().any(|(s, _, _)| s.eq_ignore_ascii_case(&name)) {
            continue;
        }
        out.push((name, d.clone(), false));
    }
    out
}

/// Hermes projects plus every repository under the scan roots, as (slug, folder, is a project).
pub fn all_repos() -> Vec<(String, PathBuf, bool)> {
    let roots = crate::settings::Settings::load().roots();
    merge_repos(&repos(), &crate::fleet::discover(&roots), norm)
}

/// Every Hermes project whose folder is a git repository, as (slug, folder). Only these can run
/// missions, so the pass works from this list; Opt in makes a repository one.
pub fn repos() -> Vec<(String, PathBuf)> {
    crate::projects::list()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.archived)
        .filter_map(|p| {
            let path = p
                .primary_path
                .clone()
                .or_else(|| p.folders.first().map(|f| f.path.clone()))?;
            let dir = PathBuf::from(path);
            dir.join(".git").exists().then_some((p.slug, dir))
        })
        .collect()
}

fn checks_of(dir: &Path) -> Vec<String> {
    let stack = crate::fleet::detect_stack(dir);
    let pkg = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default();
    crate::prompts::checks_for(&stack, |f| dir.join(f).exists(), &pkg)
}

fn status_for(
    c: &rusqlite::Connection,
    slug: &str,
    dir: &Path,
    fetch: bool,
    room: bool,
    now: i64,
) -> (RepoStatus, RepoState, Action) {
    if fetch {
        let _ = crate::fleet::git(dir, &["fetch", "-q", "origin"]);
        if config_of(dir).enabled {
            sync_checkout(dir);
        }
    }
    let cfg = config_of(dir);
    let allow = crate::routing::merge_allowed(dir);
    let paused_here = store::kv_get(c, &paused_key(slug))
        .ok()
        .flatten()
        .as_deref()
        == Some("1");
    let enabled = cfg.enabled && !paused_here;
    let tasks = on_origin(dir, TASKS);
    let has_plan = tasks.is_some() && on_origin(dir, PHASES).is_some();
    let phases = tasks.as_deref().map(parse_tasks).unwrap_or_default();
    let st = load_state(c, slug);
    let goal = st
        .goal
        .as_ref()
        .and_then(|id| goals::get(id).ok().flatten());
    let active = goal.as_ref().map(|g| (g.id.as_str(), g.status.as_str()));
    let checks = checks_of(dir);
    let action = decide(
        slug, enabled, has_plan, &phases, active, &st, room, &checks, now,
    );
    let next = match &action {
        Action::Off(r) | Action::Wait(r) => r.clone(),
        Action::Start(kind, _) => format!("ready to start a {} goal", kind.replace('_', " ")),
    };
    let status = RepoStatus {
        slug: slug.into(),
        path: dir.display().to_string(),
        hermes_project: true,
        enabled: cfg.enabled,
        paused_here,
        level: level(&cfg, allow),
        brief: cfg.brief.clone(),
        allow_merge: allow,
        has_plan,
        phases,
        goal: goal.as_ref().map(|g| g.id.clone()),
        goal_status: goal.as_ref().map(|g| g.status.clone()),
        next,
        log: st.log.iter().rev().take(15).cloned().collect(),
    };
    (status, st, action)
}

/// Every repository Studio can see, with its autonomy state (no fetch, no changes).
pub fn statuses() -> Result<Vec<RepoStatus>> {
    let c = store::open()?;
    let now = store::now();
    Ok(all_repos()
        .iter()
        .map(|(slug, dir, project)| {
            let mut s = status_for(&c, slug, dir, false, true, now).0;
            s.hermes_project = *project;
            s
        })
        .collect())
}

/// The Hermes project slug for a folder, creating the project when there is none: missions,
/// reviews and merges find a repository through its project.
pub fn ensure_project(name: &str, dir: &Path) -> Result<String> {
    let find = || {
        crate::projects::list()
            .unwrap_or_default()
            .into_iter()
            .find(|p| {
                p.folders
                    .iter()
                    .any(|f| norm(Path::new(&f.path)) == norm(dir))
            })
            .map(|p| p.slug)
    };
    if let Some(s) = find() {
        return Ok(s);
    }
    crate::projects::create(name, &[dir.display().to_string()])?;
    find().ok_or_else(|| anyhow!("Hermes did not list the new project for {}", dir.display()))
}

/// What one pass did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PassReport {
    pub considered: usize,
    pub started: Vec<String>,
    pub finished: Vec<String>,
}

/// One pass over every opted-in repository: close out finished goals, then start the next goal
/// where there is room. Does nothing while autonomy is off or the fleet is paused.
pub fn pass(v: &Vault) -> Result<PassReport> {
    let c = store::open()?;
    let mut rep = PassReport::default();
    if !enabled_globally(&c) || crate::pause::paused() {
        return Ok(rep);
    }
    let now = store::now();
    let all = repos();
    // Close out goals that ended, so their repository can move on.
    let mut active = 0usize;
    for (slug, _) in &all {
        let mut st = load_state(&c, slug);
        let Some(id) = st.goal.clone() else { continue };
        match goals::get(&id)?.map(|g| g.status) {
            Some(s) if s == "done" => {
                st.goal = None;
                let kind = st.goal_kind.replace('_', " ");
                st.goals_done += 1;
                push_event(
                    &mut st,
                    Event {
                        at: now,
                        kind: "finished".into(),
                        goal: Some(id.clone()),
                        text: format!("{kind} goal done"),
                    },
                );
                save_state(&c, slug, &st)?;
                rep.finished.push(id);
            }
            None => {
                st.goal = None;
                push_event(
                    &mut st,
                    Event {
                        at: now,
                        kind: "gone".into(),
                        goal: Some(id),
                        text: "the goal was deleted; moving on".into(),
                    },
                );
                save_state(&c, slug, &st)?;
            }
            // A failed goal holds no worker; its repository restarts it below.
            Some(s) if s == "failed" => {}
            Some(_) => active += 1,
        }
    }
    let cap = max_active(&c);
    for (slug, dir) in &all {
        if !config_of(dir).enabled {
            continue;
        }
        rep.considered += 1;
        let room = active < cap;
        let (status, mut st, action) = status_for(&c, slug, dir, true, room, now);
        if let Action::Start(kind, outcome) = action {
            match start_goal(v, slug, &kind, &outcome, &status, dir) {
                Ok(id) => {
                    if status.goal_status.as_deref() == Some("failed") {
                        st.restarts.retain(|t| now - *t < 24 * 3600);
                        st.restarts.push(now);
                        push_event(
                            &mut st,
                            Event {
                                at: now,
                                kind: "restarted".into(),
                                goal: status.goal.clone(),
                                text: "the goal failed; starting a fresh goal for what is left"
                                    .into(),
                            },
                        );
                    }
                    st.goal = Some(id.clone());
                    st.goal_kind = kind.clone();
                    st.last_start = now;
                    if kind == "next_phase" {
                        st.last_next_phase = now;
                    }
                    push_event(
                        &mut st,
                        Event {
                            at: now,
                            kind: "started".into(),
                            goal: Some(id.clone()),
                            text: status.next.clone(),
                        },
                    );
                    active += 1;
                    rep.started.push(id);
                }
                Err(e) => {
                    st.last_start = now;
                    push_event(
                        &mut st,
                        Event {
                            at: now,
                            kind: "error".into(),
                            goal: None,
                            text: format!("{e:#}"),
                        },
                    );
                }
            }
            save_state(&c, slug, &st)?;
        }
        let _ = v.write_note(
            &format!("01_Projects/Autonomy/{slug}.md"),
            &report_note(
                &status_for(&c, slug, dir, false, true, now).0,
                &load_state(&c, slug),
            ),
        );
    }
    Ok(rep)
}

fn start_goal(
    v: &Vault,
    slug: &str,
    kind: &str,
    outcome: &str,
    s: &RepoStatus,
    dir: &Path,
) -> Result<String> {
    let cfg = config_of(dir);
    let g = goals::create(&NewGoal {
        outcome: with_direction(outcome, &cfg.brief),
        repos: vec![slug.into()],
        checks: vec![done_check(kind, &checks_of(dir))],
        budget_usd: cfg.budget_usd.max(0.0),
        deadline: None,
        autonomy: Some(s.level.clone()),
    })?;
    crate::planner::plan_in_background(v.root.clone(), g.id.clone());
    Ok(g.id)
}

/// Turn autonomy off on this machine only, or back on. The committed opt-in is untouched.
pub fn set_paused_here(slug: &str, paused: bool) -> Result<()> {
    let c = store::open()?;
    store::kv_set(&c, &paused_key(slug), if paused { "1" } else { "0" })
}

/// Opt a repository in: a pull request that adds the `autonomy` block (and `allow_merge` when
/// the level is full) to `.agentos/routing.json`, made in Studio's own worktree from origin's
/// default branch, then merged. Returns the pull request URL. The owner's checkout is untouched.
pub fn opt_in(slug: &str, dir: &Path, level_wanted: &str, brief: &str) -> Result<String> {
    if !goals::LEVELS.contains(&level_wanted) {
        bail!("level must be watch, ask or full");
    }
    let _ = crate::fleet::git(dir, &["fetch", "-q", "origin"]);
    let base = default_branch(dir);
    let slug = &ensure_project(slug, dir)?;
    let wt = store::studio_dir().join("autonomy-optin").join(slug);
    let _ = std::fs::remove_dir_all(&wt);
    let _ = crate::fleet::git(dir, &["worktree", "prune"]);
    let branch = format!("agentos/autonomy-opt-in-{}", store::now());
    let ok = |args: &[&str], at: &Path| -> Result<String> {
        let (code, out, err) = crate::procs::run_with_timeout(
            "git",
            args,
            None,
            Some(at),
            std::time::Duration::from_secs(120),
        )?;
        if code != Some(0) {
            bail!("git {}: {}", args.join(" "), err.trim());
        }
        Ok(out)
    };
    let wts = wt.display().to_string();
    ok(
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            &wts,
            &format!("origin/{base}"),
        ],
        dir,
    )?;
    let result = (|| -> Result<String> {
        let rel = ".agentos/routing.json";
        let existing = std::fs::read_to_string(wt.join(rel)).unwrap_or_default();
        let tools = tools_for_checks(&checks_of(&wt), wt.join("package.json").exists());
        let text = opted_in_routing_full(
            &existing,
            level_wanted,
            level_wanted == "full",
            brief,
            &tools,
        )?;
        std::fs::create_dir_all(wt.join(".agentos"))?;
        std::fs::write(wt.join(rel), text)?;
        ok(&["add", rel], &wt)?;
        ok(
            &[
                "commit",
                "-q",
                "-m",
                "chore(agentos): opt in to autonomous maintenance",
            ],
            &wt,
        )?;
        ok(&["push", "-q", "-u", "origin", &branch], &wt)?;
        let body = format!("Opts {slug} in to AgentOS Studio autonomous maintenance at level {level_wanted}: agents keep {PHASES} and {TASKS}, take the next open task as a mission, open a pull request for each, and the review and merge gate merges it. See docs/AUTONOMOUS-REPOS.md in AgentOS Studio.");
        let (code, out, err) = crate::procs::run_with_timeout(
            "gh",
            &[
                "pr",
                "create",
                "--base",
                &base,
                "--head",
                &branch,
                "--title",
                "chore(agentos): opt in to autonomous maintenance",
                "--body",
                &body,
            ],
            None,
            Some(&wt),
            std::time::Duration::from_secs(60),
        )?;
        if code != Some(0) {
            bail!("gh pr create: {}", err.trim());
        }
        let url = out
            .lines()
            .rev()
            .find(|l| l.starts_with("https://"))
            .unwrap_or("")
            .trim()
            .to_string();
        let (code, _, err) = crate::procs::run_with_timeout(
            "gh",
            &["pr", "merge", &url, "--squash", "--delete-branch"],
            None,
            Some(&wt),
            std::time::Duration::from_secs(90),
        )?;
        if code != Some(0) {
            return Ok(format!("{url} (opened; merge it yourself: {})", err.trim()));
        }
        Ok(url)
    })();
    let _ = ok(&["worktree", "remove", "--force", &wts], dir);
    let _ = crate::fleet::git(dir, &["branch", "-D", &branch]);
    let _ = crate::fleet::git(dir, &["fetch", "-q", "origin"]);
    sync_checkout(dir);
    let c = store::open()?;
    let mut st = load_state(&c, slug);
    let text = match &result {
        Ok(u) => format!("opted in at {level_wanted}: {u}"),
        Err(e) => format!("opt-in failed: {e:#}"),
    };
    push_event(
        &mut st,
        Event {
            at: store::now(),
            kind: "opt-in".into(),
            goal: None,
            text,
        },
    );
    save_state(&c, slug, &st)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASKS_MD: &str = "# Tasks\n\n## Phase 1: Foundations\n- [x] Add CI\n- [X] Add a README\n\n## Phase 2: Checkout\n- [ ] Add the cart page\n- [ ] Add payment\n- [x] Pick a provider\n\n## Phase 3: Launch\n- [ ] Write the launch post\n";

    #[test]
    fn tasks_parse_into_phases_with_open_and_done_counts() {
        let p = parse_tasks(TASKS_MD);
        assert_eq!(p.len(), 3);
        assert_eq!(
            (p[0].title.as_str(), p[0].done, p[0].open.len()),
            ("Phase 1: Foundations", 2, 0)
        );
        assert_eq!(p[1].open, ["Add the cart page", "Add payment"]);
        assert_eq!(p[1].done, 1);
        assert_eq!(parse_tasks("- [ ] loose task")[0].title, "Tasks");
        assert!(parse_tasks("## Empty heading\nprose").is_empty());
    }

    #[test]
    fn the_level_never_merges_without_allow_merge() {
        let c = |l: &str| Config {
            enabled: true,
            level: l.into(),
            budget_usd: 0.0,
            brief: String::new(),
        };
        assert_eq!(level(&c("full"), true), "full");
        assert_eq!(level(&c("full"), false), "ask");
        assert_eq!(level(&c(""), true), "full");
        assert_eq!(level(&c(""), false), "ask");
        assert_eq!(level(&c("watch"), true), "watch");
        assert_eq!(level(&c("bogus"), false), "ask");
    }

    #[test]
    fn decide_walks_plan_then_phases_then_next_phase() {
        let st = RepoState::default();
        let now = 10_000_000;
        let none: &[Phase] = &[];
        assert!(matches!(
            decide("shop", false, false, none, None, &st, true, &[], now),
            Action::Off(_)
        ));
        match decide("shop", true, false, none, None, &st, true, &[], now) {
            Action::Start(k, o) => {
                assert_eq!(k, "plan");
                assert!(o.contains(PHASES) && o.contains(TASKS) && o.contains(REPORT));
            }
            a => panic!("{a:?}"),
        }
        let phases = parse_tasks(TASKS_MD);
        match decide(
            "shop",
            true,
            true,
            &phases,
            None,
            &st,
            true,
            &["npm test".into()],
            now,
        ) {
            Action::Start(k, o) => {
                assert_eq!(k, "phase");
                assert!(o.contains("Phase 2: Checkout") && o.contains("- Add the cart page"));
                assert!(!o.contains("launch post"), "later phases stay out");
                assert!(o.contains("npm test"));
            }
            a => panic!("{a:?}"),
        }
        let all_done = parse_tasks("## P1\n- [x] a\n");
        assert!(
            matches!(decide("shop", true, true, &all_done, None, &st, true, &[], now), Action::Start(k, _) if k == "next_phase")
        );
        let recent = RepoState {
            last_next_phase: now - 60,
            ..Default::default()
        };
        assert!(matches!(
            decide("shop", true, true, &all_done, None, &recent, true, &[], now),
            Action::Wait(_)
        ));
        // In flight, too soon, or no room: wait.
        assert!(
            matches!(decide("shop", true, true, &phases, Some(("G-001", "running")), &st, true, &[], now), Action::Wait(r) if r.contains("G-001"))
        );
        // A failed goal is followed by a fresh one for the same phase, twice a day at most.
        assert!(
            matches!(decide("shop", true, true, &phases, Some(("G-002", "failed")), &st, true, &[], now), Action::Start(k, _) if k == "phase")
        );
        let twice = RepoState {
            restarts: vec![now - 3600, now - 7200],
            ..Default::default()
        };
        assert!(
            matches!(decide("shop", true, true, &phases, Some(("G-002", "failed")), &twice, true, &[], now), Action::Wait(r) if r.contains("needs you"))
        );
        let yesterday = RepoState {
            restarts: vec![now - 25 * 3600, now - 26 * 3600],
            ..Default::default()
        };
        assert!(matches!(
            decide(
                "shop",
                true,
                true,
                &phases,
                Some(("G-002", "failed")),
                &yesterday,
                true,
                &[],
                now
            ),
            Action::Start(..)
        ));
        assert!(
            matches!(decide("shop", true, true, &phases, Some(("G-002", "paused")), &st, true, &[], now), Action::Wait(r) if r.contains("needs you"))
        );
        let soon = RepoState {
            last_start: now - 60,
            ..Default::default()
        };
        assert!(matches!(
            decide("shop", true, true, &phases, None, &soon, true, &[], now),
            Action::Wait(_)
        ));
        assert!(
            matches!(decide("shop", true, true, &phases, None, &st, false, &[], now), Action::Wait(r) if r.contains("limit"))
        );
    }

    #[test]
    fn outcomes_and_checks_follow_the_rules() {
        for o in [
            plan_outcome("x"),
            phase_outcome("x", &parse_tasks(TASKS_MD)[1], &[]),
            next_phase_outcome("x", &parse_tasks(TASKS_MD)),
        ] {
            assert!(!o.contains('\u{2014}') && !o.contains('\u{2013}'));
            assert!(o.contains("never pushed to main"));
        }
        assert_eq!(
            done_check("phase", &["npm run build".into(), "npm test".into()]).target,
            "npm test"
        );
        assert_eq!(done_check("plan", &["npm test".into()]).target, TASKS);
        assert_eq!(done_check("phase", &[]).kind, "file");
        goals::validate_check(&done_check("plan", &[])).unwrap();
    }

    #[test]
    fn opt_in_keeps_the_existing_policy_and_validates() {
        let old = r#"{"roles":{"implementer":{"provider":"claude-subscription"}},"allowed_tools":["Bash(npm test*)"]}"#;
        let new = opted_in_routing(old, "full", true).unwrap();
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert_eq!(v["allow_merge"], true);
        assert_eq!(v["autonomy"]["enabled"], true);
        assert_eq!(v["autonomy"]["level"], "full");
        assert_eq!(v["allowed_tools"][0], "Bash(npm test*)");
        let ask = opted_in_routing("", "ask", false).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&ask)
            .unwrap()
            .get("allow_merge")
            .is_none());
        assert!(opted_in_routing("not json", "ask", false).is_err());
        let t = tools_for_checks(
            &[
                "npm run build".into(),
                "npm test".into(),
                "cd src-tauri && cargo test".into(),
            ],
            true,
        );
        assert_eq!(
            t,
            [
                "Bash(npm ci*)",
                "Bash(npm install*)",
                "Bash(npm run build*)",
                "Bash(npm test*)",
                "Bash(cargo test*)"
            ]
        );
        let with_tools = opted_in_routing_full(old, "full", true, "", &t).unwrap();
        let v: serde_json::Value = serde_json::from_str(&with_tools).unwrap();
        let list: Vec<&str> = v["allowed_tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(list[0], "Bash(npm test*)", "existing rules are kept first");
        assert_eq!(
            list.iter().filter(|r| **r == "Bash(npm test*)").count(),
            1,
            "no duplicates"
        );
        assert!(list.contains(&"Bash(npm ci*)"));
        let with = opted_in_routing_with(old, "full", true, "Keep the design").unwrap();
        assert_eq!(config_in(&with).brief, "Keep the design");
        let again = opted_in_routing_with(&with, "full", true, "").unwrap();
        assert_eq!(
            config_in(&again).brief,
            "Keep the design",
            "an empty direction keeps the old one"
        );
        let o = with_direction("Finish phase 1.", "Change content only.");
        assert!(
            o.starts_with("Finish phase 1.")
                && o.ends_with("Change content only.")
                && o.contains("reviewer must enforce")
        );
        assert_eq!(with_direction("Finish.", "  "), "Finish.");
        assert!(
            opted_in_routing(r#"{"roles":{"wizard":{"provider":"x"}}}"#, "ask", false).is_err(),
            "a broken policy is never committed"
        );
    }

    #[test]
    fn state_round_trips_and_the_log_is_bounded() {
        let d = tempfile::tempdir().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        let c = store::open().unwrap();
        let mut st = RepoState::default();
        for i in 0..(LOG_LIMIT + 5) {
            push_event(
                &mut st,
                Event {
                    at: i as i64,
                    kind: "x".into(),
                    goal: None,
                    text: String::new(),
                },
            );
        }
        assert_eq!(st.log.len(), LOG_LIMIT);
        assert_eq!(st.log[0].at, 5);
        save_state(&c, "Shop", &st).unwrap();
        assert_eq!(load_state(&c, "shop").log.len(), LOG_LIMIT);
        assert!(!enabled_globally(&c));
        assert_eq!(max_active(&c), DEFAULT_MAX_ACTIVE);
        store::set_test_db(None);
    }

    /// Live, read-only: every repository the Autonomy screen would list on this machine.
    /// `cargo test --lib -- --ignored autonomy_live_list --nocapture`
    #[test]
    #[ignore]
    fn autonomy_live_list() {
        let all = statuses().unwrap();
        for s in &all {
            println!(
                "{:<40} project={} enabled={} plan={} next={}",
                s.slug, s.hermes_project, s.enabled, s.has_plan, s.next
            );
        }
        println!("{} repositories", all.len());
        assert!(all
            .iter()
            .any(|s| s.slug == "agentos-studio" && s.hermes_project));
    }

    #[test]
    fn the_opt_in_is_read_from_routing_text_and_only_clean_default_checkouts_move() {
        assert!(config_in(r#"{"autonomy":{"enabled":true,"level":"full"}}"#).enabled);
        assert!(!config_in(r#"{"allow_merge":true}"#).enabled);
        assert!(!config_in("not json").enabled);
        assert!(may_fast_forward("main", "main", false));
        assert!(
            !may_fast_forward("main", "main", true),
            "uncommitted work stays put"
        );
        assert!(
            !may_fast_forward("feature", "main", false),
            "another branch stays put"
        );
        assert!(
            !may_fast_forward("", "main", false),
            "a detached head stays put"
        );
    }

    #[test]
    fn every_local_repository_is_listed_once() {
        let n = |p: &Path| p.display().to_string().to_lowercase().replace('\\', "/");
        let projects = vec![("shop-app".to_string(), PathBuf::from("C:/r/Shop"))];
        let found = vec![
            PathBuf::from("c:/r/shop"),
            PathBuf::from("C:/r/blog"),
            PathBuf::from("C:/other/blog"),
        ];
        let m = merge_repos(&projects, &found, n);
        assert_eq!(
            m.iter()
                .map(|(s, _, p)| (s.as_str(), *p))
                .collect::<Vec<_>>(),
            [("shop-app", true), ("blog", false)],
            "a project folder is not listed twice, and two folders with one name keep the first"
        );
    }

    #[test]
    fn the_report_names_plan_progress_and_the_log() {
        let s = RepoStatus {
            slug: "shop".into(),
            path: "C:/r/shop".into(),
            hermes_project: true,
            enabled: true,
            paused_here: false,
            level: "full".into(),
            brief: String::new(),
            allow_merge: true,
            has_plan: true,
            phases: parse_tasks(TASKS_MD),
            goal: None,
            goal_status: None,
            next: "G-004 is running".into(),
            log: vec![],
        };
        let mut st = RepoState::default();
        push_event(
            &mut st,
            Event {
                at: 0,
                kind: "started".into(),
                goal: Some("G-004".into()),
                text: "phase".into(),
            },
        );
        let n = report_note(&s, &st);
        assert!(n.starts_with("---\ngenerated_by: agentos-studio"));
        assert!(n.contains("Phase 2: Checkout: 1 done, 2 open"));
        assert!(n.contains("[[01_Projects/Goals/G-004|G-004]]"));
    }
}
