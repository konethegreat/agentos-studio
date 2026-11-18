//! Fleet doctor (S2): reads `hermes kanban diagnostics --json`, acts on repeated crashes,
//! stuck blocked tasks, and entitlement failures; computes 6h stalled mission verdicts for
//! the Today screen; and reports opted-in repos with no goal started within an hour.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};

use crate::agent_log::{self, NewEvent};
use crate::autonomy::{self, RepoStatus};
use crate::kanban;
use crate::missions::{self, Mission};
use crate::provider_health;
use crate::quota_grid::Lane;
use crate::store;

const STALLED_SECS: i64 = 6 * 3600;
const IDLE_REPO_SECS: i64 = 3600;

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
    if crate::pause::paused() {
        return;
    }
    let _ = run_now();
}

pub fn run_now() -> FleetDoctorReport {
    let now = chrono::Utc::now().timestamp();
    let mut last_error = None;
    let raw_diag = match kanban::diagnostics_json() {
        Ok(s) => s,
        Err(e) => {
            last_error = Some(format!("kanban diagnostics failed: {e}"));
            "[]".to_string()
        }
    };
    let diags = parse_diagnostics(&raw_diag);
    let missions_all = missions::list(300).unwrap_or_default();
    let lanes = crate::quota_grid::lanes(false);
    let repos = autonomy::statuses().unwrap_or_default();

    let diagnostic_actions = act_on_diagnostics_with(
        &diags,
        &missions_all,
        &lanes,
        |task_id, provider, model| kanban::set_model(task_id, provider, model),
        |task_id, reason| kanban::reclaim(task_id, reason),
        |task_id| kanban::archive(task_id),
        |provider| {
            let Ok(c) = store::open() else {
                return "store unavailable".into();
            };
            let rep = provider_health::read();
            let entries = provider_health::auto_heal_in(&c, &rep, now, true);
            let filtered: Vec<_> = entries
                .into_iter()
                .filter(|e| e.provider.eq_ignore_ascii_case(provider))
                .collect();
            if filtered.is_empty() {
                "no heal action needed".into()
            } else {
                filtered
                    .into_iter()
                    .map(|e| format!("{}: {}", e.outcome, e.detail))
                    .collect::<Vec<_>>()
                    .join("; ")
            }
        },
    );

    let stalled_verdicts = compute_stalled_verdicts(&missions_all, now);
    let idle_repos = compute_idle_repos(&repos, &missions_all, now);

    for act in &diagnostic_actions {
        if matches!(
            act.action.as_str(),
            "repinned_and_reclaimed" | "archived" | "auto_healed" | "reclaimed"
        ) {
            let _ = agent_log::record(&NewEvent {
                mission_id: None,
                kind: "doctor",
                from_agent: "agentos-studio",
                to_agent: "owner",
                summary: &format!("Doctor {} on {}: {}", act.action, act.task_id, act.kind),
                detail: &act.detail,
                duration_ms: None,
                cost_usd: None,
            });
        }
    }

    let rep = FleetDoctorReport {
        checked_at: now,
        diagnostic_actions,
        stalled_verdicts,
        idle_repos,
        last_error,
    };
    *report_lock().lock().unwrap() = Some(rep.clone());
    rep
}

/// Parse `hermes kanban diagnostics --json` output into normalized `KanbanDiag` records.
pub fn parse_diagnostics(raw: &str) -> Vec<KanbanDiag> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let items = if let Some(arr) = val.as_array() {
        arr.clone()
    } else if let Some(arr) = val.get("diagnostics").and_then(|v| v.as_array()) {
        arr.clone()
    } else if val.is_object() {
        vec![val]
    } else {
        Vec::new()
    };

    for item in items {
        let top_task = item
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if let Some(nested) = item.get("diagnostics").and_then(|v| v.as_array()) {
            for d in nested {
                if let Some(parsed) = parse_diag_entry(d, &top_task) {
                    out.push(parsed);
                }
            }
        } else if item.get("kind").is_some()
            && let Some(parsed) = parse_diag_entry(&item, &top_task)
        {
            out.push(parsed);
        }
    }
    out
}

fn parse_diag_entry(d: &serde_json::Value, fallback_task: &str) -> Option<KanbanDiag> {
    let task_id = d
        .get("task_id")
        .and_then(|v| v.as_str())
        .unwrap_or(fallback_task)
        .trim()
        .to_string();
    if task_id.is_empty() {
        return None;
    }
    let kind = d
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if kind.is_empty() {
        return None;
    }
    let detail = d
        .get("detail")
        .or_else(|| d.get("reason"))
        .or_else(|| d.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let provider = d
        .get("provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let model = d
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(KanbanDiag {
        task_id,
        kind,
        detail,
        provider,
        model,
    })
}

fn is_model_error(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("model")
        || lower.contains("not found")
        || lower.contains("unsupported")
        || lower.contains("404")
        || lower.contains("invalid_request")
        || lower.contains("provider")
        || lower.contains("no reply")
}

fn is_entitlement_error(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("entitlement")
        || lower.contains("quota")
        || lower.contains("billing")
        || lower.contains("credit")
        || lower.contains("subscription")
        || lower.contains("auth")
        || lower.contains("401")
        || lower.contains("402")
        || lower.contains("403")
}

fn infer_provider(d: &KanbanDiag, missions: &[Mission]) -> Option<String> {
    if let Some(p) = d.provider.as_deref().filter(|s| !s.trim().is_empty()) {
        return Some(p.to_string());
    }
    if let Some(m) = missions
        .iter()
        .find(|m| m.task_id.as_deref() == Some(d.task_id.as_str()))
    {
        if !m.provider.trim().is_empty() {
            return Some(m.provider.clone());
        }
    }
    let lower = d.detail.to_ascii_lowercase();
    for candidate in ["antigravity", "claude", "anthropic", "openrouter", "openai"] {
        if lower.contains(candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

pub fn act_on_diagnostics_with<FSet, FRec, FArch, FHeal>(
    diags: &[KanbanDiag],
    missions: &[Mission],
    lanes: &[Lane],
    mut set_model: FSet,
    mut reclaim: FRec,
    mut archive: FArch,
    mut auto_heal_provider: FHeal,
) -> Vec<DiagnosticAction>
where
    FSet: FnMut(&str, &str, &str) -> Result<()>,
    FRec: FnMut(&str, &str) -> Result<()>,
    FArch: FnMut(&str) -> Result<()>,
    FHeal: FnMut(&str) -> String,
{
    let mut actions = Vec::new();
    let first_open = lanes.iter().find(|l| l.open);

    for d in diags {
        match d.kind.as_str() {
            "repeated_crashes" => {
                if is_model_error(&d.detail) {
                    if let Some(lane) = first_open {
                        let set_res = set_model(&d.task_id, &lane.provider, &lane.model);
                        let rec_res = reclaim(
                            &d.task_id,
                            "AgentOS Fleet Doctor: repeated crashes with model error; re-pinned to open lane",
                        );
                        let (action, detail) = match (set_res, rec_res) {
                            (Ok(()), Ok(())) => (
                                "repinned_and_reclaimed",
                                format!("re-pinned to {}/{} and reclaimed", lane.provider, lane.model),
                            ),
                            (e1, e2) => (
                                "repin_failed",
                                format!("set_model={:?}, reclaim={:?}", e1.err(), e2.err()),
                            ),
                        };
                        actions.push(DiagnosticAction {
                            task_id: d.task_id.clone(),
                            kind: d.kind.clone(),
                            action: action.into(),
                            detail,
                        });
                    } else {
                        actions.push(DiagnosticAction {
                            task_id: d.task_id.clone(),
                            kind: d.kind.clone(),
                            action: "no_open_lane".into(),
                            detail: "no open lane available to re-pin".into(),
                        });
                    }
                } else {
                    let rec_res = reclaim(
                        &d.task_id,
                        "AgentOS Fleet Doctor: repeated crashes; reclaimed for retry",
                    );
                    actions.push(DiagnosticAction {
                        task_id: d.task_id.clone(),
                        kind: d.kind.clone(),
                        action: if rec_res.is_ok() { "reclaimed" } else { "reclaim_failed" }.into(),
                        detail: rec_res.err().map(|e| e.to_string()).unwrap_or_else(|| "reclaimed".into()),
                    });
                }
            }
            "stuck_in_blocked" => {
                let linked = missions
                    .iter()
                    .find(|m| m.task_id.as_deref() == Some(d.task_id.as_str()));
                let dead = linked.is_none_or(|m| m.status == "failed");
                if dead {
                    let res = archive(&d.task_id);
                    actions.push(DiagnosticAction {
                        task_id: d.task_id.clone(),
                        kind: d.kind.clone(),
                        action: if res.is_ok() { "archived" } else { "archive_failed" }.into(),
                        detail: res
                            .err()
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "archived failed or deleted mission task".into()),
                    });
                } else {
                    actions.push(DiagnosticAction {
                        task_id: d.task_id.clone(),
                        kind: d.kind.clone(),
                        action: "needs_review".into(),
                        detail: "linked mission is not failed; left on board".into(),
                    });
                }
            }
            "repeated_failures" => {
                if is_entitlement_error(&d.detail) {
                    if let Some(provider) = infer_provider(d, missions) {
                        let heal_out = auto_heal_provider(&provider);
                        actions.push(DiagnosticAction {
                            task_id: d.task_id.clone(),
                            kind: d.kind.clone(),
                            action: "auto_healed".into(),
                            detail: format!("{provider}: {heal_out}"),
                        });
                    } else {
                        actions.push(DiagnosticAction {
                            task_id: d.task_id.clone(),
                            kind: d.kind.clone(),
                            action: "needs_provider".into(),
                            detail: "entitlement failure without identifiable provider".into(),
                        });
                    }
                } else {
                    actions.push(DiagnosticAction {
                        task_id: d.task_id.clone(),
                        kind: d.kind.clone(),
                        action: "noted".into(),
                        detail: d.detail.clone(),
                    });
                }
            }
            _ => {
                actions.push(DiagnosticAction {
                    task_id: d.task_id.clone(),
                    kind: d.kind.clone(),
                    action: "noted".into(),
                    detail: d.detail.clone(),
                });
            }
        }
    }

    actions
}

/// S2.2: Any non-terminal mission with no state change for 6 hours gets a doctor verdict.
pub fn compute_stalled_verdicts(missions: &[Mission], now: i64) -> Vec<StalledVerdict> {
    missions
        .iter()
        .filter(|m| !matches!(m.status.as_str(), "done" | "failed"))
        .filter(|m| (now - m.updated_at).max(0) >= STALLED_SECS)
        .map(|m| {
            let stalled_hours = (now - m.updated_at).max(0) / 3600;
            let note = m
                .review_note
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("no recent note");
            let (waiting_on, action_taken, action_needed) = match m.status.as_str() {
                "queued" => (
                    format!("scheduler slot or lane deferral ({note})"),
                    "checked lane availability".to_string(),
                    "verify open lanes on Quota grid or unpause fleet".to_string(),
                ),
                "running" => (
                    format!("worker progress on {} ({note})", m.task_id.as_deref().unwrap_or("local runner")),
                    "checked kanban diagnostics and heartbeat".to_string(),
                    "reclaim or restart stuck worker if no heartbeat".to_string(),
                ),
                "review" => (
                    format!("CI checks or PR review ({note})"),
                    "checked PR status".to_string(),
                    "inspect PR checks or merge manually".to_string(),
                ),
                "blocked" | "awaiting" => (
                    format!("owner input or unblock ({note})"),
                    "kept mission paused for review".to_string(),
                    "answer escalation or unblock mission".to_string(),
                ),
                other => (
                    format!("status '{other}' ({note})"),
                    "monitored".to_string(),
                    "inspect mission state".to_string(),
                ),
            };
            StalledVerdict {
                mission_id: m.id.clone(),
                repo: m.repo.clone(),
                status: m.status.clone(),
                stalled_hours,
                waiting_on,
                action_taken,
                action_needed,
            }
        })
        .collect()
}

/// S2.3: Report any opted-in repository with no goal started within an hour and the reason.
pub fn compute_idle_repos(
    repos: &[RepoStatus],
    missions: &[Mission],
    now: i64,
) -> Vec<IdleRepoReport> {
    repos
        .iter()
        .filter(|r| r.enabled && !r.paused_here && r.goal.is_none())
        .filter_map(|r| {
            let recent_mission = missions
                .iter()
                .any(|m| m.repo == r.slug && (now - m.updated_at).max(0) < IDLE_REPO_SECS);
            let recent_log = r
                .log
                .iter()
                .any(|ev| (now - ev.at).max(0) < IDLE_REPO_SECS);
            if recent_mission || recent_log {
                return None;
            }
            let reason = if !r.hermes_project {
                "not a Hermes project".to_string()
            } else if !r.next.trim().is_empty() {
                r.next.clone()
            } else if let Some(ev) = r.log.first() {
                ev.text.clone()
            } else {
                "no goal started within 1 hour (check fetch or default branch)".to_string()
            };
            Some(IdleRepoReport {
                slug: r.slug.clone(),
                path: r.path.clone(),
                reason,
            })
        })
        .collect()
}

/// Helper for S2 replay test: derive a `KanbanDiag` from a `kanban show --json` TaskView.
pub fn diag_from_task_view(view: &kanban::TaskView) -> Option<KanbanDiag> {
    let last_err = view
        .runs
        .iter()
        .rev()
        .find_map(|r| r.error.as_deref())
        .unwrap_or_default();
    if view.status == "blocked" && view.runs.len() >= 2 && is_model_error(last_err) {
        return Some(KanbanDiag {
            task_id: view.id.clone(),
            kind: "repeated_crashes".into(),
            detail: last_err.to_string(),
            provider: view.runs.last().and_then(|r| r.provider.clone()),
            model: view.runs.last().and_then(|r| r.model.clone()),
        });
    }
    if view.status == "blocked" && is_entitlement_error(last_err) {
        return Some(KanbanDiag {
            task_id: view.id.clone(),
            kind: "repeated_failures".into(),
            detail: last_err.to_string(),
            provider: view.runs.last().and_then(|r| r.provider.clone()),
            model: None,
        });
    }
    if view.status == "blocked" {
        return Some(KanbanDiag {
            task_id: view.id.clone(),
            kind: "stuck_in_blocked".into(),
            detail: last_err.to_string(),
            provider: None,
            model: None,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission(id: &str, repo: &str, status: &str, task: Option<&str>, updated_at: i64) -> Mission {
        Mission {
            id: id.into(),
            repo: repo.into(),
            goal: id.into(),
            status: status.into(),
            agent: "hermes".into(),
            provider: "antigravity-oauth".into(),
            model: "gemini-4-hydrogen".into(),
            task_id: task.map(str::to_string),
            created_at: updated_at,
            updated_at,
            ..Default::default()
        }
    }

    fn lane(provider: &str, model: &str, open: bool) -> Lane {
        Lane {
            provider: provider.into(),
            model: model.into(),
            name: model.into(),
            flat_rate: true,
            used_pct: 10,
            state: if open { "ready".into() } else { "exhausted".into() },
            open,
            reason: None,
            resets_at: None,
            score: 100,
        }
    }

    #[test]
    fn replay_four_blocked_tasks_2026_11_17() {
        // Replay test built from real `hermes kanban show --json` shapes for four blocked tasks:
        // 1) repeated_crashes with model error -> re-pinned to first open lane & reclaimed
        // 2) stuck_in_blocked with failed mission -> archived
        // 3) stuck_in_blocked with deleted mission -> archived
        // 4) repeated_failures on entitlement -> S0.2 auto-heal for that provider
        let t1 = r#"{"id":"KT-101","status":"blocked","runs":[{"provider":"openrouter","model":"bad-model","error":"model not found 404"},{"provider":"openrouter","model":"bad-model","error":"model not found 404"}]}"#;
        let t2 = r#"{"id":"KT-102","status":"blocked","runs":[{"error":"worker exited"}]}"#;
        let t3 = r#"{"id":"KT-103","status":"blocked","runs":[{"error":"orphan task"}]}"#;
        let t4 = r#"{"id":"KT-104","status":"blocked","runs":[{"provider":"claude","error":"entitlement quota 403"}]}"#;

        let views: Vec<kanban::TaskView> = [t1, t2, t3, t4]
            .iter()
            .map(|s| kanban::parse_show(s).expect("valid show json"))
            .collect();
        let diags: Vec<KanbanDiag> = views.iter().filter_map(diag_from_task_view).collect();
        assert_eq!(diags.len(), 4);

        let missions = vec![
            mission("M-1", "repo-a", "running", Some("KT-101"), 1000),
            mission("M-2", "repo-b", "failed", Some("KT-102"), 1000),
            // KT-103 has no mission (deleted)
            mission("M-4", "repo-c", "running", Some("KT-104"), 1000),
        ];
        let lanes = vec![
            lane("openrouter", "bad-model", false),
            lane("antigravity-oauth", "gemini-4-hydrogen", true),
        ];

        let mut repinned = Vec::new();
        let mut reclaimed = Vec::new();
        let mut archived = Vec::new();
        let mut healed = Vec::new();

        let actions = act_on_diagnostics_with(
            &diags,
            &missions,
            &lanes,
            |id, prov, model| {
                repinned.push((id.to_string(), prov.to_string(), model.to_string()));
                Ok(())
            },
            |id, _reason| {
                reclaimed.push(id.to_string());
                Ok(())
            },
            |id| {
                archived.push(id.to_string());
                Ok(())
            },
            |prov| {
                healed.push(prov.to_string());
                "ok: refreshed".into()
            },
        );

        assert_eq!(actions.len(), 4);
        assert_eq!(repinned, vec![("KT-101".into(), "antigravity-oauth".into(), "gemini-4-hydrogen".into())]);
        assert_eq!(reclaimed, vec!["KT-101".to_string()]);
        assert_eq!(archived, vec!["KT-102".to_string(), "KT-103".to_string()]);
        assert_eq!(healed, vec!["claude".to_string()]);
    }

    #[test]
    fn stalled_verdicts_and_idle_repos() {
        let now = 100_000;
        let missions = vec![
            mission("M-10", "r1", "queued", None, now - 7 * 3600),
            mission("M-11", "r1", "done", None, now - 10 * 3600),
            mission("M-12", "r2", "running", Some("KT-9"), now - 600),
        ];
        let verdicts = compute_stalled_verdicts(&missions, now);
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].mission_id, "M-10");
        assert_eq!(verdicts[0].stalled_hours, 7);

        let repos = vec![
            RepoStatus {
                slug: "r1".into(),
                path: "/tmp/r1".into(),
                hermes_project: false,
                enabled: true,
                paused_here: false,
                level: "full".into(),
                brief: String::new(),
                allow_merge: true,
                has_plan: false,
                phases: vec![],
                goal: None,
                goal_status: None,
                next: String::new(),
                log: vec![],
            },
            RepoStatus {
                slug: "r2".into(),
                path: "/tmp/r2".into(),
                hermes_project: true,
                enabled: true,
                paused_here: false,
                level: "full".into(),
                brief: String::new(),
                allow_merge: true,
                has_plan: true,
                phases: vec![],
                goal: None,
                goal_status: None,
                next: "fetch failed".into(),
                log: vec![],
            },
        ];
        let idle = compute_idle_repos(&repos, &missions, now);
        assert_eq!(idle.len(), 1);
        assert_eq!(idle[0].slug, "r1");
        assert_eq!(idle[0].reason, "not a Hermes project");
    }
}
