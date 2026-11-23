//! Fleet recovery advisories; scheduler policy remains the authority for recovery.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::autonomy;
use crate::kanban;
use crate::missions::{self, Mission};
use crate::{procs, store};

const STALLED_SECS: i64 = 6 * 3600;
const IDLE_REPO_SECS: i64 = 3600;
const MISSION_LIMIT: usize = 300;
const REPOSITORY_LIMIT: usize = 300;

#[derive(Debug)]
struct RepoSnapshot {
    slug: String,
    path: String,
    enabled: bool,
    paused_here: bool,
    goal: Option<String>,
    last_start: i64,
    log: Vec<autonomy::Event>,
}

fn repository_snapshots() -> Result<Vec<RepoSnapshot>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let c = Connection::open_with_flags(store::db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    c.busy_timeout(Duration::from_secs(1))?;
    let projects = Connection::open_with_flags(
        crate::projects::projects_db(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    projects.busy_timeout(Duration::from_secs(1))?;
    repository_snapshots_in(&c, &projects, deadline)
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .context("repository inspection deadline exceeded")
}

fn local_git(dir: &Path, args: &[&str], deadline: Instant) -> Result<String> {
    let limit = remaining(deadline)?.min(Duration::from_secs(5));
    // Unlike an unknown environment variable, an unsupported flag fails closed on older Git.
    let args: Vec<_> = std::iter::once("--no-lazy-fetch")
        .chain(args.iter().copied())
        .collect();
    let (code, stdout, stderr) = procs::run_with_timeout_env(
        "git",
        &args,
        None,
        Some(dir),
        limit,
        &[("GIT_TERMINAL_PROMPT", "0")],
    )?;
    anyhow::ensure!(
        code == Some(0),
        "Git inspection failed for {}: {}",
        dir.display(),
        stderr.trim()
    );
    Ok(stdout)
}

fn repository_config(dir: &Path, deadline: Instant) -> Result<autonomy::Config> {
    let refs = local_git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname) %(symref)",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            "refs/remotes/origin/master",
        ],
        deadline,
    )?;
    let remote_head = refs.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some("refs/remotes/origin/HEAD"))
            .then(|| fields.next())
            .flatten()
    });
    let default_ref = remote_head.or_else(|| {
        ["refs/remotes/origin/main", "refs/remotes/origin/master"]
            .into_iter()
            .find(|candidate| {
                refs.lines()
                    .any(|line| line.split_whitespace().next() == Some(candidate))
            })
    });
    let committed = if let Some(reference) = default_ref {
        let entry = local_git(
            dir,
            &["ls-tree", reference, "--", ".agentos/routing.json"],
            deadline,
        )?;
        if entry.trim().is_empty() {
            None
        } else {
            Some(local_git(
                dir,
                &["show", &format!("{reference}:.agentos/routing.json")],
                deadline,
            )?)
        }
    } else {
        None
    };
    remaining(deadline)?;
    let text = match committed {
        Some(text) => text,
        None => match std::fs::read_to_string(dir.join(".agentos/routing.json")) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(autonomy::Config::default())
            }
            Err(e) => return Err(e).context("reading local routing configuration"),
        },
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).context("invalid routing configuration")?;
    let object = value
        .as_object()
        .context("routing configuration must be an object")?;
    match object.get("autonomy") {
        Some(config) => {
            serde_json::from_value(config.clone()).context("invalid routing autonomy configuration")
        }
        None => Ok(autonomy::Config::default()),
    }
}

fn repository_snapshots_in(
    c: &Connection,
    projects: &Connection,
    deadline: Instant,
) -> Result<Vec<RepoSnapshot>> {
    remaining(deadline)?;
    let mut query = projects.prepare("SELECT p.slug, COALESCE(p.primary_path, (SELECT f.path FROM project_folders f WHERE f.project_id = p.id ORDER BY f.is_primary DESC, f.path LIMIT 1)), p.archived FROM projects p ORDER BY p.slug LIMIT ?1")?;
    let rows = query
        .query_map([REPOSITORY_LIMIT as i64 + 1], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        rows.len() <= REPOSITORY_LIMIT,
        "registered repository limit exceeded ({REPOSITORY_LIMIT})"
    );
    let mut repos = Vec::new();
    for (slug, path, archived) in rows {
        remaining(deadline)?;
        anyhow::ensure!(
            matches!(archived, 0 | 1),
            "invalid archived state for {slug}"
        );
        if archived == 1 {
            continue;
        }
        let path = path
            .filter(|path| !path.trim().is_empty())
            .with_context(|| format!("registered project {slug} has no repository path"))?;
        let dir = Path::new(&path);
        anyhow::ensure!(
            std::fs::metadata(dir)
                .with_context(|| format!("reading registered project path for {slug}"))?
                .is_dir(),
            "registered project path for {slug} is not a directory"
        );
        match std::fs::symlink_metadata(dir.join(".git")) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("reading Git marker for {slug}")),
        }
        let config = repository_config(dir, deadline)?;
        let key = slug.to_lowercase();
        let state = store::kv_get(c, &format!("autonomy.state.{key}"))?
            .map(|raw| serde_json::from_str::<autonomy::RepoState>(&raw))
            .transpose()
            .with_context(|| format!("invalid autonomy state for {slug}"))?
            .unwrap_or_default();
        let paused = store::kv_get(c, &format!("autonomy.off.{key}"))?;
        anyhow::ensure!(
            matches!(paused.as_deref(), None | Some("0" | "1")),
            "invalid autonomy hold for {slug}"
        );
        if let Some(id) = state.goal.as_deref() {
            let status = c
                .query_row("SELECT status FROM goals WHERE id = ?1", [id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?;
            anyhow::ensure!(
                status.is_some(),
                "registered project {slug} references missing goal {id}"
            );
        }
        repos.push(RepoSnapshot {
            slug,
            path,
            enabled: config.enabled,
            paused_here: paused.as_deref() == Some("1"),
            goal: state.goal,
            last_start: state.last_start,
            log: state.log,
        });
    }
    remaining(deadline)?;
    Ok(repos)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KanbanDiag {
    pub task_id: String,
    pub kind: String,
    pub detail: String,
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticAction {
    pub task_id: String,
    pub mission_id: Option<String>,
    pub kind: String,
    pub action: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StalledVerdict {
    pub mission_id: String,
    pub repo: String,
    pub status: String,
    pub stalled_hours: i64,
    pub waiting_on: String,
    pub action_taken: String,
    pub action_needed: String,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IdleRepoReport {
    pub slug: String,
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FleetDoctorReport {
    pub checked_at: i64,
    pub diagnostic_actions: Vec<DiagnosticAction>,
    pub stalled_verdicts: Vec<StalledVerdict>,
    pub idle_repos: Vec<IdleRepoReport>,
    pub last_error: Option<String>,
}

static LAST_REPORT: OnceLock<Mutex<Option<FleetDoctorReport>>> = OnceLock::new();

fn report_lock() -> &'static Mutex<Option<FleetDoctorReport>> {
    LAST_REPORT.get_or_init(|| Mutex::new(None))
}

pub fn status() -> Option<FleetDoctorReport> {
    report_lock().lock().unwrap().clone()
}

pub fn step() {
    if !crate::pause::paused() {
        let _ = run_now();
    }
}

static RECOVERY_LOCK: Mutex<()> = Mutex::new(());

pub fn run_now() -> FleetDoctorReport {
    let mut report = report_from_results(
        chrono::Utc::now().timestamp(),
        kanban::diagnostics_json(),
        missions::list_checked(MISSION_LIMIT + 1),
        repository_snapshots(),
    );
    {
        let _serial = RECOVERY_LOCK.lock().unwrap();
        recover(&mut report.diagnostic_actions);
    }
    *report_lock().lock().unwrap() = Some(report.clone());
    report
}

/// Run the recoverable findings through the scheduler's retry path, re-checking the link first so
/// a mission that moved since the scan is never touched. Anything held stays for the owner.
fn recover(actions: &mut [DiagnosticAction]) {
    let paused = crate::pause::paused();
    for action in actions.iter_mut().filter(|a| a.action == "retry_mission") {
        if paused {
            action.action = "held".into();
            action.detail = format!("the fleet is paused; nothing was queued: {}", action.detail);
            continue;
        }
        let Some(id) = action.mission_id.clone() else {
            action.action = "needs_review".into();
            action.detail = "the mission link was lost before recovery; left on board".into();
            continue;
        };
        let outcome = missions::get(&id)
            .map_err(|e| anyhow::anyhow!("{e:#}"))
            .and_then(|m| m.ok_or_else(|| anyhow::anyhow!("mission {id} disappeared")))
            .and_then(|m| {
                anyhow::ensure!(
                    m.task_id
                        .as_deref()
                        .and_then(|t| t.strip_prefix(kanban::PREFIX))
                        == Some(action.task_id.as_str()),
                    "the mission link changed"
                );
                crate::automation::recover_blocked(&m, &action.detail)
            });
        match outcome {
            Ok(crate::automation::Recovery::Retried(why)) => {
                action.action = "retried".into();
                action.detail = why;
            }
            Ok(crate::automation::Recovery::Held(why)) => {
                action.action = "held".into();
                action.detail = why;
            }
            Err(e) => {
                action.action = "needs_review".into();
                action.detail = format!("recovery was not attempted: {e:#}");
            }
        }
    }
}

fn report_from_results(
    now: i64,
    diagnostics: Result<String>,
    missions: Result<Vec<Mission>>,
    repos: Result<Vec<RepoSnapshot>>,
) -> FleetDoctorReport {
    let mut errors = Vec::new();
    let diags = diagnostics
        .and_then(|raw| parse_diagnostics(&raw))
        .unwrap_or_else(|e| {
            errors.push(format!("kanban diagnostics failed: {e:#}"));
            Vec::new()
        });
    let mut missions = missions.unwrap_or_else(|e| {
        errors.push(format!("mission query failed: {e:#}"));
        Vec::new()
    });
    if missions.len() > MISSION_LIMIT {
        missions.truncate(MISSION_LIMIT);
        errors.push(format!(
            "only the first {MISSION_LIMIT} missions were inspected"
        ));
    }
    let repos = repos.unwrap_or_else(|e| {
        errors.push(format!("repository query failed: {e:#}"));
        Vec::new()
    });
    FleetDoctorReport {
        checked_at: now,
        diagnostic_actions: diagnose_tasks(&diags, &missions),
        stalled_verdicts: compute_stalled_verdicts(&missions, now),
        idle_repos: if errors.is_empty() {
            compute_idle_repos(&repos, &missions, now)
        } else {
            Vec::new()
        },
        last_error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

pub fn parse_diagnostics(raw: &str) -> Result<Vec<KanbanDiag>> {
    let tasks: Vec<serde_json::Value> =
        serde_json::from_str(raw).context("expected a Hermes diagnostics task array")?;
    let mut out = Vec::new();
    for task in tasks {
        let nested = task
            .get("diagnostics")
            .and_then(|v| v.as_array())
            .context("task is missing its diagnostics array")?;
        if nested.is_empty() {
            continue;
        }
        let task_id = task
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .context("diagnostics are missing a task ID")?;
        for d in nested {
            let kind = d
                .get("kind")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .context("diagnostic is missing its kind")?;
            let detail = d
                .get("data")
                .and_then(|v| v.get("last_error"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .or_else(|| d.get("detail").and_then(|v| v.as_str()))
                .context("diagnostic is missing its detail")?;
            out.push(KanbanDiag {
                task_id: task_id.into(),
                kind: kind.into(),
                detail: detail.into(),
                provider: d
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                model: d
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            });
        }
    }
    Ok(out)
}

fn is_model_error(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    [
        "model not found",
        "unknown model",
        "invalid model",
        "unsupported model",
        "model does not exist",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn is_entitlement_error(detail: &str) -> bool {
    let words = detail
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = format!(" {words} ");
    [
        "entitlement is exhausted",
        "entitlement exhausted",
        "quota exceeded",
        "quota exhausted",
        "insufficient quota",
        "billing hard limit",
        "insufficient credits",
        "credit balance too low",
        "subscription expired",
        "authentication failed",
        "authentication error",
        "invalid api key",
        "api key revoked",
        "http 401",
        "http 402",
        "http 403",
    ]
    .iter()
    .any(|marker| normalized.contains(&format!(" {marker} ")))
}

pub fn diagnose_tasks(diags: &[KanbanDiag], missions: &[Mission]) -> Vec<DiagnosticAction> {
    let mut grouped = std::collections::BTreeMap::<&str, Vec<&KanbanDiag>>::new();
    for d in diags {
        grouped.entry(&d.task_id).or_default().push(d);
    }
    grouped.into_iter().map(|(task_id, task_diags)| {
        let d = task_diags.into_iter().min_by_key(|d| match d.kind.as_str() {
            "repeated_failures" | "repeated_crashes" if is_entitlement_error(&d.detail) => 0,
            "repeated_failures" | "repeated_crashes" if is_model_error(&d.detail) => 1,
            "repeated_failures" | "repeated_crashes" => 2,
            "stuck_in_blocked" => 3,
            _ => 4,
        }).expect("group contains a diagnostic");
        let mut action = DiagnosticAction {
            task_id: task_id.into(),
            mission_id: None,
            kind: d.kind.clone(),
            action: "needs_review".into(),
            detail: "no linked Studio mission found; left on board".into(),
        };
        let mut linked = missions.iter().filter(|m| {
            m.task_id.as_deref().and_then(|id| id.strip_prefix(kanban::PREFIX)) == Some(task_id)
        });
        let Some(mission) = linked.next() else { return action; };
        action.mission_id = Some(mission.id.clone());
        if linked.next().is_some() {
            action.mission_id = None;
            action.detail = "multiple Studio missions reference this task; inspect linkage".into();
        } else if mission.agent != "hermes" {
            action.detail = "linked mission is not owned by Hermes; left on board".into();
        } else if mission.status == "blocked"
            && matches!(d.kind.as_str(), "repeated_crashes" | "repeated_failures")
            && crate::automation::model_failure(&d.detail)
        {
            action.action = "retry_mission".into();
            action.detail = format!(
                "the mission is blocked on its model or provider, not its work; the scheduler retry budget decides whether to queue it again: {}",
                d.detail
            );
        } else if mission.status != "running" {
            action.detail = format!("linked mission is {}; inspect its state before recovery; left on board", mission.status);
        } else if matches!(d.kind.as_str(), "repeated_crashes" | "repeated_failures") {
            if is_entitlement_error(&d.detail) {
                let provider = d.provider.as_deref().unwrap_or(&mission.provider);
                if provider.trim().is_empty() {
                    action.action = "needs_provider".into();
                    action.detail = format!("identify the failed provider before checking credentials: {}", d.detail);
                } else {
                    action.action = "provider_check_needed".into();
                    action.detail = format!("check {provider} in Providers; no credentials changed: {}", d.detail);
                }
            } else if is_model_error(&d.detail) {
                action.action = "model_change_needed".into();
                action.detail = format!("inspect the pinned route and scheduler retry allowance; no recovery requested: {}", d.detail);
            } else {
                action.detail = format!("no automatic fix identified; left on board: {}", d.detail);
            }
        } else {
            action.detail = format!("inspect {}: {}; no recovery requested", d.kind, d.detail);
        }
        action
    }).collect()
}

pub fn compute_stalled_verdicts(missions: &[Mission], now: i64) -> Vec<StalledVerdict> {
    missions
        .iter()
        .filter(|m| !matches!(m.status.as_str(), "done" | "failed"))
        .filter(|m| (now - m.updated_at).max(0) >= STALLED_SECS)
        .map(|m| {
            let note = m
                .review_note
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("no recent note");
            let (waiting_on, action_needed) = match m.status.as_str() {
                "queued" => (
                    format!("scheduler slot or lane deferral ({note})"),
                    "inspect scheduler, lane availability and fleet pause state",
                ),
                "running" => (
                    format!(
                        "worker progress on {} ({note})",
                        m.task_id.as_deref().unwrap_or("local runner")
                    ),
                    "inspect worker heartbeat and diagnostics before retrying",
                ),
                "review" => (
                    format!("CI checks or PR review ({note})"),
                    "inspect PR checks and independent review; retain merge gates",
                ),
                "blocked" | "awaiting" => (
                    format!("owner input or unblock ({note})"),
                    "inspect escalation and blocking reason",
                ),
                other => (
                    format!("status '{other}' ({note})"),
                    "inspect mission state",
                ),
            };
            StalledVerdict {
                mission_id: m.id.clone(),
                repo: m.repo.clone(),
                status: m.status.clone(),
                stalled_hours: (now - m.updated_at).max(0) / 3600,
                waiting_on,
                action_taken: "read recorded mission state".into(),
                action_needed: action_needed.into(),
            }
        })
        .collect()
}

fn compute_idle_repos(
    repos: &[RepoSnapshot],
    missions: &[Mission],
    now: i64,
) -> Vec<IdleRepoReport> {
    repos
        .iter()
        .filter(|r| r.enabled && !r.paused_here && r.goal.is_none())
        .filter_map(|r| {
            let active_mission = missions
                .iter()
                .any(|m| m.repo == r.slug && matches!(m.status.as_str(), "running" | "review"));
            let recent_goal = (r.last_start > 0
                && now.saturating_sub(r.last_start) < IDLE_REPO_SECS)
                || r.log
                    .iter()
                    .any(|ev| ev.kind == "started" && now.saturating_sub(ev.at) < IDLE_REPO_SECS);
            if active_mission || recent_goal {
                return None;
            }
            let reason = r
                .log
                .last()
                .map(|event| event.text.clone())
                .unwrap_or_else(|| {
                    "no goal started within 1 hour in this registered repository".to_string()
                });
            Some(IdleRepoReport {
                slug: r.slug.clone(),
                path: r.path.clone(),
                reason,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission(status: &str, task: &str) -> Mission {
        Mission {
            id: "M-1".into(),
            repo: "repo".into(),
            status: status.into(),
            agent: "hermes".into(),
            provider: "claude-subscription-directsdk".into(),
            model: "pinned-model".into(),
            task_id: Some(task.into()),
            ..Default::default()
        }
    }

    fn diag(kind: &str, detail: &str) -> KanbanDiag {
        KanbanDiag {
            task_id: "KT-1".into(),
            kind: kind.into(),
            detail: detail.into(),
            ..Default::default()
        }
    }

    fn repo() -> RepoSnapshot {
        RepoSnapshot {
            slug: "repo".into(),
            path: "/fixture/repo".into(),
            enabled: true,
            paused_here: false,
            goal: None,
            last_start: 0,
            log: vec![],
        }
    }

    fn with_registered_repository(
        f: impl FnOnce(&rusqlite::Connection, &rusqlite::Connection, &std::path::Path),
    ) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("repo");
        std::fs::create_dir_all(path.join(".agentos")).unwrap();
        let (code, _, stderr) = crate::procs::run_with_timeout(
            "git",
            &["init", "--quiet"],
            None,
            Some(&path),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(code, Some(0), "{stderr}");
        std::fs::write(
            path.join(".agentos/routing.json"),
            r#"{"autonomy":{"enabled":true}}"#,
        )
        .unwrap();
        let studio = crate::store::open_at(&temp.path().join("studio.db")).unwrap();
        let projects = rusqlite::Connection::open_in_memory().unwrap();
        projects.execute_batch("CREATE TABLE projects (id TEXT, slug TEXT, primary_path TEXT, archived INTEGER); CREATE TABLE project_folders (project_id TEXT, path TEXT, is_primary INTEGER);").unwrap();
        projects
            .execute(
                "INSERT INTO projects VALUES ('p1', 'repo', ?1, 0)",
                [path.to_str().unwrap()],
            )
            .unwrap();
        f(&studio, &projects, &path);
    }

    #[test]
    fn repository_snapshot_skips_ordinary_project_folders() {
        with_registered_repository(|studio, projects, path| {
            let folder = path.parent().unwrap().join("notes");
            std::fs::create_dir(&folder).unwrap();
            projects
                .execute(
                    "INSERT INTO projects VALUES ('notes', 'notes', ?1, 0)",
                    [folder.to_str().unwrap()],
                )
                .unwrap();
            let repos =
                repository_snapshots_in(studio, projects, Instant::now() + Duration::from_secs(5))
                    .unwrap();
            assert_eq!(repos.len(), 1);
            assert_eq!(repos[0].slug, "repo");
        });
    }

    #[test]
    fn repository_snapshot_reports_missing_and_broken_repository_paths() {
        with_registered_repository(|studio, projects, path| {
            let folder = path.parent().unwrap().join("broken");
            projects
                .execute(
                    "INSERT INTO projects VALUES ('broken', 'broken', ?1, 0)",
                    [folder.to_str().unwrap()],
                )
                .unwrap();
            let deadline = || Instant::now() + Duration::from_secs(5);
            assert!(repository_snapshots_in(studio, projects, deadline()).is_err());
            std::fs::create_dir(&folder).unwrap();
            std::fs::write(folder.join(".git"), "gitdir: missing-worktree").unwrap();
            assert!(repository_snapshots_in(studio, projects, deadline()).is_err());
        });
    }

    #[test]
    fn local_git_does_not_fetch_missing_promisor_objects() {
        with_registered_repository(|_, _, path| {
            let remote = format!(
                "remote.fixture.url={}",
                path.join("missing-promisor").display()
            );
            let error = local_git(
                path,
                &[
                    "-c",
                    &remote,
                    "-c",
                    "remote.fixture.promisor=true",
                    "cat-file",
                    "blob",
                    "1111111111111111111111111111111111111111",
                ],
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap_err()
            .to_string();
            assert!(
                !error.contains("missing-promisor"),
                "Git attempted a fetch: {error}"
            );
            assert!(error.contains("bad file"), "{error}");
        });
    }

    #[test]
    fn registered_repository_snapshot_rejects_incomplete_inputs() {
        with_registered_repository(|studio, projects, path| {
            let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(5);
            let repos = repository_snapshots_in(studio, projects, deadline()).unwrap();
            assert_eq!(repos.len(), 1);
            assert!(repos[0].enabled);
            crate::store::kv_set(studio, "autonomy.state.repo", "not-json").unwrap();
            assert!(repository_snapshots_in(studio, projects, deadline())
                .unwrap_err()
                .to_string()
                .contains("autonomy state"));
            crate::store::kv_set(
                studio,
                "autonomy.state.repo",
                &serde_json::to_string(&autonomy::RepoState::default()).unwrap(),
            )
            .unwrap();
            std::fs::write(path.join(".agentos/routing.json"), "not-json").unwrap();
            assert!(repository_snapshots_in(studio, projects, deadline())
                .unwrap_err()
                .to_string()
                .contains("routing"));
            std::fs::write(
                path.join(".agentos/routing.json"),
                r#"{"autonomy":{"enabled":true}}"#,
            )
            .unwrap();
            projects.execute_batch("DROP TABLE projects").unwrap();
            assert!(repository_snapshots_in(studio, projects, deadline()).is_err());
        });
    }

    #[test]
    fn repository_snapshot_rejects_dangling_goals_and_expired_deadlines() {
        with_registered_repository(|studio, projects, _| {
            let state = autonomy::RepoState {
                goal: Some("G-missing".into()),
                ..Default::default()
            };
            crate::store::kv_set(
                studio,
                "autonomy.state.repo",
                &serde_json::to_string(&state).unwrap(),
            )
            .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            assert!(repository_snapshots_in(studio, projects, deadline)
                .unwrap_err()
                .to_string()
                .contains("G-missing"));
            assert!(
                repository_snapshots_in(studio, projects, std::time::Instant::now())
                    .unwrap_err()
                    .to_string()
                    .contains("deadline")
            );
        });
    }

    #[test]
    fn synthetic_diagnostics_link_real_format_mission_ids() {
        let raw = r#"[
            {"task_id":"KT-1","status":"blocked","assignee":"implementer","diagnostics":[{"kind":"repeated_crashes","detail":"model not found 404"}]},
            {"task_id":null,"dispatch_profiles":[],"diagnostics":[]}
        ]"#;
        let diags = parse_diagnostics(raw).unwrap();
        assert_eq!(diags.len(), 1);
        let actions = diagnose_tasks(&diags, &[mission("running", "kanban:KT-1")]);
        assert_eq!(actions[0].action, "model_change_needed");
        assert_eq!(actions[0].task_id, "KT-1");
        assert_eq!(
            diagnose_tasks(&diags, &[mission("running", "KT-1")])[0].action,
            "needs_review"
        );
    }

    #[test]
    fn doctor_keeps_recovery_advisory() {
        let m = mission("running", "kanban:KT-1");
        for (kind, detail, expected) in [
            (
                "repeated_crashes",
                "model not found 404",
                "model_change_needed",
            ),
            (
                "repeated_failures",
                "entitlement is exhausted (HTTP 403)",
                "provider_check_needed",
            ),
            (
                "repeated_failures",
                "API key revoked",
                "provider_check_needed",
            ),
            ("repeated_crashes", "test suite failed", "needs_review"),
        ] {
            let actions = diagnose_tasks(&[diag(kind, detail)], std::slice::from_ref(&m));
            assert_eq!(actions[0].action, expected);
            assert!(actions[0].detail.contains(detail));
        }
        assert_eq!(m.provider, "claude-subscription-directsdk");
        assert_eq!(m.model, "pinned-model");
    }

    #[test]
    fn blocked_model_failures_route_to_the_scheduler_retry() {
        for (kind, detail) in [
            (
                "repeated_crashes",
                "No reply: gemini-4-hydrogen-high didn't produce a reply this time",
            ),
            (
                "repeated_failures",
                "worker exited cleanly (rc=0) without kanban_complete; broken pipe",
            ),
            (
                "repeated_failures",
                "billing, credits, or account entitlement is exhausted",
            ),
        ] {
            let actions =
                diagnose_tasks(&[diag(kind, detail)], &[mission("blocked", "kanban:KT-1")]);
            assert_eq!(actions[0].action, "retry_mission", "{detail}");
            assert_eq!(actions[0].mission_id.as_deref(), Some("M-1"));
            assert!(actions[0].detail.contains("retry budget"));
        }
        assert_eq!(
            diagnose_tasks(
                &[diag("repeated_failures", "test suite failed")],
                &[mission("blocked", "kanban:KT-1")]
            )[0]
            .action,
            "needs_review",
            "a blocked mission whose work failed stays for a human"
        );
        assert_eq!(
            diagnose_tasks(
                &[diag(
                    "repeated_failures",
                    "No reply: x didn't produce a reply"
                )],
                &[mission("running", "kanban:KT-1")]
            )[0]
            .action,
            "needs_review",
            "a running worker is never requeued behind its back"
        );
    }

    #[test]
    fn doctor_requeues_a_blocked_model_failure_once_then_holds() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        crate::automation::set_enabled(true);
        let m = missions::create(
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
        let detail = "No reply: gemini-4-hydrogen-high didn't produce a reply this time";
        missions::dispatch(&m.id, "kanban:KT-1").unwrap();
        missions::block(&m.id, "worker crashed", false).unwrap();
        let mut actions = diagnose_tasks(
            &[diag("repeated_crashes", detail)],
            &[missions::get(&m.id).unwrap().unwrap()],
        );
        recover(&mut actions);
        assert_eq!(actions[0].action, "retried", "{:?}", actions[0]);
        assert_eq!(
            missions::get(&m.id).unwrap().unwrap().status,
            "queued",
            "the scheduler queued it again"
        );

        missions::dispatch(&m.id, "kanban:KT-1").unwrap();
        missions::block(&m.id, "worker crashed", false).unwrap();
        let c = crate::store::open().unwrap();
        crate::store::kv_set(
            &c,
            &crate::automation::model_retry_key(&m.id),
            &crate::automation::MODEL_RETRIES.to_string(),
        )
        .unwrap();
        let mut actions = diagnose_tasks(
            &[diag("repeated_crashes", detail)],
            &[missions::get(&m.id).unwrap().unwrap()],
        );
        recover(&mut actions);
        assert_eq!(actions[0].action, "held", "{:?}", actions[0]);
        assert!(actions[0].detail.contains("attempts are spent"));
        assert_eq!(
            missions::get(&m.id).unwrap().unwrap().status,
            "blocked",
            "a spent budget never loops the mission"
        );
        crate::store::set_test_db(None);
    }

    /// Replay of the agentos-studio board as recorded on 2026-11-23. The fixture holds the two
    /// blocked cards from the 2026-11-17 Gemini episode that were still blocked (their
    /// `tasks.last_failure_error` and `task_runs.outcome` read read-only from the board) and the
    /// three blocked `needs_input` cards. The 2026-11-17 cards that were resolved are no longer on
    /// the board, so this replays what the recorded rows can still prove.
    #[test]
    fn recorded_blocked_board_replays_with_only_model_failures_requeued() {
        let raw = include_str!("../tests/fixtures/kanban-diagnostics-2026-11-23.json");
        let diags = parse_diagnostics(raw).expect("the recorded diagnostics must parse");
        assert_eq!(
            diags.len(),
            5,
            "two blocked model failures and three blocked needs_input cards"
        );

        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        crate::automation::set_enabled(true);
        let _ = crate::pause::resume();

        let mut linked = Vec::new();
        for diag in &diags {
            let m = missions::create(
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
            missions::dispatch(&m.id, &format!("kanban:{}", diag.task_id)).unwrap();
            missions::block(&m.id, "worker crashed", false).unwrap();
            linked.push(missions::get(&m.id).unwrap().unwrap());
        }

        let mut actions = diagnose_tasks(&diags, &linked);
        assert_eq!(actions.len(), 5);
        let retry: Vec<_> = actions
            .iter()
            .filter(|a| a.action == "retry_mission")
            .collect();
        assert_eq!(retry.len(), 2, "{actions:?}");
        assert!(
            retry.iter().any(|a| a.task_id == "t_456f2788"),
            "No reply card"
        );
        assert!(
            retry.iter().any(|a| a.task_id == "t_9b07991f"),
            "entitlement card"
        );
        assert!(
            actions
                .iter()
                .filter(|a| a.action == "needs_review")
                .all(|a| a.detail.contains("linked mission is blocked")),
            "needs_input cards stay for a human: {actions:?}"
        );

        recover(&mut actions);
        for action in &actions {
            let expected = match action.task_id.as_str() {
                "t_456f2788" | "t_9b07991f" => "retried",
                _ => "needs_review",
            };
            assert_eq!(action.action, expected, "{action:?}");
        }
        for m in &linked {
            let task = m.task_id.as_deref().unwrap_or_default();
            let status = missions::get(&m.id).unwrap().unwrap().status;
            if task.ends_with("t_456f2788") || task.ends_with("t_9b07991f") {
                assert_eq!(
                    status, "queued",
                    "the scheduler queued the model failure again"
                );
            } else {
                assert_eq!(status, "blocked", "the needs_input card is untouched");
            }
        }
        crate::store::set_test_db(None);
    }

    #[test]
    fn held_terminal_unlinked_and_other_agent_tasks_need_review() {
        for status in [
            "blocked", "awaiting", "paused", "queued", "review", "failed", "done",
        ] {
            for kind in ["stuck_in_blocked", "repeated_failures", "repeated_crashes"] {
                let actions = diagnose_tasks(
                    &[diag(kind, "model not found 404")],
                    &[mission(status, "kanban:KT-1")],
                );
                assert_eq!(actions[0].action, "needs_review");
                assert!(actions[0].detail.contains(status));
            }
        }
        let d = diag("repeated_failures", "quota 403");
        assert_eq!(
            diagnose_tasks(std::slice::from_ref(&d), &[])[0].action,
            "needs_review"
        );
        let mut m = mission("running", "kanban:KT-1");
        m.agent = "qoder".into();
        assert_eq!(diagnose_tasks(&[d], &[m])[0].action, "needs_review");
    }

    #[test]
    fn ambiguous_mission_linkage_needs_review() {
        let m = mission("running", "kanban:KT-1");
        let mut other = m.clone();
        other.id = "M-2".into();
        let actions = diagnose_tasks(&[diag("repeated_crashes", "model not found")], &[m, other]);
        assert_eq!(actions[0].action, "needs_review");
        assert!(actions[0].detail.contains("multiple Studio missions"));
    }

    #[test]
    fn overlapping_diagnostics_produce_one_finding_per_task() {
        let d = diag("repeated_crashes", "model not found");
        let actions = diagnose_tasks(
            &[
                diag("stuck_in_blocked", "blocked for 24 hours"),
                d.clone(),
                d,
            ],
            &[mission("running", "kanban:KT-1")],
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action, "model_change_needed");
    }

    #[test]
    fn application_errors_do_not_recommend_model_changes() {
        for detail in [
            "source file not found",
            "provider SDK panicked",
            "test suite failed",
            "NameError: name 'author' is not defined",
            "test_authorization failed",
            "expected 1403 records, got 1401",
            "credit column is missing",
            "subscription fixture failed",
            "test_model failed: expected 1404 records",
            "test_model returned HTTP 404 for a missing application route",
        ] {
            let actions = diagnose_tasks(
                &[diag("repeated_crashes", detail)],
                &[mission("running", "kanban:KT-1")],
            );
            assert_eq!(actions[0].action, "needs_review");
        }
    }

    #[test]
    fn malformed_diagnostics_are_errors_not_empty_health_reports() {
        for raw in [
            "not json",
            "null",
            "{}",
            r#"[{"task_id":"KT-1"}]"#,
            r#"[{"task_id":null,"diagnostics":[{"kind":"repeated_crashes","detail":"model not found"}]}]"#,
            r#"[{"task_id":"KT-1","diagnostics":[{"detail":"model not found"}]}]"#,
        ] {
            assert!(
                parse_diagnostics(raw).is_err(),
                "accepted invalid diagnostics: {raw}"
            );
        }
        assert!(parse_diagnostics("[]").unwrap().is_empty());
    }

    #[test]
    fn diagnostics_use_actual_error_not_generic_recovery_advice() {
        let raw = r#"[{"task_id":"KT-1","diagnostics":[{"kind":"repeated_failures","detail":"credentials or model not found; fix provider/model","data":{"last_error":"test suite failed"}}]}]"#;
        let diags = parse_diagnostics(raw).unwrap();
        assert_eq!(diags[0].detail, "test suite failed");
        assert_eq!(
            diagnose_tasks(&diags, &[mission("running", "kanban:KT-1")])[0].action,
            "needs_review"
        );
    }

    #[test]
    fn incomplete_inputs_are_visible_and_suppress_idle_inferences() {
        let report = report_from_results(
            10,
            Ok("not json".into()),
            Err(anyhow::anyhow!("query failed")),
            Ok(vec![repo()]),
        );
        let error = report.last_error.unwrap();
        assert!(error.contains("kanban diagnostics failed"));
        assert!(error.contains("mission query failed"));
        assert!(report.idle_repos.is_empty());
        let report = report_from_results(
            10,
            Ok("[]".into()),
            Ok(vec![]),
            Err(anyhow::anyhow!("repository unavailable")),
        );
        assert!(report
            .last_error
            .unwrap()
            .contains("repository unavailable"));
    }

    #[test]
    fn truncated_mission_queries_cannot_report_all_clear() {
        let report = report_from_results(
            10,
            Ok("[]".into()),
            Ok(vec![mission("done", "kanban:KT-1"); 301]),
            Ok(vec![repo()]),
        );
        assert!(report
            .last_error
            .unwrap()
            .contains("only the first 300 missions"));
        assert!(report.idle_repos.is_empty());
    }

    #[test]
    fn healthy_empty_inputs_have_no_findings_or_error() {
        let report = report_from_results(10, Ok("[]".into()), Ok(vec![]), Ok(vec![]));
        assert!(report.last_error.is_none());
        assert!(report.diagnostic_actions.is_empty());
        assert!(report.stalled_verdicts.is_empty());
        assert!(report.idle_repos.is_empty());
    }

    #[test]
    fn stalled_verdicts_report_only_observed_checks() {
        for status in ["queued", "running", "review", "blocked"] {
            let verdicts = compute_stalled_verdicts(&[mission(status, "kanban:KT-1")], 7 * 3600);
            assert_eq!(verdicts[0].stalled_hours, 7);
            assert_eq!(verdicts[0].action_taken, "read recorded mission state");
            assert!(!verdicts[0].action_needed.contains("merge manually"));
        }
        assert!(compute_stalled_verdicts(&[mission("done", "kanban:KT-1")], 7 * 3600).is_empty());
    }

    #[test]
    fn failed_or_held_updates_do_not_hide_idle_repositories() {
        let now = 100_000;
        for status in ["failed", "blocked", "queued", "done"] {
            let mut m = mission(status, "kanban:KT-1");
            m.updated_at = now;
            let mut r = repo();
            r.log.push(autonomy::Event {
                at: now,
                kind: "error".into(),
                goal: None,
                text: "fetch failed".into(),
            });
            let idle = compute_idle_repos(&[r], &[m], now);
            assert_eq!(idle.len(), 1);
            assert_eq!(idle[0].reason, "fetch failed");
        }
    }

    #[test]
    fn active_work_and_recent_goal_starts_are_not_idle() {
        let now = 100_000;
        for status in ["running", "review"] {
            assert!(
                compute_idle_repos(&[repo()], &[mission(status, "kanban:KT-1")], now).is_empty()
            );
        }
        let mut r = repo();
        r.log.push(autonomy::Event {
            at: now,
            kind: "started".into(),
            goal: Some("G-1".into()),
            text: "started".into(),
        });
        assert!(compute_idle_repos(std::slice::from_ref(&r), &[], now).is_empty());
        assert_eq!(compute_idle_repos(&[r], &[], now + 3601).len(), 1);
    }
}
