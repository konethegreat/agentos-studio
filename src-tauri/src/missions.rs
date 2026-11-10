//! Missions: the only object allowed to authorise a merge.
//!
//! A mission is not a chat. It is a durable unit of work with an owner role, acceptance criteria,
//! a provider, a budget and a review verdict. Chats and workers are what a mission spawns.
//!
//! The status machine is deliberately small:
//!
//! ```text
//! queued -> running -> review -> done
//!                  \-> blocked -> queued
//!                         \-> failed
//! ```
//!
//! `review` is where a second agent decides, and `done` is only reachable through an approval.
//! Nothing here merges; `merge_blockers` is the policy a merger must satisfy.

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::store;

pub const ROLES: [&str; 5] = ["planner", "implementer", "reviewer", "gatekeeper", "memory"];
pub const STATUSES: [&str; 6] = ["queued", "running", "review", "blocked", "done", "failed"];

#[derive(Debug, Clone, Serialize)]
pub struct Mission {
    pub id: String,
    pub repo: String,
    pub goal: String,
    pub acceptance: String,
    pub role: String,
    pub agent: String,
    pub status: String,
    pub provider: String,
    pub model: String,
    pub budget_usd: f64,
    pub spent_usd: f64,
    pub task_id: Option<String>,
    pub reviewer: Option<String>,
    pub review_verdict: Option<String>,
    pub pr_url: Option<String>,
    pub review_note: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// `hermes verify --json` output from the mission's repository, recorded before review.
    pub verify: Option<String>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The next free id: one above the highest number in use, so a deleted mission never makes a
/// later one collide with an existing id (a row count would).
fn next_id(c: &Connection) -> Result<String> {
    let mut st = c.prepare("SELECT id FROM missions")?;
    let max = st
        .query_map([], |r| r.get::<_, String>(0))?
        .flatten()
        .filter_map(|id| id.strip_prefix("M-").and_then(|n| n.parse::<i64>().ok()))
        .max()
        .unwrap_or(0);
    Ok(format!("M-{:03}", max + 1))
}

/// Remove a mission that is not in flight (queued, blocked, done or failed). Used by live tests to
/// leave nothing behind; a running or in-review mission must be stopped or judged first.
pub fn delete(id: &str) -> Result<()> {
    let m = get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    if m.status == "running" || m.status == "review" {
        bail!("{id} is {}; stop or judge it before deleting", m.status);
    }
    let c = store::open()?;
    c.execute("DELETE FROM missions WHERE id = ?1", params![id])?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn create(
    repo: &str,
    goal: &str,
    acceptance: &str,
    role: &str,
    agent: &str,
    provider: &str,
    model: &str,
    budget_usd: f64,
) -> Result<Mission> {
    if repo.trim().is_empty() || goal.trim().is_empty() {
        bail!("a mission needs a repo and a goal");
    }
    if acceptance.trim().is_empty() {
        bail!(
            "a mission needs acceptance criteria; without them the reviewer has nothing to judge"
        );
    }
    if !ROLES.contains(&role) {
        bail!("unknown role '{role}'");
    }
    if budget_usd < 0.0 {
        bail!("budget cannot be negative");
    }
    let c = store::open()?;
    let id = next_id(&c)?;
    let t = now();
    c.execute(
        "INSERT INTO missions
         (id, repo, goal, acceptance, role, agent, status, provider, model,
          budget_usd, spent_usd, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8, ?9, 0, ?10, ?10)",
        params![
            id,
            repo.trim(),
            goal.trim(),
            acceptance.trim(),
            role,
            agent,
            provider,
            model,
            budget_usd,
            t
        ],
    )?;
    get(&id)?.ok_or_else(|| anyhow::anyhow!("mission vanished after insert"))
}

pub fn get(id: &str) -> Result<Option<Mission>> {
    let c = store::open()?;
    Ok(c.query_row(
        "SELECT id, repo, goal, acceptance, role, agent, status, provider, model,
                budget_usd, spent_usd, task_id, reviewer, review_verdict, review_note,
                created_at, updated_at, pr_url, verify_json
         FROM missions WHERE id = ?1",
        params![id],
        row,
    )
    .optional()?)
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Mission> {
    Ok(Mission {
        id: r.get(0)?,
        repo: r.get(1)?,
        goal: r.get(2)?,
        acceptance: r.get(3)?,
        role: r.get(4)?,
        agent: r.get(5)?,
        status: r.get(6)?,
        provider: r.get(7)?,
        model: r.get(8)?,
        budget_usd: r.get(9)?,
        spent_usd: r.get(10)?,
        task_id: r.get(11)?,
        reviewer: r.get(12)?,
        review_verdict: r.get(13)?,
        pr_url: r.get(17)?,
        review_note: r.get(14)?,
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
        verify: r.get(18)?,
    })
}

pub fn list(limit: usize) -> Result<Vec<Mission>> {
    let c = store::open()?;
    let mut st = c.prepare(
        "SELECT id, repo, goal, acceptance, role, agent, status, provider, model,
                budget_usd, spent_usd, task_id, reviewer, review_verdict, review_note,
                created_at, updated_at, pr_url, verify_json
         FROM missions
         ORDER BY CASE status WHEN 'review' THEN 0 WHEN 'running' THEN 1 WHEN 'queued' THEN 2
                              WHEN 'blocked' THEN 3 ELSE 4 END, updated_at DESC
         LIMIT ?1",
    )?;
    let rows = st.query_map(params![limit as i64], row)?;
    Ok(rows.flatten().collect())
}

fn set_status(c: &Connection, id: &str, from: &[&str], to: &str) -> Result<()> {
    let m = get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    if !from.contains(&m.status.as_str()) {
        bail!("{id} is {}, cannot move to {to}", m.status);
    }
    c.execute(
        "UPDATE missions SET status = ?1, updated_at = ?2 WHERE id = ?3",
        params![to, now(), id],
    )?;
    // Unit tests use a thread-local store; the notifier's thread would read the real one.
    if matches!(to, "done" | "failed") && m.status != to && !cfg!(test) {
        // `hermes send` to the user's messaging target, when one is set; off by default.
        let title: String = m.goal.chars().take(80).collect();
        let pr = m.pr_url.clone();
        let outcome = if to == "done" { "is done" } else { "failed" };
        std::thread::spawn(move || {
            crate::hermes_surfaces::notify_mission(&title, outcome, pr.as_deref())
        });
    }
    Ok(())
}

/// Start a queued mission. The single dispatch path for the board button and the scheduler:
/// Hermes missions go on Hermes' kanban board (Hermes owns execution); every other agent gets an
/// Agent Bus task.
pub fn start(v: &crate::vault::Vault, id: &str) -> Result<Mission> {
    let m = get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    if m.status != "queued" && m.status != "blocked" {
        bail!(
            "{} is {}; only a queued or blocked mission can start",
            id,
            m.status
        );
    }
    // `auto`: the router picks the agent now, from live quota and measured outcomes.
    let m = if m.agent.trim().eq_ignore_ascii_case(crate::assign::AUTO) {
        crate::assign::resolve(&m)?;
        get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?
    } else {
        m
    };
    // A new attempt starts with a clean heartbeat history.
    crate::stuck::reset(id);
    // A design decision goes to a council of agents on different providers, not one worker.
    if kind(id) == KIND_DECISION {
        return crate::council::start(v, &m);
    }
    if uses_kanban(&m) {
        let task_id = crate::kanban::create_task(&m)?;
        return dispatch(id, &task_id);
    }
    // Agent work happens in a known project folder, never in the home folder.
    let dir = crate::review_agent::project_dir(&m.repo)
        .filter(|d| d.is_dir())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{id}: repository '{}' is not a known project with a folder. Add it in Projects first.",
                m.repo
            )
        })?;
    // Every attempt gets its own worktree and branch, so a mission never shares a checkout.
    let wt = crate::worktree::create(&dir, &m)?;
    let conventions = crate::learning::conventions_for(v, &m.repo);
    let args = crate::vault::DelegateArgs {
        title: m.goal.chars().take(90).collect(),
        assigned_to: m.agent.clone(),
        creator: "agentos-studio".into(),
        project: m.repo.clone(),
        context_files: context_files(&dir),
        instructions: bus_instructions(&m, &dir, Some(&wt), &conventions),
        priority: "high".into(),
    };
    let task = v.delegate_task(args)?;
    let task_id = task
        .get("id")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("the bus did not return a task id"))?;
    let started = dispatch(id, &task_id)?;
    // Headless agents get a worker now; IDE agents pick the task up from the bus.
    let agent = m.agent.trim().to_lowercase();
    if crate::bus::headless_available(&agent) {
        if let Err(e) = crate::bus::start_worker(v, &task_id, &agent) {
            let _ = v.log_activity(&task_id, &format!("no worker started: {e:#}"));
        }
    }
    Ok(started)
}

/// The files an agent should read before starting: the shared system context and the
/// repository's own guide files that exist. Paths are absolute, except vault-relative memory.
pub fn context_files(dir: &std::path::Path) -> Vec<String> {
    let mut out = vec!["03_Memory/SYSTEM_CONTEXT.md".to_string()];
    for f in ["README.md", "CLAUDE.md", "AGENTS.md"] {
        let p = dir.join(f);
        if p.is_file() {
            out.push(p.display().to_string());
        }
    }
    out
}

/// The bus task body for a mission. Carries the `Repository:` line every worker resolves its
/// working folder from, the acceptance criteria, on rework the reviewer's note, and the
/// repository's active conventions. Pure.
pub fn bus_instructions(
    m: &Mission,
    dir: &std::path::Path,
    worktree: Option<&std::path::Path>,
    conventions: &str,
) -> String {
    let mut b = format!(
        "{}\n\nAcceptance criteria:\n{}\n\nMission: {}. Role: {}. Provider: {}.\n\nWork in a branch, open a pull request with gh, and put the pull request URL in your complete_task summary. Do not merge: AgentOS Studio reviews and merges.",
        m.goal.trim(),
        m.acceptance.trim(),
        m.id,
        m.role,
        if m.provider.trim().is_empty() { "default" } else { m.provider.trim() }
    );
    b.push_str(crate::constitution_gate::WORKER_RULES);
    if m.review_verdict.as_deref() == Some("reject") {
        if let Some(note) = m.review_note.as_deref().filter(|n| !n.trim().is_empty()) {
            b.push_str(&format!(
                "\n\nThis is a rework. The previous attempt was rejected by the reviewer:\n{}",
                note.trim()
            ));
        }
    }
    if !conventions.is_empty() {
        b.push_str(&format!(
            "\n\nConventions from earlier reviews in this repository:\n{}",
            conventions
        ));
    }
    b.push_str(&format!("\n\nRepository: {}", dir.display()));
    if let Some(wt) = worktree {
        b.push_str(&format!(
            "\nWorktree: {}\nBranch: {}",
            wt.display(),
            crate::worktree::branch_for(m)
        ));
    }
    if !m.model.trim().is_empty() {
        b.push_str(&format!("\nModel: {}", m.model.trim()));
    }
    if let Some(pack) = crate::agent_packs::for_mission(&m.id) {
        b.push_str(&format!("\nAgent pack: {pack}"));
    }
    b
}

/// A mission is ordinary work unless it is marked a design decision, which a council decides.
pub const KIND_WORK: &str = "work";
pub const KIND_DECISION: &str = "decision";

/// The mission's kind (`missions.kind`, schema 9). Unknown missions read as work.
pub fn kind(id: &str) -> String {
    store::open()
        .ok()
        .and_then(|c| {
            c.query_row(
                "SELECT kind FROM missions WHERE id = ?1",
                params![id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
        })
        .unwrap_or_else(|| KIND_WORK.into())
}

/// Mark a queued or blocked mission as work or a design decision.
pub fn set_kind(id: &str, kind: &str) -> Result<()> {
    if kind != KIND_WORK && kind != KIND_DECISION {
        bail!("kind must be work or decision, got '{kind}'");
    }
    let c = store::open()?;
    let n = c.execute(
        "UPDATE missions SET kind = ?1, updated_at = ?2 WHERE id = ?3 AND status IN ('queued', 'blocked')",
        params![kind, now(), id],
    )?;
    if n == 0 {
        bail!("{id} is not queued or blocked, so its kind cannot change");
    }
    Ok(())
}

/// Hermes missions run through `hermes kanban`; the rest through the Agent Bus. Pure.
pub fn uses_kanban(m: &Mission) -> bool {
    m.agent.trim().eq_ignore_ascii_case("hermes")
}

/// Attach the bus task that will do the work, and start the mission.
pub fn dispatch(id: &str, task_id: &str) -> Result<Mission> {
    let c = store::open()?;
    set_status(&c, id, &["queued", "blocked"], "running")?;
    c.execute(
        "UPDATE missions SET task_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![task_id, now(), id],
    )?;
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// Hand a running mission to another agent (an IDE agent that never picked it up). The reason
/// is kept in kv `mission.note.<id>` so the board can show why the agent changed.
pub fn reassign(id: &str, agent: &str, reason: &str) -> Result<Mission> {
    let c = store::open()?;
    c.execute(
        "UPDATE missions SET agent = ?1, updated_at = ?2 WHERE id = ?3 AND status = 'running'",
        params![agent, now(), id],
    )?;
    let _ = store::kv_set(&c, &format!("mission.note.{id}"), reason.trim());
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// Set the agent of a mission that has not started (quota-aware assignment of an `auto` mission).
pub fn set_agent(id: &str, agent: &str) -> Result<()> {
    let c = store::open()?;
    let n = c.execute(
        "UPDATE missions SET agent = ?1, updated_at = ?2 WHERE id = ?3 AND status IN ('queued', 'blocked')",
        params![agent, now(), id],
    )?;
    if n == 0 {
        bail!("{id} has started, so its agent is not reassigned here");
    }
    Ok(())
}

/// A handoff: the mission keeps running, now held by `agent` on the new bus task `task_id`.
/// The reason is kept in kv `mission.note.<id>` like a reassignment.
pub fn hand_to(id: &str, agent: &str, task_id: &str, reason: &str) -> Result<Mission> {
    let c = store::open()?;
    let n = c.execute(
        "UPDATE missions SET agent = ?1, task_id = ?2, updated_at = ?3 WHERE id = ?4 AND status = 'running'",
        params![agent, task_id, now(), id],
    )?;
    if n == 0 {
        bail!("{id} is not running, so it cannot be handed off");
    }
    let _ = store::kv_set(&c, &format!("mission.note.{id}"), reason.trim());
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// The pull request a mission produced. Used by the reviewer and the merge gate.
pub fn set_pr(id: &str, url: &str) -> Result<()> {
    let c = store::open()?;
    c.execute(
        "UPDATE missions SET pr_url = ?1, updated_at = ?2 WHERE id = ?3",
        params![url.trim(), now(), id],
    )?;
    Ok(())
}

/// The first GitHub pull request URL in a piece of text.
pub fn pull_url(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(i) = rest.find("https://github.com/") {
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == ')' || c == '>' || c == '"')
            .unwrap_or(tail.len());
        let url = &tail[..end];
        if url.contains("/pull/") {
            return Some(url.trim_end_matches(['.', ',']).to_string());
        }
        rest = &tail[end..];
    }
    None
}

/// The worker's own words in a completed bus task: the text under the last `## Completion summary`
/// heading, up to the next `## ` heading. Everything above it (the instructions, a handoff packet
/// with recall results and the project brief) was written by someone else and may quote other
/// pull requests, so a mission's pull request is only ever read from here.
pub fn completion_summary(body: &str) -> Option<String> {
    let lines: Vec<&str> = body.lines().collect();
    let start = lines
        .iter()
        .rposition(|l| l.trim() == "## Completion summary")?;
    let rest = &lines[start + 1..];
    let end = rest
        .iter()
        .position(|l| l.starts_with("## "))
        .unwrap_or(rest.len());
    let text = rest[..end].join("\n");
    Some(text).filter(|t| !t.trim().is_empty())
}

/// Record the `hermes verify --json` result for the mission's repository.
pub fn set_verify(id: &str, json: &str) -> Result<()> {
    let c = store::open()?;
    c.execute(
        "UPDATE missions SET verify_json = ?1, updated_at = ?2 WHERE id = ?3",
        params![json, now(), id],
    )?;
    Ok(())
}

/// Replace the measured spend (Hermes usage data is cumulative, so this is a total, not a delta).
pub fn set_spent(id: &str, total_usd: f64) -> Result<()> {
    let c = store::open()?;
    c.execute(
        "UPDATE missions SET spent_usd = ?1, updated_at = ?2 WHERE id = ?3",
        params![total_usd.max(0.0), now(), id],
    )?;
    Ok(())
}

/// Why a recorded `hermes verify` result blocks a merge, if it does. A repository Hermes cannot
/// detect a recipe for (`no-recipe`) is not a failure: there is nothing to run. Pure.
pub fn verify_failure(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    if v.get("ok").and_then(serde_json::Value::as_bool) != Some(false) {
        return None;
    }
    if v.get("error").and_then(serde_json::Value::as_str) == Some("no-recipe") {
        return None;
    }
    let failed = v
        .get("phases")
        .and_then(serde_json::Value::as_array)
        .and_then(|ps| {
            ps.iter()
                .find(|p| p.get("ok").and_then(serde_json::Value::as_bool) == Some(false))
        })
        .map(|p| {
            format!(
                "{} ({})",
                p.get("phase").and_then(|x| x.as_str()).unwrap_or("?"),
                p.get("command").and_then(|x| x.as_str()).unwrap_or("")
            )
        })
        .or_else(|| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| "failed".into());
    Some(format!("hermes verify failed: {failed}"))
}

/// The pull request number in a mission URL, if any.
pub fn pr_number(m: &Mission) -> Option<u64> {
    let url = m.pr_url.as_deref()?;
    url.rsplit('/').next()?.parse().ok()
}

/// The worker reported back. Nothing is approved here; the reviewer decides.
pub fn submit_for_review(id: &str, spent_usd: f64) -> Result<Mission> {
    let c = store::open()?;
    set_status(&c, id, &["running"], "review")?;
    // A new round of work gets a new automatic review.
    let _ = store::kv_delete(&c, &crate::review_agent::auto_key(id));
    c.execute(
        "UPDATE missions SET spent_usd = spent_usd + ?1, updated_at = ?2 WHERE id = ?3",
        params![spent_usd.max(0.0), now(), id],
    )?;
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// A different agent, on a different provider where possible, rules on the work.
pub fn review(id: &str, reviewer: &str, verdict: &str, note: &str) -> Result<Mission> {
    let m = get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    if m.status != "review" {
        bail!(
            "{} is {}; only a mission in review can be judged",
            id,
            m.status
        );
    }
    if reviewer.trim().is_empty() {
        bail!("a review needs a named reviewer");
    }
    if reviewer.trim().eq_ignore_ascii_case(m.agent.trim()) {
        bail!("the reviewer must not be the agent that did the work");
    }
    if note.trim().is_empty() {
        bail!("a review needs a note, approve or reject");
    }
    let c = store::open()?;
    match verdict {
        "approve" => {
            c.execute(
                "UPDATE missions SET status = 'done', reviewer = ?1, review_verdict = 'approve',
                    review_note = ?2, updated_at = ?3 WHERE id = ?4",
                params![reviewer.trim(), note.trim(), now(), id],
            )?;
        }
        "reject" => {
            c.execute(
                "UPDATE missions SET status = 'queued', reviewer = ?1, review_verdict = 'reject',
                    review_note = ?2, updated_at = ?3 WHERE id = ?4",
                params![reviewer.trim(), note.trim(), now(), id],
            )?;
            // A second reject on the same mission is an escalation for a goal.
            crate::goal_loop::count_reject(id);
        }
        other => bail!("verdict must be approve or reject, got '{other}'"),
    }
    let judged = get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    // A finished mission teaches the vault without a human step. The turn takes minutes, so it
    // runs in the background; the `learning.done` key keeps a retry from writing twice. Skipped
    // under unit tests, where a thread would escape the thread-local store and index overrides.
    if verdict == "approve" && !cfg!(test) {
        let done = id.to_string();
        std::thread::spawn(move || {
            let v = crate::vault::Vault::new(crate::settings::vault_path());
            if let Err(e) = crate::learning::on_mission_reviewed(&v, &done) {
                eprintln!("learning: lessons for {done} failed: {e:#}");
            }
        });
    }
    Ok(judged)
}

/// A person in Studio releases a blocked mission back to the queue (an untrusted Notion proposal,
/// or a mission blocked for any other reason once it is resolved).
pub fn unblock(id: &str, note: &str) -> Result<Mission> {
    let c = store::open()?;
    set_status(&c, id, &["blocked"], "queued")?;
    c.execute(
        "UPDATE missions SET review_note = ?1, updated_at = ?2 WHERE id = ?3",
        params![note.trim(), now(), id],
    )?;
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// Agents Studio knows how to address. Until the agent registry exists (Phase C), this list is it.
pub const KNOWN_AGENTS: [&str; 4] = ["hermes", "claude-code", "qoder", "antigravity"];

/// Cancel a mission that has not started work or is waiting on a human: queued or blocked to
/// failed. Running and in-review missions are stopped in Studio, where the run can be ended.
pub fn cancel(id: &str, reason: &str) -> Result<Mission> {
    let c = store::open()?;
    set_status(&c, id, &["queued", "blocked"], "failed")?;
    c.execute(
        "UPDATE missions SET review_note = ?1, updated_at = ?2 WHERE id = ?3",
        params![reason.trim(), now(), id],
    )?;
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// Blocked and failed are the two states a human has to resolve.
pub fn block(id: &str, reason: &str, failed: bool) -> Result<Mission> {
    let c = store::open()?;
    let target = if failed { "failed" } else { "blocked" };
    set_status(&c, id, &["queued", "running", "review"], target)?;
    c.execute(
        "UPDATE missions SET review_note = ?1, updated_at = ?2 WHERE id = ?3",
        params![reason.trim(), now(), id],
    )?;
    get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))
}

/// Everything that must hold before a gatekeeper merges the pull request.
/// Returns the reasons it is not allowed, so the UI can say why.
pub fn merge_blockers(m: &Mission, ci_green: bool, allowlisted: bool) -> Vec<String> {
    let mut out = Vec::new();
    if m.status != "done" {
        out.push(format!("mission is {}", m.status));
    }
    if m.review_verdict.as_deref() != Some("approve") {
        out.push("no approving review".into());
    }
    if let Some(r) = m.reviewer.as_deref() {
        if r.eq_ignore_ascii_case(m.agent.as_str()) {
            out.push("reviewer is the implementer".into());
        }
    } else {
        out.push("no reviewer".into());
    }
    if !ci_green {
        out.push("CI is not green".into());
    }
    if m.budget_usd > 0.0 && m.spent_usd > m.budget_usd {
        out.push(format!("spent ${:.2} of ${:.2}", m.spent_usd, m.budget_usd));
    }
    if !allowlisted {
        out.push("repository is not on the auto-merge allow list".into());
    }
    if let Some(reason) = m.verify.as_deref().and_then(verify_failure) {
        out.push(reason);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_db<T>(f: impl FnOnce() -> T) -> T {
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let out = f();
        crate::store::set_test_db(None);
        out
    }

    #[test]
    fn the_pull_request_comes_from_the_completion_summary_not_the_packet() {
        // The shape of a handoff task that recall filled with another repository's pull request.
        let body = "# TASK-011: Handoff\n\n## Instructions\nHANDOFF from claude-code.\n\n\
## From shared memory (recall)\n- 03_Memory/Coordination.md:61 PR (qoder) https://github.com/o/studio/pull/80 phase-e/\n\n\
## Activity log\n- completed by hermes\n\n## Completion summary\n> [!success] Completed by **hermes**\n\n\
Opened PR https://github.com/o/sandbox/pull/7 (the only PR for this branch). Not merged.\n";
        let s = completion_summary(body).unwrap();
        assert_eq!(
            pull_url(&s).as_deref(),
            Some("https://github.com/o/sandbox/pull/7")
        );
        // Without a summary, nothing is taken from the rest of the body.
        let no_summary = "## Instructions\nsee https://github.com/o/studio/pull/80\n";
        assert!(completion_summary(no_summary).is_none());
        // A summary with no pull request does not fall back to the body either.
        let empty = "## Instructions\nhttps://github.com/o/studio/pull/80\n\n## Completion summary\n> done\n\nNo PR.\n";
        assert!(completion_summary(empty)
            .and_then(|s| pull_url(&s))
            .is_none());
        // Only the text up to the next heading belongs to the summary.
        let later = "## Completion summary\n\nshipped\n\n## Notes\nhttps://github.com/o/x/pull/1\n";
        assert!(completion_summary(later)
            .and_then(|s| pull_url(&s))
            .is_none());
    }

    fn make() -> Mission {
        create(
            "agentos-studio",
            "Add the missions board",
            "Board lists missions with status and review verdict",
            "implementer",
            "hermes",
            "claude-subscription",
            "",
            0.0,
        )
        .unwrap()
    }

    #[test]
    fn a_failed_hermes_verify_blocks_merge_and_no_recipe_does_not() {
        assert_eq!(
            verify_failure(r#"{"ok": false, "error": "no-recipe"}"#),
            None
        );
        assert_eq!(verify_failure(r#"{"ok": true, "phases": []}"#), None);
        let failed = r#"{"ok": false, "phases": [{"phase": "build", "command": "npm run build", "ok": true},
            {"phase": "test", "command": "npm test", "ok": false}]}"#;
        assert_eq!(
            verify_failure(failed).as_deref(),
            Some("hermes verify failed: test (npm test)")
        );
        with_db(|| {
            let mut m = make();
            m.verify = Some(failed.into());
            let b = merge_blockers(&m, true, true);
            assert!(
                b.iter().any(|r| r.starts_with("hermes verify failed")),
                "{b:?}"
            );
        });
    }

    #[test]
    fn every_bus_task_names_its_repository() {
        with_db(|| {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(d.path().join("README.md"), "# r").unwrap();
            let mut m = make();
            let body = bus_instructions(&m, d.path(), None, "");
            assert!(body.contains("Acceptance criteria:"));
            assert!(
                body.contains("constitution gate"),
                "the worker is told the gate's rules"
            );
            assert!(body.contains(&m.id));
            assert!(
                !body.contains("Conventions from earlier reviews"),
                "no conventions, no section"
            );
            // The worker resolves its folder from exactly this line.
            assert_eq!(crate::bus::repo_from_body(&body).unwrap(), d.path());
            assert_eq!(crate::bus::workdir_from_body(&body).unwrap(), d.path());
            // With a worktree the worker runs there; the repository line still names the checkout.
            let wt = d.path().join("wt");
            std::fs::create_dir_all(&wt).unwrap();
            m.model = "claude-opus-5-5".into();
            let body = bus_instructions(
                &m,
                d.path(),
                Some(&wt),
                "- Use the retry helper: call it before any request.",
            );
            assert_eq!(crate::bus::workdir_from_body(&body).unwrap(), wt);
            assert_eq!(crate::bus::repo_from_body(&body).unwrap(), d.path());
            assert_eq!(
                crate::bus::model_from_body(&body).as_deref(),
                Some("claude-opus-5-5")
            );
            assert!(body.contains(&format!("Branch: {}", crate::worktree::branch_for(&m))));
            assert!(
                body.contains("Conventions from earlier reviews in this repository:")
                    && body.contains("Use the retry helper"),
                "the repository conventions reach the worker: {body}"
            );
            let ctx = context_files(d.path());
            assert_eq!(ctx[0], "03_Memory/SYSTEM_CONTEXT.md");
            assert!(ctx.iter().any(|c| c.ends_with("README.md")));
            assert!(
                !ctx.iter().any(|c| c.ends_with("CLAUDE.md")),
                "only files that exist"
            );

            m.review_verdict = Some("reject".into());
            m.review_note = Some("cover the empty case".into());
            assert!(bus_instructions(&m, d.path(), None, "").contains("cover the empty case"));
        });
    }

    #[test]
    fn a_mission_outside_a_known_project_does_not_start() {
        with_db(|| {
            let d = tempfile::tempdir().unwrap();
            let v = crate::vault::Vault::new(d.path().join("vault"));
            v.scaffold().unwrap();
            let m = create(
                "no-such-project-7f3a",
                "g",
                "a",
                "implementer",
                "claude-code",
                "",
                "",
                0.0,
            )
            .unwrap();
            let e = start(&v, &m.id).unwrap_err().to_string();
            assert!(e.contains("not a known project"), "{e}");
            assert_eq!(get(&m.id).unwrap().unwrap().status, "queued");
            assert!(
                v.fetch_pending_tasks("all").unwrap().is_empty(),
                "no task written"
            );
        });
    }

    #[test]
    fn a_decision_mission_is_marked_only_before_it_starts() {
        with_db(|| {
            let m = make();
            assert_eq!(kind(&m.id), KIND_WORK);
            set_kind(&m.id, KIND_DECISION).unwrap();
            assert_eq!(kind(&m.id), KIND_DECISION);
            assert!(set_kind(&m.id, "vote").is_err());
            dispatch(&m.id, "council:M").unwrap();
            assert!(set_kind(&m.id, KIND_WORK).is_err(), "not once it runs");
            assert_eq!(kind("M-999"), KIND_WORK);
        });
    }

    #[test]
    fn deleting_a_mission_never_reuses_an_id() {
        with_db(|| {
            let a = make();
            let b = make();
            assert!(delete(&a.id).is_ok());
            let c = make();
            assert_ne!(c.id, b.id, "a row count would hand out {} again", b.id);
            let r = dispatch(&c.id, "TASK-009").unwrap();
            assert!(delete(&r.id).is_err(), "a running mission is not deleted");
        });
    }

    #[test]
    fn a_mission_needs_acceptance_criteria() {
        with_db(|| {
            assert!(create("r", "g", "  ", "implementer", "hermes", "x", "", 0.0).is_err());
            assert!(create("r", "g", "ok", "wizard", "hermes", "x", "", 0.0).is_err());
            assert!(create("", "g", "ok", "implementer", "hermes", "x", "", 0.0).is_err());
        });
    }

    #[test]
    fn only_an_approved_review_reaches_done() {
        with_db(|| {
            let m = make();
            assert_eq!(m.status, "queued");
            let m = dispatch(&m.id, "TASK-001").unwrap();
            assert_eq!(m.status, "running");
            let m = submit_for_review(&m.id, 0.0).unwrap();
            assert_eq!(m.status, "review");

            // The implementer cannot approve its own work.
            assert!(review(&m.id, "hermes", "approve", "looks fine").is_err());
            // A bare verdict with no note is refused.
            assert!(review(&m.id, "claude-code", "approve", "  ").is_err());

            let done = review(&m.id, "claude-code", "approve", "diff matches acceptance").unwrap();
            assert_eq!(done.status, "done");
            assert_eq!(done.review_verdict.as_deref(), Some("approve"));
        });
    }

    #[test]
    fn rejection_returns_to_the_queue_with_the_note() {
        with_db(|| {
            let m = dispatch(&make().id, "TASK-002").unwrap();
            let m = submit_for_review(&m.id, 1.5).unwrap();
            let m = review(
                &m.id,
                "qoder",
                "reject",
                "tests do not cover the empty case",
            )
            .unwrap();
            assert_eq!(m.status, "queued");
            assert_eq!(m.review_verdict.as_deref(), Some("reject"));
            assert!(m.review_note.unwrap().contains("empty case"));
            assert_eq!(m.spent_usd, 1.5);
        });
    }

    #[test]
    fn merge_policy_names_every_reason() {
        with_db(|| {
            let m = dispatch(&make().id, "TASK-003").unwrap();
            let m = submit_for_review(&m.id, 0.0).unwrap();
            let blockers = merge_blockers(&m, true, true);
            assert!(blockers.iter().any(|b| b.contains("mission is review")));

            let done = review(&m.id, "claude-code", "approve", "ok").unwrap();
            assert!(merge_blockers(&done, true, true).is_empty());
            let blocked = merge_blockers(&done, false, false);
            assert_eq!(blocked.len(), 2, "CI and allow list: {blocked:?}");
        });
    }
}
