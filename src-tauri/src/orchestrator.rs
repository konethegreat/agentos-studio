//! The orchestrator: one pass a minute that routes work, answers routine questions and
//! unsticks missions, so the fleet keeps moving without the owner.

use serde::Serialize;

use crate::missions::{self, Mission};
use crate::{escalation, pause, reports, store, stuck};

const ENABLED: &str = "orchestrator.enabled";

/// Phrases that keep a destructive question for the owner: irrecoverable production data,
/// credentials or money. Everything else is answered automatically.
const OWNER_PATTERNS: [&str; 10] = [
    "production",
    "prod database",
    "prod db",
    "credential",
    "api key",
    "secret",
    "payment",
    "billing",
    "invoice",
    "drop table",
];

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct PassReport {
    pub enabled: bool,
    pub paused: bool,
    pub routed: Vec<String>,
    pub answered: Vec<String>,
    pub retried: Vec<String>,
    pub blocked: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct Status {
    pub enabled: bool,
    pub paused: bool,
    pub running: usize,
    pub queued: usize,
    pub blocked: usize,
    pub paused_missions: usize,
    pub auto_answered_today: i64,
    /// Open questions only the owner may answer (Law 1: production data, credentials, money).
    pub needs_you: Vec<String>,
}

/// On unless the owner turned it off.
pub fn enabled() -> bool {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, ENABLED).ok().flatten())
        .map(|v| v.trim() != "0")
        .unwrap_or(true)
}

pub fn set_enabled(on: bool) {
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, ENABLED, if on { "1" } else { "0" });
    }
}

/// Whether a question must still reach the owner. Pure.
pub fn needs_owner(kind: &str, question: &str) -> bool {
    if kind != "destructive" {
        return false;
    }
    let q = question.to_lowercase();
    OWNER_PATTERNS.iter().any(|p| q.contains(p))
}

/// What to do with a running mission that never got a task id. Pure.
pub fn untracked_verdict(updated_at: i64, now: i64, retries: u32) -> stuck::Verdict {
    stuck::judge(true, updated_at, now, retries)
}

/// One pass. Answers routine questions, routes work, unsticks untracked missions and prunes
/// the report feed. Failures are collected rather than raised: the next pass retries.
pub fn pass() -> PassReport {
    let mut out = PassReport {
        enabled: enabled(),
        paused: pause::paused(),
        ..Default::default()
    };
    if !out.enabled {
        return out;
    }
    let now = store::now();
    if let Ok(open) = escalation::open_list() {
        for e in open {
            if needs_owner(&e.kind, &e.question) {
                continue;
            }
            match escalation::auto_answer(&e.id, "orchestrator") {
                Ok(done) => {
                    let answer = done.answer.clone().unwrap_or_default();
                    let _ = reports::record_here(
                        "escalation.auto_answered",
                        "",
                        &format!("Answered {}: {answer}", e.id),
                        &format!("Kind {}. Asked: {}", e.kind, e.question),
                        &e.id,
                    );
                    out.answered.push(e.id);
                }
                Err(err) => out.error = Some(format!("answering {}: {err}", e.id)),
            }
        }
    }
    if out.paused {
        return out;
    }
    let all = match missions::list(300) {
        Ok(m) => m,
        Err(e) => {
            out.error = Some(e.to_string());
            return out;
        }
    };
    for m in all.iter().filter(|m| m.status == "queued") {
        let Some(agent) = routed(m) else { continue };
        match missions::set_agent(&m.id, agent) {
            Ok(()) => {
                let _ = reports::record_here(
                    "mission.routed",
                    &m.repo,
                    &format!("{} routed to {agent}", m.id),
                    m.goal.lines().next().unwrap_or_default(),
                    &m.id,
                );
                out.routed.push(format!("{} -> {agent}", m.id));
            }
            Err(e) => out.error = Some(format!("routing {}: {e}", m.id)),
        }
    }
    for m in all
        .iter()
        .filter(|m| m.status == "running" && m.task_id.is_none())
    {
        let retries = stuck::retries(&m.id);
        match untracked_verdict(m.updated_at, now, retries) {
            stuck::Verdict::Healthy => {}
            stuck::Verdict::Retry => {
                let note = format!(
                    "The dispatch left no task to follow (attempt {} of {}); queued again.",
                    retries + 1,
                    stuck::MAX_RETRIES
                );
                let done = missions::block(&m.id, &note, false)
                    .and_then(|_| missions::unblock(&m.id, &note));
                match done {
                    Ok(_) => {
                        stuck::note_retry(&m.id);
                        let _ = reports::record_here(
                            "mission.unstuck",
                            &m.repo,
                            &format!("{} requeued", m.id),
                            &note,
                            &m.id,
                        );
                        out.retried.push(m.id.clone());
                    }
                    Err(e) => out.error = Some(format!("requeueing {}: {e}", m.id)),
                }
            }
            stuck::Verdict::Block => {
                let note = "The dispatch left no task to follow and the retry is spent.";
                match missions::block(&m.id, note, false) {
                    Ok(_) => {
                        let _ = reports::record_here(
                            "mission.blocked",
                            &m.repo,
                            &format!("{} blocked", m.id),
                            note,
                            &m.id,
                        );
                        out.blocked.push(m.id.clone());
                    }
                    Err(e) => out.error = Some(format!("blocking {}: {e}", m.id)),
                }
            }
        }
    }
    if let Ok(c) = store::open() {
        let _ = reports::prune(&c, reports::KEEP);
    }
    out
}

/// What the Today card shows.
pub fn status() -> Status {
    let all = missions::list(300).unwrap_or_default();
    let start = chrono::Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|d| d.and_local_timezone(chrono::Local).single())
        .map(|d| d.timestamp())
        .unwrap_or(0);
    Status {
        enabled: enabled(),
        paused: pause::paused(),
        running: all.iter().filter(|m| m.status == "running").count(),
        queued: all.iter().filter(|m| m.status == "queued").count(),
        blocked: all.iter().filter(|m| m.status == "blocked").count(),
        paused_missions: all.iter().filter(|m| m.status == "paused").count(),
        auto_answered_today: reports::count_kind_since("escalation.auto_answered", start)
            .unwrap_or(0),
        needs_you: escalation::open_list()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| needs_owner(&e.kind, &e.question))
            .map(|e| format!("{}: {}", e.id, e.question))
            .collect(),
    }
}

/// Pick the executor for a queued mission from its goal text and repository. Pure.
///
/// Strong UI signals go to Antigravity, CLI, cron, Notion and cross-repository work goes to
/// Hermes, and everything else is left to the existing quota-aware router (`auto`). Qoder is
/// never chosen for unattended work.
pub fn choose_executor(goal: &str, repo: &str) -> &'static str {
    let text = format!("{} {}", goal, repo).to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if has(&[
        "cron",
        "notion",
        "vault",
        "hermes",
        "kanban",
        "shell script",
    ]) {
        return "hermes";
    }
    if has(&[
        "frontend",
        "ui ",
        " ui",
        "screen",
        "layout",
        "tailwind",
        "react",
        "css",
        "landing page",
        "component",
        "styles",
        "stylesheet",
        "button",
        "dialog",
    ]) {
        return "antigravity";
    }
    "auto"
}

/// The executor the orchestrator sets on a queued mission, if it has a strong opinion.
/// A mission keeps any agent the owner or the planner chose.
pub fn routed(m: &Mission) -> Option<&'static str> {
    if m.status != "queued" {
        return None;
    }
    if !m.agent.trim().is_empty() && !m.agent.trim().eq_ignore_ascii_case("auto") {
        return None;
    }
    match choose_executor(&m.goal, &m.repo) {
        "auto" => None,
        agent => Some(agent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_work_goes_to_antigravity() {
        assert_eq!(
            choose_executor(
                "Rebuild the settings screen in the React frontend",
                "agentos-studio"
            ),
            "antigravity"
        );
        assert_eq!(
            choose_executor(
                "Fix the landing page layout and Tailwind styles",
                "k-one-it-solutions-website"
            ),
            "antigravity"
        );
    }

    #[test]
    fn cli_and_fleet_work_goes_to_hermes() {
        assert_eq!(
            choose_executor(
                "Add a cron job that syncs the vault each morning",
                "fleet-tools"
            ),
            "hermes"
        );
        assert_eq!(
            choose_executor("Sync the task board to Notion", "fleet-tools"),
            "hermes"
        );
    }

    #[test]
    fn plain_code_work_stays_with_the_router() {
        assert_eq!(
            choose_executor("Add a retry to the update check", "agentos-studio"),
            "auto"
        );
    }

    #[test]
    fn only_destructive_production_questions_reach_the_owner() {
        assert!(needs_owner("destructive", "Drop the production database?"));
        assert!(needs_owner("destructive", "Rotate the API key now?"));
        assert!(!needs_owner("destructive", "Delete the staging copy?"));
        assert!(!needs_owner("merge", "Merge into production branch 9?"));
        assert!(!needs_owner("budget", "Passed budget. Keep going?"));
    }

    #[test]
    fn an_untracked_mission_retries_once_then_blocks() {
        let now = 1_000_000;
        assert_eq!(
            untracked_verdict(now - 5 * 60, now, 0),
            stuck::Verdict::Healthy
        );
        assert_eq!(
            untracked_verdict(now - 20 * 60, now, 0),
            stuck::Verdict::Retry
        );
        assert_eq!(
            untracked_verdict(now - 20 * 60, now, 1),
            stuck::Verdict::Block
        );
    }

    #[test]
    fn a_pass_answers_routine_questions_and_leaves_production_alone() {
        let _g = store::test_env_lock();
        let d = tempfile::TempDir::new().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        let routine = escalation::raise(
            None,
            Some("M-100"),
            "budget",
            "M-100 passed its budget. Keep going?",
            &["stop", "raise"],
            "stop",
        )
        .unwrap()
        .0;
        let dangerous = escalation::raise(
            None,
            Some("M-101"),
            "destructive",
            "Drop the production database for a clean run?",
            &["stop", "drop"],
            "stop",
        )
        .unwrap()
        .0;
        let out = pass();
        assert!(out.enabled && !out.paused);
        assert_eq!(out.answered, vec![routine.id.clone()]);
        let answered = escalation::get(&routine.id).unwrap().unwrap();
        assert_eq!(answered.status, "answered");
        assert_eq!(answered.answered_by.as_deref(), Some("orchestrator"));
        let open = escalation::get(&dangerous.id).unwrap().unwrap();
        assert_eq!(open.status, "open");
        assert_eq!(
            reports::list(0, "", 10).unwrap()[0].kind,
            "escalation.auto_answered"
        );
        let st = status();
        assert_eq!(st.auto_answered_today, 1);
        assert_eq!(st.needs_you.len(), 1);
        assert!(st.needs_you[0].starts_with(&dangerous.id));
        store::set_test_db(None);
    }

    #[test]
    fn a_disabled_orchestrator_answers_nothing() {
        let _g = store::test_env_lock();
        let d = tempfile::TempDir::new().unwrap();
        store::set_test_db(Some(d.path().join("studio.db")));
        set_enabled(false);
        let e = escalation::raise(
            None,
            Some("M-102"),
            "budget",
            "Keep going?",
            &["stop", "raise"],
            "stop",
        )
        .unwrap()
        .0;
        let out = pass();
        assert!(!out.enabled);
        assert!(out.answered.is_empty());
        assert_eq!(escalation::get(&e.id).unwrap().unwrap().status, "open");
        set_enabled(true);
        store::set_test_db(None);
    }

    #[test]
    fn an_explicit_agent_is_never_replaced() {
        let mut m = Mission {
            id: "M-1".into(),
            repo: "agentos-studio".into(),
            goal: "Rebuild the settings screen".into(),
            acceptance: String::new(),
            role: "implementer".into(),
            agent: "claude-code".into(),
            status: "queued".into(),
            provider: String::new(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: None,
            reviewer: None,
            review_verdict: None,
            pr_url: None,
            review_note: None,
            created_at: 0,
            updated_at: 0,
            verify: None,
        };
        assert_eq!(routed(&m), None);
        m.agent = "auto".into();
        assert_eq!(routed(&m), Some("antigravity"));
    }
}
