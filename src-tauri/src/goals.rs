//! Goals: the owner's outcome, how Studio knows it is finished, and how much it may do alone.
//!
//! A goal is one sentence plus runnable done checks (`command`, `url`, `file`), the repositories
//! it touches, a metered budget, an optional deadline and an autonomy level. Its missions carry
//! `missions.goal_id`; missions without one are the standalone missions Studio always had.
//! `docs/GOALS.md` is the design.
//!
//! ```text
//! planning -> awaiting -> running -> verifying -> done
//!        \-> running             \-> running (a fix mission)
//!                       \-> paused  \-> failed
//! ```

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::missions::{self, Mission};
use crate::store;

/// Watch: plan and every mission wait for a click. Ask on risk (the default): runs everything and
/// asks only on the escalation list. Full: also merges outside the allow list.
pub const LEVELS: [&str; 3] = ["watch", "ask", "full"];
pub const DEFAULT_LEVEL: &str = "ask";
pub const STATUSES: [&str; 7] = [
    "planning",
    "awaiting",
    "running",
    "verifying",
    "done",
    "failed",
    "paused",
];
pub const CHECK_KINDS: [&str; 3] = ["command", "url", "file"];

/// One runnable done check. `repo` says where a command runs or a file must exist; empty means
/// the goal's first repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Check {
    pub kind: String,
    pub target: String,
    #[serde(default)]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Goal {
    pub id: String,
    pub outcome: String,
    pub repos: Vec<String>,
    pub checks: Vec<Check>,
    pub budget_usd: f64,
    pub deadline: Option<String>,
    pub autonomy: String,
    pub status: String,
    /// Why the goal is where it is, in one line ("waiting for plan approval", the last error).
    pub note: String,
    /// The completion report, once the goal is done or failed.
    pub report: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What the form sends.
#[derive(Debug, Clone, Deserialize)]
pub struct NewGoal {
    pub outcome: String,
    pub repos: Vec<String>,
    pub checks: Vec<Check>,
    #[serde(default)]
    pub budget_usd: f64,
    #[serde(default)]
    pub deadline: Option<String>,
    /// Empty: the strictest default of the goal's repositories.
    #[serde(default)]
    pub autonomy: Option<String>,
}

/// A check the owner can run and see pass or fail: a known kind and a target that means
/// something for it. Pure.
pub fn validate_check(c: &Check) -> Result<()> {
    let t = c.target.trim();
    if !CHECK_KINDS.contains(&c.kind.as_str()) {
        bail!("unknown check kind '{}'; use command, url or file", c.kind);
    }
    if t.is_empty() {
        bail!("a {} check needs a target", c.kind);
    }
    match c.kind.as_str() {
        "url" if !(t.starts_with("http://") || t.starts_with("https://")) => {
            bail!("a url check must start with http:// or https://, got '{t}'")
        }
        "file" => {
            let p = std::path::Path::new(t);
            if p.is_absolute()
                || p.components()
                    .any(|x| matches!(x, std::path::Component::ParentDir))
            {
                bail!("a file check is a path inside the repository, got '{t}'");
            }
        }
        _ => {}
    }
    Ok(())
}

/// The order of caution: a lower rank asks more. Pure.
pub fn rank(level: &str) -> usize {
    LEVELS.iter().position(|l| *l == level).unwrap_or(1)
}

/// The most cautious of several levels; an empty list is the default. Pure.
pub fn strictest<'a>(levels: impl IntoIterator<Item = &'a str>) -> String {
    levels
        .into_iter()
        .min_by_key(|l| rank(l))
        .unwrap_or(DEFAULT_LEVEL)
        .to_string()
}

/// A repository's default autonomy level (kv `autonomy.repo.<slug>`), Ask on risk when unset.
pub fn repo_default(repo: &str) -> String {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, &repo_key(repo)).ok().flatten())
        .filter(|l| LEVELS.contains(&l.as_str()))
        .unwrap_or_else(|| DEFAULT_LEVEL.into())
}

pub fn set_repo_default(repo: &str, level: &str) -> Result<()> {
    if !LEVELS.contains(&level) {
        bail!("autonomy must be watch, ask or full, got '{level}'");
    }
    if repo.trim().is_empty() {
        bail!("a repository is needed");
    }
    let c = store::open()?;
    store::kv_set(&c, &repo_key(repo), level)
}

fn repo_key(repo: &str) -> String {
    format!("autonomy.repo.{}", repo.trim().to_lowercase())
}

/// What happens when a goal's mission has an approved pull request. Pure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum MergeDecision {
    /// The gate decides on its own.
    Merge,
    /// The repository is not on the allow list: ask the owner.
    Ask,
    /// Watch: the owner merges with a click.
    Wait,
}

pub fn merge_decision(level: &str, allowlisted: bool) -> MergeDecision {
    match level {
        "watch" => MergeDecision::Wait,
        "full" => MergeDecision::Merge,
        _ if allowlisted => MergeDecision::Merge,
        _ => MergeDecision::Ask,
    }
}

/// At Watch the plan is shown before anything starts. Pure.
pub fn plan_needs_approval(level: &str) -> bool {
    level == "watch"
}

/// At Watch each mission waits for its Start click. Pure.
pub fn missions_wait_for_click(level: &str) -> bool {
    level == "watch"
}

fn now() -> i64 {
    store::now()
}

fn next_id(c: &Connection) -> Result<String> {
    let mut st = c.prepare("SELECT id FROM goals")?;
    let max = st
        .query_map([], |r| r.get::<_, String>(0))?
        .flatten()
        .filter_map(|id| id.strip_prefix("G-").and_then(|n| n.parse::<i64>().ok()))
        .max()
        .unwrap_or(0);
    Ok(format!("G-{:03}", max + 1))
}

/// Validate and store a goal. It starts in `planning`; the planner takes it from there.
pub fn create(input: &NewGoal) -> Result<Goal> {
    let outcome = input.outcome.trim();
    if outcome.is_empty() {
        bail!("a goal needs an outcome");
    }
    let repos: Vec<String> = input
        .repos
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    if repos.is_empty() {
        bail!("a goal needs at least one repository");
    }
    if input.checks.is_empty() {
        bail!(
            "a goal needs at least one done check (a command, a URL or a file); without one Studio cannot know it is finished"
        );
    }
    for c in &input.checks {
        validate_check(c)?;
    }
    if input.budget_usd < 0.0 {
        bail!("budget cannot be negative");
    }
    let autonomy = match input.autonomy.as_deref().map(str::trim) {
        Some(l) if !l.is_empty() => {
            if !LEVELS.contains(&l) {
                bail!("autonomy must be watch, ask or full, got '{l}'");
            }
            l.to_string()
        }
        _ => {
            let defaults: Vec<String> = repos.iter().map(|r| repo_default(r)).collect();
            strictest(defaults.iter().map(String::as_str))
        }
    };
    let deadline = input
        .deadline
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(String::from);
    let c = store::open()?;
    let id = next_id(&c)?;
    let t = now();
    c.execute(
        "INSERT INTO goals (id, outcome, repos, checks, budget_usd, deadline, autonomy, status,
                            note, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'planning', '', ?8, ?8)",
        params![
            id,
            outcome,
            serde_json::to_string(&repos)?,
            serde_json::to_string(&input.checks)?,
            input.budget_usd,
            deadline,
            autonomy,
            t
        ],
    )?;
    get(&id)?.ok_or_else(|| anyhow::anyhow!("goal vanished after insert"))
}

const COLUMNS: &str = "id, outcome, repos, checks, budget_usd, deadline, autonomy, status, note,
                       report, created_at, updated_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Goal> {
    let repos: String = r.get(2)?;
    let checks: String = r.get(3)?;
    Ok(Goal {
        id: r.get(0)?,
        outcome: r.get(1)?,
        repos: serde_json::from_str(&repos).unwrap_or_default(),
        checks: serde_json::from_str(&checks).unwrap_or_default(),
        budget_usd: r.get(4)?,
        deadline: r.get(5)?,
        autonomy: r.get(6)?,
        status: r.get(7)?,
        note: r.get(8)?,
        report: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

pub fn get(id: &str) -> Result<Option<Goal>> {
    let c = store::open()?;
    Ok(c.query_row(
        &format!("SELECT {COLUMNS} FROM goals WHERE id = ?1"),
        params![id],
        row,
    )
    .optional()?)
}

/// Active goals first, newest first within a group.
pub fn list(limit: usize) -> Result<Vec<Goal>> {
    let c = store::open()?;
    let mut st = c.prepare(&format!(
        "SELECT {COLUMNS} FROM goals
         ORDER BY CASE status WHEN 'awaiting' THEN 0 WHEN 'running' THEN 1 WHEN 'verifying' THEN 2
                              WHEN 'planning' THEN 3 WHEN 'paused' THEN 4 ELSE 5 END,
                  updated_at DESC
         LIMIT ?1"
    ))?;
    let rows = st.query_map(params![limit as i64], row)?;
    Ok(rows.flatten().collect())
}

/// Move a goal between statuses, only from the states given. The note says why.
pub fn set_status(id: &str, from: &[&str], to: &str, note: &str) -> Result<Goal> {
    if !STATUSES.contains(&to) {
        bail!("unknown goal status '{to}'");
    }
    let g = get(id)?.ok_or_else(|| anyhow::anyhow!("no goal '{id}'"))?;
    if !from.contains(&g.status.as_str()) {
        bail!("{id} is {}, cannot move to {to}", g.status);
    }
    let c = store::open()?;
    c.execute(
        "UPDATE goals SET status = ?1, note = ?2, updated_at = ?3 WHERE id = ?4",
        params![to, note.trim(), now(), id],
    )?;
    get(id)?.ok_or_else(|| anyhow::anyhow!("no goal '{id}'"))
}

/// Replace the one-line note without changing the status.
pub fn set_note(id: &str, note: &str) -> Result<()> {
    let c = store::open()?;
    c.execute(
        "UPDATE goals SET note = ?1, updated_at = ?2 WHERE id = ?3",
        params![note.trim(), now(), id],
    )?;
    Ok(())
}

pub fn set_report(id: &str, report: &str) -> Result<()> {
    let c = store::open()?;
    c.execute(
        "UPDATE goals SET report = ?1, updated_at = ?2 WHERE id = ?3",
        params![report, now(), id],
    )?;
    Ok(())
}

/// Set the goal's metered budget (an escalation's Raise doubles it).
pub fn set_budget(id: &str, budget_usd: f64) -> Result<()> {
    if budget_usd < 0.0 {
        bail!("budget cannot be negative");
    }
    let c = store::open()?;
    c.execute(
        "UPDATE goals SET budget_usd = ?1, updated_at = ?2 WHERE id = ?3",
        params![budget_usd, now(), id],
    )?;
    Ok(())
}

/// Change how much the goal may do alone, while it is still open.
pub fn set_autonomy(id: &str, level: &str) -> Result<Goal> {
    if !LEVELS.contains(&level) {
        bail!("autonomy must be watch, ask or full, got '{level}'");
    }
    let c = store::open()?;
    let n = c.execute(
        "UPDATE goals SET autonomy = ?1, updated_at = ?2
         WHERE id = ?3 AND status NOT IN ('done', 'failed')",
        params![level, now(), id],
    )?;
    if n == 0 {
        bail!("{id} is finished or unknown, so its autonomy cannot change");
    }
    get(id)?.ok_or_else(|| anyhow::anyhow!("no goal '{id}'"))
}

/// Put a mission under a goal.
pub fn attach(goal_id: &str, mission_id: &str) -> Result<()> {
    let c = store::open()?;
    c.execute(
        "UPDATE missions SET goal_id = ?1 WHERE id = ?2",
        params![goal_id, mission_id],
    )?;
    Ok(())
}

/// The goal a mission belongs to, if any.
pub fn goal_of(mission_id: &str) -> Option<String> {
    let c = store::open().ok()?;
    c.query_row(
        "SELECT goal_id FROM missions WHERE id = ?1",
        params![mission_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .ok()
    .flatten()
    .flatten()
}

/// The missions under a goal, oldest first (the plan's order).
pub fn missions_of(goal_id: &str) -> Result<Vec<Mission>> {
    let c = store::open()?;
    let mut st = c.prepare("SELECT id FROM missions WHERE goal_id = ?1 ORDER BY created_at, id")?;
    let ids: Vec<String> = st
        .query_map(params![goal_id], |r| r.get::<_, String>(0))?
        .flatten()
        .collect();
    let mut out = Vec::new();
    for id in ids {
        if let Some(m) = missions::get(&id)? {
            out.push(m);
        }
    }
    Ok(out)
}

/// Is any goal running or verifying? The scheduler's minute pass runs for those even with
/// Automations off.
pub fn any_active() -> bool {
    store::open()
        .ok()
        .and_then(|c| {
            c.query_row(
                "SELECT 1 FROM goals WHERE status IN ('running', 'verifying') LIMIT 1",
                [],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
        })
        .is_some()
}

/// Metered spend across a goal's missions. Pure.
pub fn spent(missions: &[Mission]) -> f64 {
    missions.iter().map(|m| m.spent_usd).sum()
}

/// Remove a goal that has nothing in flight, with its missions. Used by live tests and by
/// Delete on a finished goal; refuses while any mission is running or in review.
pub fn delete(id: &str) -> Result<()> {
    let ms = missions_of(id)?;
    if let Some(m) = ms
        .iter()
        .find(|m| m.status == "running" || m.status == "review")
    {
        bail!(
            "{} is {}; stop or judge it before deleting {id}",
            m.id,
            m.status
        );
    }
    for m in &ms {
        missions::delete(&m.id)?;
    }
    crate::escalation::delete_for_goal(id)?;
    let c = store::open()?;
    c.execute("DELETE FROM goals WHERE id = ?1", params![id])?;
    c.execute(
        "DELETE FROM kv WHERE key LIKE ?1",
        params![format!("goal.%.{id}")],
    )?;
    Ok(())
}

/// Where the goal's vault note lives.
pub fn note_rel(id: &str) -> String {
    format!("01_Projects/Goals/{id}.md")
}

fn level_label(level: &str) -> &'static str {
    match level {
        "watch" => "Watch",
        "full" => "Full",
        _ => "Ask on risk",
    }
}

/// The goal as a vault note: frontmatter for queries, the outcome, the checks and the missions
/// with links to their pull requests. Pure.
pub fn render_note(g: &Goal, ms: &[Mission]) -> String {
    let mut s = String::new();
    s.push_str("---\n");
    s.push_str("type: goal\n");
    s.push_str(&format!("id: {}\n", g.id));
    s.push_str(&format!("status: {}\n", g.status));
    s.push_str(&format!("autonomy: {}\n", g.autonomy));
    s.push_str(&format!("repos: [{}]\n", g.repos.join(", ")));
    s.push_str(&format!("budget_usd: {:.2}\n", g.budget_usd));
    if let Some(d) = &g.deadline {
        s.push_str(&format!("deadline: {d}\n"));
    }
    s.push_str(&format!(
        "updated: {}\n",
        chrono::DateTime::from_timestamp(g.updated_at, 0)
            .map(|t| t.to_rfc3339())
            .unwrap_or_default()
    ));
    s.push_str("---\n\n");
    s.push_str(&format!("# {}: {}\n\n", g.id, g.outcome));
    s.push_str(&format!(
        "Status: **{}**. Autonomy: {}. Spent ${:.2}",
        g.status,
        level_label(&g.autonomy),
        spent(ms)
    ));
    if g.budget_usd > 0.0 {
        s.push_str(&format!(" of ${:.2}", g.budget_usd));
    }
    s.push_str(".\n");
    if !g.note.is_empty() {
        s.push_str(&format!("\n{}\n", g.note));
    }
    s.push_str("\n## Definition of done\n\n");
    for c in &g.checks {
        let repo = c.repo.as_deref().unwrap_or("");
        let at = if repo.is_empty() {
            String::new()
        } else {
            format!(" in {repo}")
        };
        s.push_str(&format!("- {}: `{}`{at}\n", c.kind, c.target));
    }
    s.push_str("\n## Missions\n\n");
    if ms.is_empty() {
        s.push_str("None yet.\n");
    }
    for m in ms {
        let pr = m
            .pr_url
            .as_deref()
            .map(|u| format!(", [pull request]({u})"))
            .unwrap_or_default();
        s.push_str(&format!(
            "- {} ({}, {}): {}{pr}\n",
            m.id, m.status, m.agent, m.goal
        ));
    }
    if let Some(r) = &g.report {
        s.push_str("\n## Report\n\n");
        s.push_str(r.trim());
        s.push('\n');
    }
    s
}

/// Write or refresh the goal's vault note. A failure is reported, never fatal to the goal.
pub fn write_note(v: &crate::vault::Vault, id: &str) -> Result<()> {
    let g = get(id)?.ok_or_else(|| anyhow::anyhow!("no goal '{id}'"))?;
    let ms = missions_of(id)?;
    v.write_note(&note_rel(id), &render_note(&g, &ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_db<T>(f: impl FnOnce() -> T) -> T {
        let d = tempfile::tempdir().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        let out = f();
        store::set_test_db(None);
        out
    }

    fn check(kind: &str, target: &str) -> Check {
        Check {
            kind: kind.into(),
            target: target.into(),
            repo: None,
        }
    }

    fn new_goal() -> NewGoal {
        NewGoal {
            outcome: "Add a probe endpoint and document it".into(),
            repos: vec!["agentos-sandbox-1".into()],
            checks: vec![check("command", "npm test"), check("file", "README.md")],
            budget_usd: 0.0,
            deadline: None,
            autonomy: None,
        }
    }

    #[test]
    fn checks_must_be_runnable() {
        assert!(validate_check(&check("command", "npm test")).is_ok());
        assert!(validate_check(&check("url", "https://example.com/probe")).is_ok());
        assert!(validate_check(&check("file", "docs/PROBE.md")).is_ok());
        assert!(validate_check(&check("url", "example.com")).is_err());
        assert!(validate_check(&check("file", "../outside.md")).is_err());
        assert!(validate_check(&check("file", "C:/abs.md")).is_err());
        assert!(validate_check(&check("command", "  ")).is_err());
        assert!(validate_check(&check("vibes", "x")).is_err());
    }

    #[test]
    fn the_strictest_level_wins_and_watch_never_merges_alone() {
        assert_eq!(strictest(["full", "ask"]), "ask");
        assert_eq!(strictest(["full", "watch", "ask"]), "watch");
        assert_eq!(strictest(Vec::<&str>::new()), "ask");
        assert_eq!(merge_decision("watch", true), MergeDecision::Wait);
        assert_eq!(merge_decision("ask", true), MergeDecision::Merge);
        assert_eq!(merge_decision("ask", false), MergeDecision::Ask);
        assert_eq!(merge_decision("full", false), MergeDecision::Merge);
        assert!(plan_needs_approval("watch") && !plan_needs_approval("ask"));
        assert!(missions_wait_for_click("watch") && !missions_wait_for_click("full"));
    }

    #[test]
    fn a_goal_needs_an_outcome_a_repository_and_a_done_check() {
        with_db(|| {
            let mut g = new_goal();
            g.checks.clear();
            assert!(create(&g).unwrap_err().to_string().contains("done check"));
            let mut g = new_goal();
            g.repos = vec!["  ".into()];
            assert!(create(&g).is_err());
            let mut g = new_goal();
            g.outcome = " ".into();
            assert!(create(&g).is_err());
            let mut g = new_goal();
            g.autonomy = Some("yolo".into());
            assert!(create(&g).is_err());
        })
    }

    #[test]
    fn a_new_goal_takes_the_strictest_repository_default() {
        with_db(|| {
            set_repo_default("agentos-sandbox-1", "full").unwrap();
            set_repo_default("agentos-sandbox-2", "watch").unwrap();
            let g = create(&new_goal()).unwrap();
            assert_eq!((g.id.as_str(), g.autonomy.as_str()), ("G-001", "full"));
            assert_eq!(g.status, "planning");
            let mut two = new_goal();
            two.repos.push("agentos-sandbox-2".into());
            assert_eq!(create(&two).unwrap().autonomy, "watch");
            let mut picked = new_goal();
            picked.autonomy = Some("ask".into());
            let g3 = create(&picked).unwrap();
            assert_eq!((g3.id.as_str(), g3.autonomy.as_str()), ("G-003", "ask"));
            assert_eq!(repo_default("unknown-repo"), "ask");
            assert!(set_repo_default("x", "maybe").is_err());
        })
    }

    #[test]
    fn status_moves_only_from_the_states_given_and_finished_goals_keep_their_level() {
        with_db(|| {
            let g = create(&new_goal()).unwrap();
            assert!(set_status(&g.id, &["running"], "done", "").is_err());
            let g = set_status(&g.id, &["planning"], "running", "planned 2 missions").unwrap();
            assert_eq!(
                (g.status.as_str(), g.note.as_str()),
                ("running", "planned 2 missions")
            );
            assert_eq!(set_autonomy(&g.id, "watch").unwrap().autonomy, "watch");
            set_status(&g.id, &["running"], "done", "all checks passed").unwrap();
            assert!(set_autonomy(&g.id, "full").is_err());
            assert!(set_status(&g.id, &["done"], "nonsense", "").is_err());
        })
    }

    #[test]
    fn missions_belong_to_a_goal_and_the_note_lists_them() {
        with_db(|| {
            let g = create(&new_goal()).unwrap();
            let m = missions::create(
                "agentos-sandbox-1",
                "Add GET /probe",
                "npm test passes",
                "implementer",
                "auto",
                "",
                "",
                0.0,
            )
            .unwrap();
            let lone = missions::create(
                "agentos-sandbox-1",
                "Unrelated",
                "x",
                "implementer",
                "hermes",
                "",
                "",
                0.0,
            )
            .unwrap();
            attach(&g.id, &m.id).unwrap();
            assert_eq!(goal_of(&m.id).as_deref(), Some(g.id.as_str()));
            assert_eq!(goal_of(&lone.id), None);
            let ms = missions_of(&g.id).unwrap();
            assert_eq!(ms.len(), 1);
            let note = render_note(&g, &ms);
            assert!(note.contains("type: goal") && note.contains("status: planning"));
            assert!(note.contains("- command: `npm test`"));
            assert!(note.contains("M-001 (queued, auto): Add GET /probe"));
            assert!(note.contains("Autonomy: Ask on risk"));

            let d = tempfile::tempdir().unwrap();
            let v = crate::vault::Vault::new(d.path());
            write_note(&v, &g.id).unwrap();
            let on_disk = std::fs::read_to_string(d.path().join(note_rel(&g.id))).unwrap();
            assert!(on_disk.contains("# G-001: Add a probe endpoint"));

            assert!(!any_active(), "a planning goal is not active yet");
            set_status(&g.id, &["planning"], "running", "").unwrap();
            assert!(any_active());
            set_status(&g.id, &["running"], "failed", "test").unwrap();
            assert!(!any_active());
            delete(&g.id).unwrap();
            assert!(get(&g.id).unwrap().is_none());
            assert!(
                missions::get(&m.id).unwrap().is_none(),
                "its mission went with it"
            );
            assert!(
                missions::get(&lone.id).unwrap().is_some(),
                "standalone missions stay"
            );
        })
    }
}
