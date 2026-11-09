//! The planner: a goal becomes missions through Hermes' own decomposer.
//!
//! Studio writes the goal as a triage task and runs `hermes kanban decompose --json`, which asks
//! Hermes' auxiliary model for a task graph (2 to 6 tasks with self-contained specs and
//! dependencies). It runs against a private kanban database (`HERMES_KANBAN_DB`) under Studio's
//! data folder: the gateway's dispatcher only scans `kanban/boards/`, so nothing in a plan can start
//! as a Hermes worker outside Studio's missions, review and merge gate. On a normal board the
//! dispatcher promotes and claims decomposed children within a minute, even with
//! `auto_promote_children: false`, because its tick recomputes readiness itself.
//!
//! Each planned task becomes one mission under the goal (agent `auto`, so `assign.rs` picks the
//! agent from quota and outcomes); its dependencies are kept in kv `mission.after.<id>`, and the
//! private database is deleted once the plan is mirrored. `decompose` with `fanout: false` is
//! `specify`, so one call covers both.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::goals::{self, Goal};
use crate::missions::Mission;
use crate::store;

const DECOMPOSE_LIMIT: Duration = Duration::from_secs(300);
const CLI_LIMIT: Duration = Duration::from_secs(90);

/// One task of a plan, as Hermes wrote it. `parents` are the keys of tasks that must finish first.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedTask {
    pub key: String,
    pub title: String,
    pub body: String,
    pub parents: Vec<String>,
}

/// The private kanban database for one goal's plan.
pub fn plan_db(goal_id: &str) -> PathBuf {
    store::studio_dir()
        .join("plans")
        .join(format!("{goal_id}.db"))
}

/// The triage task the decomposer reads: the outcome, where the work happens and how it will be
/// judged, plus the rules a Studio mission needs (one pull request in one named repository). Pure.
pub fn triage_body(g: &Goal, folders: &[(String, Option<PathBuf>)]) -> String {
    let mut b = format!("Outcome: {}\n\nRepositories:\n", g.outcome.trim());
    for (slug, dir) in folders {
        match dir {
            Some(d) => b.push_str(&format!("- {slug} ({})\n", d.display())),
            None => b.push_str(&format!("- {slug}\n")),
        }
    }
    b.push_str("\nDefinition of done (checked by Studio after the last merge):\n");
    for c in &g.checks {
        let at = c
            .repo
            .as_deref()
            .filter(|r| !r.is_empty())
            .map(|r| format!(" in {r}"))
            .unwrap_or_default();
        b.push_str(&format!("- {}: {}{at}\n", c.kind, c.target));
    }
    b.push_str(
        "\nRules for the plan:\n\
- Each task is one pull request in ONE of the repositories above. Start each task body with a line \
`Repository: <slug>` naming it.\n\
- End each task body with a section `Acceptance criteria:` listing what a reviewer checks.\n\
- Tasks that touch the same file must depend on each other, so two pull requests never edit the \
same lines at once.\n\
- Workers do not merge: AgentOS Studio reviews and merges each pull request.\n",
    );
    b
}

/// The repository a task names on its `Repository:` line, if it is one of the goal's. Pure.
pub fn task_repo(body: &str, repos: &[String]) -> Option<String> {
    body.lines().find_map(|l| {
        let v = l
            .trim()
            .trim_start_matches(['-', '*', ' '])
            .strip_prefix("Repository:")?
            .trim();
        let first = v
            .split(|c: char| c.is_whitespace() || c == '(' || c == ',')
            .next()
            .unwrap_or("")
            .trim_matches(['`', '"', '\'', '.']);
        repos
            .iter()
            .find(|r| r.eq_ignore_ascii_case(first))
            .cloned()
    })
}

/// The text under an `Acceptance criteria` heading or label, up to the next heading. Pure.
pub fn acceptance_of(body: &str) -> Option<String> {
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.iter().position(|l| {
        let t = l
            .trim()
            .trim_start_matches('#')
            .trim()
            .trim_end_matches(':')
            .to_lowercase();
        t == "acceptance criteria" || t == "acceptance"
    })?;
    let rest = &lines[start + 1..];
    let end = rest
        .iter()
        .position(|l| l.trim_start().starts_with('#'))
        .unwrap_or(rest.len());
    let text = rest[..end].join("\n").trim().to_string();
    Some(text).filter(|t| !t.is_empty())
}

/// A `kanban show --json` document as a planned task. Only parents inside the plan count; the
/// root the children report to is not a dependency. Pure.
pub fn parse_show(json: &str, in_plan: &[String]) -> Result<PlannedTask> {
    let v: Value = serde_json::from_str(json_object(json)).context("kanban show --json")?;
    let t = v.get("task").unwrap_or(&v);
    let key = t
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("a planned task has no id"))?
        .to_string();
    let title = t
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if title.is_empty() {
        bail!("planned task {key} has no title");
    }
    let body = t
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let parents = v
        .get("parents")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_str().or_else(|| p.get("id").and_then(Value::as_str)))
                .filter(|p| in_plan.iter().any(|k| k == p))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    Ok(PlannedTask {
        key,
        title,
        body,
        parents,
    })
}

/// The JSON object in CLI output that may carry warning lines around it. Pure.
fn json_object(text: &str) -> &str {
    match (text.find('{'), text.rfind('}')) {
        (Some(a), Some(b)) if b > a => &text[a..=b],
        _ => text,
    }
}

/// Order tasks so every task comes after the tasks it depends on; a cycle is an error. Pure.
pub fn ordered(tasks: &[PlannedTask]) -> Result<Vec<PlannedTask>> {
    let mut out: Vec<PlannedTask> = Vec::new();
    let mut left: Vec<PlannedTask> = tasks.to_vec();
    while !left.is_empty() {
        let pos = left
            .iter()
            .position(|t| t.parents.iter().all(|p| out.iter().any(|o| &o.key == p)))
            .ok_or_else(|| anyhow::anyhow!("the plan's dependencies form a cycle"))?;
        out.push(left.remove(pos));
    }
    Ok(out)
}

fn kanban(db: &Path, args: &[&str], limit: Duration) -> Result<String> {
    let mut full: Vec<&str> = Vec::new();
    let profile = crate::hermes_surfaces::profile_for_role("planner");
    if let Some(p) = profile.as_deref() {
        full.extend(["-p", p]);
    }
    full.push("kanban");
    full.extend_from_slice(args);
    let dbs = db.display().to_string();
    let (code, out, err) = crate::procs::run_with_timeout_env(
        "hermes",
        &full,
        None,
        None,
        limit,
        &[("HERMES_KANBAN_DB", dbs.as_str())],
    )?;
    if code != Some(0) {
        bail!(
            "hermes kanban {} failed: {}",
            args.first().copied().unwrap_or(""),
            last_line(&err).unwrap_or_else(|| last_line(&out).unwrap_or_default())
        );
    }
    Ok(out)
}

fn last_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.starts_with("hermes: "))
        .map(String::from)
}

fn remove_db(db: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let p = PathBuf::from(format!("{}{suffix}", db.display()));
        let _ = std::fs::remove_file(p);
    }
}

/// Ask Hermes to split the goal. Returns the tasks in dependency order; the private database is
/// removed whether or not the call succeeds.
pub fn decompose(g: &Goal) -> Result<Vec<PlannedTask>> {
    let db = plan_db(&g.id);
    if let Some(dir) = db.parent() {
        std::fs::create_dir_all(dir)?;
    }
    remove_db(&db);
    let result = decompose_in(g, &db);
    remove_db(&db);
    result
}

fn decompose_in(g: &Goal, db: &Path) -> Result<Vec<PlannedTask>> {
    let folders: Vec<(String, Option<PathBuf>)> = g
        .repos
        .iter()
        .map(|r| (r.clone(), crate::review_agent::project_dir(r)))
        .collect();
    let body_file = db.with_extension("md");
    std::fs::write(&body_file, triage_body(g, &folders))?;
    let title: String = g.outcome.trim().chars().take(120).collect();
    let bf = body_file.display().to_string();
    let created = kanban(
        db,
        &["create", &title, "--triage", "--body-file", &bf, "--json"],
        CLI_LIMIT,
    );
    let _ = std::fs::remove_file(&body_file);
    let created: Value = serde_json::from_str(json_object(&created?))?;
    let root = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("hermes kanban create returned no task id"))?
        .to_string();
    let out = kanban(db, &["decompose", &root, "--json"], DECOMPOSE_LIMIT)?;
    let d: Value = serde_json::from_str(json_object(&out))?;
    if d.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!(
            "the decomposer did not plan this goal: {}",
            d.get("reason")
                .and_then(Value::as_str)
                .unwrap_or("no reason given")
        );
    }
    // fanout false: one tightened task, which is the root itself.
    let keys: Vec<String> = match d.get("child_ids").and_then(Value::as_array) {
        Some(a) if !a.is_empty() => a
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        _ => vec![root.clone()],
    };
    let mut tasks = Vec::new();
    for k in &keys {
        let shown = kanban(db, &["show", k, "--json"], CLI_LIMIT)?;
        tasks.push(parse_show(&shown, &keys)?);
    }
    ordered(&tasks)
}

fn after_key(mission_id: &str) -> String {
    format!("mission.after.{mission_id}")
}

/// The missions a mission waits for (kv `mission.after.<id>`, comma separated).
pub fn depends_on(mission_id: &str) -> Vec<String> {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, &after_key(mission_id)).ok().flatten())
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// A queued goal mission may start once every mission it depends on is done. Pure over `all`.
pub fn dependencies_done(deps: &[String], all: &[Mission]) -> bool {
    deps.iter()
        .all(|d| all.iter().any(|m| &m.id == d && m.status == "done"))
}

/// Create one mission per planned task, under the goal, in dependency order.
pub fn mirror(g: &Goal, tasks: &[PlannedTask]) -> Result<Vec<Mission>> {
    if tasks.is_empty() {
        bail!("the plan has no tasks");
    }
    let dod: String = g
        .checks
        .iter()
        .map(|c| format!("- {}: {}", c.kind, c.target))
        .collect::<Vec<_>>()
        .join("\n");
    let mut made: Vec<(String, Mission)> = Vec::new();
    for t in tasks {
        let repo = task_repo(&t.body, &g.repos).unwrap_or_else(|| g.repos[0].clone());
        let acceptance = acceptance_of(&t.body).unwrap_or_else(|| {
            format!(
                "The task below is done as written, with tests where code changed.\n\nThe goal's definition of done:\n{dod}"
            )
        });
        let goal_text = format!(
            "{}\n\n{}\n\nPart of goal {}: {}",
            t.title,
            t.body,
            g.id,
            g.outcome.trim()
        );
        let m = crate::router::create_mission(
            &repo,
            &goal_text,
            &acceptance,
            "implementer",
            crate::assign::AUTO,
            "",
            "",
            g.budget_usd,
        )?;
        goals::attach(&g.id, &m.id)?;
        let deps: Vec<String> = t
            .parents
            .iter()
            .filter_map(|p| made.iter().find(|(k, _)| k == p).map(|(_, m)| m.id.clone()))
            .collect();
        if !deps.is_empty() {
            let c = store::open()?;
            store::kv_set(&c, &after_key(&m.id), &deps.join(","))?;
        }
        made.push((t.key.clone(), m));
    }
    Ok(made.into_iter().map(|(_, m)| m).collect())
}

/// Plan a goal that is in `planning`: decompose, mirror, then wait for approval at Watch or start
/// running. A failure leaves the goal in planning with the reason, so Plan again can retry.
pub fn plan(v: &crate::vault::Vault, goal_id: &str) -> Result<Goal> {
    let g = goals::get(goal_id)?.ok_or_else(|| anyhow::anyhow!("no goal '{goal_id}'"))?;
    if g.status != "planning" {
        bail!(
            "{goal_id} is {}; only a goal in planning is planned",
            g.status
        );
    }
    if !goals::missions_of(goal_id)?.is_empty() {
        bail!("{goal_id} already has missions");
    }
    goals::set_note(goal_id, "Hermes is planning this goal")?;
    let planned = decompose(&g).and_then(|tasks| mirror(&g, &tasks));
    let out = match planned {
        Ok(ms) => {
            let n = ms.len();
            let (to, note) = if goals::plan_needs_approval(&g.autonomy) {
                (
                    "awaiting",
                    format!("Planned {n} missions. Watch: approve the plan to start."),
                )
            } else {
                ("running", format!("Planned {n} missions."))
            };
            goals::set_status(goal_id, &["planning"], to, &note)?
        }
        Err(e) => {
            goals::set_note(
                goal_id,
                &format!("The planner failed: {e:#}. Plan again to retry."),
            )?;
            let _ = goals::write_note(v, goal_id);
            return Err(e);
        }
    };
    let _ = goals::write_note(v, goal_id);
    Ok(out)
}

/// Run the planner off the caller's thread (a decomposition takes about half a minute).
pub fn plan_in_background(vault_root: PathBuf, goal_id: String) {
    std::thread::spawn(move || {
        let v = crate::vault::Vault::new(&vault_root);
        if let Err(e) = plan(&v, &goal_id) {
            eprintln!("planner: {goal_id}: {e:#}");
        }
    });
}

/// Watch: the owner read the plan and lets it run. Missions still wait for their Start clicks.
pub fn approve(goal_id: &str) -> Result<Goal> {
    goals::set_status(
        goal_id,
        &["awaiting"],
        "running",
        "Plan approved. Start each mission when you are ready.",
    )
}

/// May the scheduler start this queued goal mission now? The goal must be running and not at
/// Watch (there the owner clicks Start), and every dependency must be done. Pure.
pub fn goal_mission_startable(g: &Goal, deps: &[String], all: &[Mission]) -> bool {
    g.status == "running"
        && !goals::missions_wait_for_click(&g.autonomy)
        && dependencies_done(deps, all)
}

/// Why a queued goal mission has not started, for the board. Pure.
pub fn waiting_reason(g: &Goal, deps: &[String], all: &[Mission]) -> Option<String> {
    if g.status != "running" {
        return Some(format!("goal {} is {}", g.id, g.status));
    }
    if goals::missions_wait_for_click(&g.autonomy) {
        return Some("Watch: start it with a click".into());
    }
    let open: Vec<&String> = deps
        .iter()
        .filter(|d| !all.iter().any(|m| &m.id == *d && m.status == "done"))
        .collect();
    if open.is_empty() {
        None
    } else {
        Some(format!(
            "waits for {}",
            open.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal() -> Goal {
        Goal {
            id: "G-001".into(),
            outcome: "Add a probe endpoint and document it".into(),
            repos: vec!["agentos-sandbox-1".into(), "agentos-sandbox-2".into()],
            checks: vec![goals::Check {
                kind: "command".into(),
                target: "npm test".into(),
                repo: None,
            }],
            budget_usd: 0.0,
            deadline: None,
            autonomy: "ask".into(),
            status: "running".into(),
            note: String::new(),
            report: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn t(key: &str, parents: &[&str]) -> PlannedTask {
        PlannedTask {
            key: key.into(),
            title: format!("task {key}"),
            body: String::new(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn the_triage_body_names_the_repositories_the_checks_and_the_rules() {
        let b = triage_body(
            &goal(),
            &[
                ("agentos-sandbox-1".into(), Some(PathBuf::from("C:/p/s1"))),
                ("agentos-sandbox-2".into(), None),
            ],
        );
        assert!(b.starts_with("Outcome: Add a probe endpoint"));
        assert!(b.contains("- agentos-sandbox-1 (C:/p/s1)"));
        assert!(b.contains("- command: npm test"));
        assert!(b.contains("`Repository: <slug>`"));
        assert!(b.contains("Acceptance criteria:"));
    }

    #[test]
    fn a_task_names_one_of_the_goals_repositories() {
        let repos = goal().repos;
        assert_eq!(
            task_repo("Repository: agentos-sandbox-2 (Node.js).\nGoal: x", &repos).as_deref(),
            Some("agentos-sandbox-2")
        );
        assert_eq!(
            task_repo("- Repository: `AgentOS-Sandbox-1`", &repos).as_deref(),
            Some("agentos-sandbox-1")
        );
        assert_eq!(task_repo("Repository: some-other-repo", &repos), None);
        assert_eq!(task_repo("no line here", &repos), None);
    }

    #[test]
    fn acceptance_is_read_from_its_section() {
        let body = "Goal: x\n\nAcceptance criteria:\n- npm test passes\n- README mentions /probe\n\n## Notes\nignored";
        assert_eq!(
            acceptance_of(body).as_deref(),
            Some("- npm test passes\n- README mentions /probe")
        );
        assert_eq!(
            acceptance_of("### Acceptance\n1. works").as_deref(),
            Some("1. works")
        );
        assert_eq!(acceptance_of("Acceptance criteria:\n\n"), None);
        assert_eq!(acceptance_of("nothing"), None);
    }

    #[test]
    fn show_output_becomes_a_task_with_parents_inside_the_plan_only() {
        // Shape verified against hermes kanban show --json on 2026-11-09.
        let json = r#"warning line
{"task": {"id": "t_2", "title": "Document /probe", "body": "Repository: agentos-sandbox-1\n\nAcceptance criteria:\n- README"},
 "parents": ["t_1"], "children": ["t_root"], "comments": [], "events": [], "runs": []}"#;
        let keys = vec!["t_1".to_string(), "t_2".to_string()];
        let task = parse_show(json, &keys).unwrap();
        assert_eq!(task.key, "t_2");
        assert_eq!(task.title, "Document /probe");
        assert_eq!(task.parents, vec!["t_1".to_string()]);
        let lone = parse_show(
            r#"{"task":{"id":"t_9","title":"x"},"parents":["t_root"]}"#,
            &keys,
        )
        .unwrap();
        assert!(lone.parents.is_empty(), "the root is not a dependency");
        assert!(parse_show(r#"{"task":{"id":"t_9","title":" "}}"#, &keys).is_err());
    }

    #[test]
    fn dependencies_come_first_and_cycles_are_refused() {
        let order = ordered(&[t("c", &["b"]), t("a", &[]), t("b", &["a"])]).unwrap();
        let keys: Vec<&str> = order.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
        assert!(ordered(&[t("a", &["b"]), t("b", &["a"])]).is_err());
    }

    fn mission(id: &str, status: &str) -> Mission {
        Mission {
            id: id.into(),
            repo: "agentos-sandbox-1".into(),
            goal: "g".into(),
            acceptance: "a".into(),
            role: "implementer".into(),
            agent: "claude-code".into(),
            status: status.into(),
            provider: String::new(),
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
    fn a_goal_mission_starts_when_the_goal_runs_and_its_dependencies_are_done() {
        let all = vec![mission("M-001", "done"), mission("M-002", "running")];
        let g = goal();
        assert!(goal_mission_startable(&g, &["M-001".into()], &all));
        assert!(!goal_mission_startable(&g, &["M-002".into()], &all));
        assert_eq!(
            waiting_reason(&g, &["M-002".into()], &all).as_deref(),
            Some("waits for M-002")
        );
        let mut watch = goal();
        watch.autonomy = "watch".into();
        assert!(!goal_mission_startable(&watch, &[], &all));
        let mut paused = goal();
        paused.status = "awaiting".into();
        assert_eq!(
            waiting_reason(&paused, &[], &all).as_deref(),
            Some("goal G-001 is awaiting")
        );
        assert_eq!(waiting_reason(&g, &[], &all), None);
    }

    #[test]
    fn mirroring_creates_missions_under_the_goal_with_their_dependencies() {
        let d = tempfile::tempdir().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        let g = goals::create(&goals::NewGoal {
            outcome: "Add a probe endpoint and document it".into(),
            repos: vec!["agentos-sandbox-1".into(), "agentos-sandbox-2".into()],
            checks: vec![goals::Check {
                kind: "command".into(),
                target: "npm test".into(),
                repo: None,
            }],
            budget_usd: 2.0,
            deadline: None,
            autonomy: None,
        })
        .unwrap();
        let tasks = vec![
            PlannedTask {
                key: "t_a".into(),
                title: "Add GET /probe".into(),
                body: "Repository: agentos-sandbox-2\n\nAcceptance criteria:\n- npm test passes"
                    .into(),
                parents: vec![],
            },
            PlannedTask {
                key: "t_b".into(),
                title: "Document /probe".into(),
                body: "Update the README.".into(),
                parents: vec!["t_a".into()],
            },
        ];
        let ms = mirror(&g, &tasks).unwrap();
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0].repo, "agentos-sandbox-2");
        assert_eq!(ms[0].acceptance, "- npm test passes");
        assert_eq!(ms[0].agent, crate::assign::AUTO);
        assert_eq!(ms[0].budget_usd, 2.0);
        assert_eq!(
            ms[1].repo, "agentos-sandbox-1",
            "no line: the first repository"
        );
        assert!(ms[1].acceptance.contains("- command: npm test"));
        assert!(ms[1].goal.contains("Part of goal G-001"));
        assert_eq!(depends_on(&ms[1].id), vec![ms[0].id.clone()]);
        assert!(depends_on(&ms[0].id).is_empty());
        assert_eq!(goals::missions_of(&g.id).unwrap().len(), 2);
        store::set_test_db(None);
    }
}
