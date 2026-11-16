//! Shadow Kone (Beyond B1): answers the escalations the owner has already answered the same way,
//! after a window in which he can overrule it. Design and limits: `docs/SHADOW.md`.
//!
//! - A rule exists only from the owner's own decisions: his three most recent answers to one
//!   reason class agree, or he set a standing answer for that class.
//! - Budget, destructive and merge-allow-list questions are never answered by Shadow.
//! - Every answer is planned first (kv `shadow.plan.<id>`), shown on Today and sent to the phone,
//!   and given only when the window closes with no answer from the owner (`answered_by = shadow`).

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::escalation::{self, Escalation};
use crate::store;

/// Classes Shadow never answers: they are the owner's standing switches.
pub const NEVER: [&str; 3] = ["budget", "destructive", "merge_allow_list"];

/// Every reason class, in the order the Autonomy screen lists them.
pub const CLASSES: [&str; 12] = [
    "provider",
    "no_pr",
    "no_heartbeat",
    "conflict",
    "ci_after_fix",
    "second_reject",
    "done_check",
    "plan_failed",
    "other",
    "budget",
    "destructive",
    "merge_allow_list",
];

/// Identical owner answers in a row that make a rule.
pub const STREAK: usize = 3;

pub const DEFAULT_WINDOW_MINUTES: i64 = 10;

/// Who counts as the owner deciding: Studio, the phone or the Notion goals board.
pub const OWNER_SOURCES: [&str; 3] = ["studio", "phone", "notion"];

const ENABLED_KEY: &str = "shadow.enabled";
const WINDOW_KEY: &str = "shadow.window_minutes";

pub fn plan_key(escalation_id: &str) -> String {
    format!("shadow.plan.{escalation_id}")
}

pub fn standing_key(class: &str) -> String {
    format!("shadow.standing.{class}")
}

/// What kind of question this is, from the escalation kind and its text. Pure.
pub fn reason_class(kind: &str, question: &str) -> &'static str {
    match kind {
        "budget" => "budget",
        "destructive" => "destructive",
        "merge" => "merge_allow_list",
        "second_reject" => "second_reject",
        "done_check" => "done_check",
        "plan_failed" => "plan_failed",
        _ => {
            let q = question.to_lowercase();
            if crate::automation::model_failure(&q) {
                "provider"
            } else if q.contains("without opening a pull request")
                || q.contains("nothing to review")
            {
                "no_pr"
            } else if q.contains("no heartbeat") {
                "no_heartbeat"
            } else if q.contains("conflict") {
                "conflict"
            } else if q.contains("still fails after a fix") {
                "ci_after_fix"
            } else {
                "other"
            }
        }
    }
}

/// What the owner sees for a class on the Autonomy screen. Pure.
pub fn class_label(class: &str) -> &'static str {
    match class {
        "provider" => "A provider or worker failed (limit, credits, lost worker)",
        "no_pr" => "An agent finished without opening a pull request",
        "no_heartbeat" => "An agent went quiet (no heartbeat)",
        "conflict" => "The work conflicts with another pull request",
        "ci_after_fix" => "CI still fails after a fix mission",
        "second_reject" => "The reviewer rejected the work twice",
        "done_check" => "A done check failed twice",
        "plan_failed" => "Planning failed three times",
        "budget" => "A metered budget was passed",
        "destructive" => "A destructive approval",
        "merge_allow_list" => "A merge outside the allow list",
        _ => "Anything else",
    }
}

/// Did the owner himself decide this escalation? Pure.
pub fn owner_decided(e: &Escalation) -> bool {
    e.status == "answered"
        && e.answered_by
            .as_deref()
            .is_some_and(|b| OWNER_SOURCES.contains(&b))
}

/// The rule in a class, from the owner's answers newest first: his last `STREAK` answers agree.
/// Pure.
pub fn rule_from(answers_newest_first: &[&str]) -> Option<String> {
    let first = *answers_newest_first.first()?;
    (answers_newest_first.len() >= STREAK
        && answers_newest_first[..STREAK].iter().all(|a| *a == first))
    .then(|| first.to_string())
}

/// The owner's decisions in one class, newest first, leaving out `except`. Pure.
pub fn precedents<'a>(history: &'a [Escalation], class: &str, except: &str) -> Vec<&'a Escalation> {
    let mut out: Vec<&Escalation> = history
        .iter()
        .filter(|e| e.id != except && owner_decided(e))
        .filter(|e| reason_class(&e.kind, &e.question) == class)
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.answered_at.unwrap_or(e.created_at)));
    out
}

/// What Shadow would answer and why, or None when the owner must answer. Pure.
pub fn decide(
    e: &Escalation,
    history: &[Escalation],
    standing: Option<&str>,
) -> Option<(String, &'static str, String)> {
    let class = reason_class(&e.kind, &e.question);
    if NEVER.contains(&class) {
        return None;
    }
    if let Some(a) = standing.map(str::trim).filter(|a| !a.is_empty()) {
        if e.options.iter().any(|o| o == a) {
            return Some((
                a.to_string(),
                "standing",
                format!("your standing answer for \"{}\"", class_label(class)),
            ));
        }
    }
    let prec = precedents(history, class, &e.id);
    let answers: Vec<&str> = prec.iter().filter_map(|p| p.answer.as_deref()).collect();
    let answer = rule_from(&answers)?;
    if !e.options.contains(&answer) {
        return None;
    }
    let ids: Vec<&str> = prec.iter().take(STREAK).map(|p| p.id.as_str()).collect();
    Some((
        answer.clone(),
        "rule",
        format!(
            "you answered the last {STREAK} \"{}\" questions {answer} ({})",
            class_label(class),
            ids.join(", ")
        ),
    ))
}

/// When Shadow answers: after the window, and always before the escalation's own deadline. Pure.
pub fn due_at(now: i64, deadline_at: i64, window_minutes: i64) -> i64 {
    (now + window_minutes * 60).min(deadline_at - 60).max(now)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub escalation: String,
    /// The escalation's `created_at`, so a plan never carries over to a reused id.
    pub for_created_at: i64,
    pub class: String,
    pub answer: String,
    /// `standing` or `rule`.
    pub source: String,
    pub reason: String,
    pub planned_at: i64,
    pub due_at: i64,
}

fn kv(key: &str) -> Option<String> {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, key).ok().flatten())
}

pub fn enabled() -> bool {
    kv(ENABLED_KEY).as_deref().map(str::trim) != Some("0")
}

pub fn window_minutes() -> i64 {
    kv(WINDOW_KEY)
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| (1..=24 * 60).contains(n))
        .unwrap_or(DEFAULT_WINDOW_MINUTES)
}

pub fn standing(class: &str) -> Option<String> {
    kv(&standing_key(class)).filter(|s| !s.trim().is_empty())
}

pub fn plan_of(escalation_id: &str) -> Option<Plan> {
    kv(&plan_key(escalation_id)).and_then(|s| serde_json::from_str(&s).ok())
}

fn save_plan(p: &Plan) -> Result<()> {
    let c = store::open()?;
    store::kv_set(&c, &plan_key(&p.escalation), &serde_json::to_string(p)?)
}

/// The phone line announcing a plan. Pure.
pub fn announcement(p: &Plan, due_local: &str) -> String {
    format!(
        "Shadow (AgentOS Studio): I will answer {} \"{}\" at {due_local}, because {}. To overrule, reply to the question with your own answer before then.",
        p.escalation, p.answer, p.reason
    )
}

/// One Shadow pass: plan new escalations it can answer, answer those whose window closed.
/// Returns one line per action, with the escalation's mission, for the timeline and the pass log.
pub fn step(now: i64) -> Result<Vec<(Option<String>, String)>> {
    let mut log = Vec::new();
    if !enabled() || crate::pause::paused() {
        return Ok(log);
    }
    let history = escalation::list(500)?;
    for e in history.iter().filter(|e| e.status == "open") {
        match plan_of(&e.id).filter(|p| p.for_created_at == e.created_at) {
            Some(p) if now >= p.due_at => match escalation::answer(&e.id, &p.answer, "shadow") {
                Ok(_) => log.push((
                    e.mission_id.clone(),
                    format!("Shadow answered {} \"{}\": {}", e.id, p.answer, p.reason),
                )),
                Err(err) => log.push((
                    e.mission_id.clone(),
                    format!("Shadow could not answer {}: {err:#}", e.id),
                )),
            },
            Some(_) => {}
            None => {
                let class = reason_class(&e.kind, &e.question);
                let Some((answer, source, reason)) =
                    decide(e, &history, standing(class).as_deref())
                else {
                    continue;
                };
                let p = Plan {
                    escalation: e.id.clone(),
                    for_created_at: e.created_at,
                    class: class.to_string(),
                    answer,
                    source: source.to_string(),
                    reason,
                    planned_at: now,
                    due_at: due_at(now, e.deadline_at, window_minutes()),
                };
                save_plan(&p)?;
                if !cfg!(test) {
                    if let Some(target) = crate::hermes_surfaces::notify_target() {
                        let at = chrono::DateTime::from_timestamp(p.due_at, 0)
                            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
                            .unwrap_or_default();
                        let _ = crate::hermes_surfaces::send(&target, &announcement(&p, &at));
                    }
                }
                log.push((
                    e.mission_id.clone(),
                    format!(
                        "Shadow will answer {} \"{}\" in {} min: {}",
                        e.id,
                        p.answer,
                        (p.due_at - now + 59) / 60,
                        p.reason
                    ),
                ));
            }
        }
    }
    Ok(log)
}

#[derive(Debug, Clone, Serialize)]
pub struct Rule {
    pub class: String,
    pub label: String,
    /// The standing answer the owner set, if any.
    pub standing: Option<String>,
    /// The learned answer, if his last answers agree.
    pub learned: Option<String>,
    /// His decisions in this class, newest first: `E-16 retry`.
    pub precedents: Vec<String>,
    /// Shadow never answers this class.
    pub never: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub escalation: String,
    pub class: String,
    pub planned: String,
    pub source: String,
    pub reason: String,
    pub due_at: i64,
    /// `pending`, `answered` (by Shadow), `agreed` (the owner answered the same first),
    /// `overruled` or `expired`.
    pub outcome: String,
    pub final_answer: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub window_minutes: i64,
    pub rules: Vec<Rule>,
    pub record: Vec<Record>,
    /// Of the plans the owner answered first, the share where he agreed.
    pub agreement: Option<f64>,
}

/// How a plan turned out, from its escalation's current state. Pure.
pub fn outcome(p: &Plan, e: Option<&Escalation>) -> &'static str {
    let Some(e) = e.filter(|e| e.created_at == p.for_created_at) else {
        return "expired";
    };
    match (e.status.as_str(), e.answered_by.as_deref()) {
        ("open", _) => "pending",
        ("answered", Some("shadow")) => "answered",
        ("answered", _) if e.answer.as_deref() == Some(p.answer.as_str()) => "agreed",
        ("answered", _) => "overruled",
        _ => "expired",
    }
}

pub fn status() -> Result<Status> {
    let history = escalation::list(500)?;
    let rules = CLASSES
        .iter()
        .map(|class| {
            let prec = precedents(&history, class, "");
            let answers: Vec<&str> = prec.iter().filter_map(|p| p.answer.as_deref()).collect();
            Rule {
                class: class.to_string(),
                label: class_label(class).to_string(),
                standing: standing(class),
                learned: rule_from(&answers),
                precedents: prec
                    .iter()
                    .take(6)
                    .map(|p| format!("{} {}", p.id, p.answer.as_deref().unwrap_or("?")))
                    .collect(),
                never: NEVER.contains(class),
            }
        })
        .collect();
    let c = store::open()?;
    let mut plans: Vec<Plan> = c
        .prepare("SELECT value FROM kv WHERE key LIKE 'shadow.plan.%'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .flatten()
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect();
    plans.sort_by_key(|p| std::cmp::Reverse(p.planned_at));
    let record: Vec<Record> = plans
        .iter()
        .map(|p| {
            let e = history.iter().find(|e| e.id == p.escalation);
            Record {
                escalation: p.escalation.clone(),
                class: p.class.clone(),
                planned: p.answer.clone(),
                source: p.source.clone(),
                reason: p.reason.clone(),
                due_at: p.due_at,
                outcome: outcome(p, e).to_string(),
                final_answer: e
                    .filter(|e| e.created_at == p.for_created_at)
                    .and_then(|e| e.answer.clone()),
            }
        })
        .collect();
    let judged: Vec<&Record> = record
        .iter()
        .filter(|r| r.outcome == "agreed" || r.outcome == "overruled")
        .collect();
    let agreement = (!judged.is_empty()).then(|| {
        judged.iter().filter(|r| r.outcome == "agreed").count() as f64 / judged.len() as f64
    });
    Ok(Status {
        enabled: enabled(),
        window_minutes: window_minutes(),
        rules,
        record,
        agreement,
    })
}

/// Settings from the Autonomy screen. `standing` is (class, answer); an empty answer clears it.
pub fn set(
    enabled: Option<bool>,
    window: Option<i64>,
    standing: Option<(String, String)>,
) -> Result<Status> {
    let c = store::open()?;
    if let Some(on) = enabled {
        store::kv_set(&c, ENABLED_KEY, if on { "1" } else { "0" })?;
    }
    if let Some(w) = window {
        if !(1..=24 * 60).contains(&w) {
            anyhow::bail!("the Shadow window must be between 1 minute and 24 hours");
        }
        store::kv_set(&c, WINDOW_KEY, &w.to_string())?;
    }
    if let Some((class, answer)) = standing {
        if !CLASSES.contains(&class.as_str()) {
            anyhow::bail!("unknown class '{class}'");
        }
        if NEVER.contains(&class.as_str()) {
            anyhow::bail!("Shadow never answers \"{}\"", class_label(&class));
        }
        let answer = answer.trim().to_lowercase();
        if answer.is_empty() {
            store::kv_delete(&c, &standing_key(&class))?;
        } else if answer == "retry" || answer == "stop" {
            store::kv_set(&c, &standing_key(&class), &answer)?;
        } else {
            anyhow::bail!("a standing answer is retry or stop");
        }
    }
    drop(c);
    status()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(
        id: &str,
        kind: &str,
        q: &str,
        answer: Option<&str>,
        by: Option<&str>,
        at: i64,
    ) -> Escalation {
        Escalation {
            id: id.into(),
            goal_id: Some("G-1".into()),
            mission_id: Some("M-1".into()),
            kind: kind.into(),
            question: q.into(),
            options: vec!["retry".into(), "stop".into()],
            safe_answer: "stop".into(),
            status: if answer.is_some() {
                if by == Some("timeout") {
                    "expired".into()
                } else {
                    "answered".into()
                }
            } else {
                "open".into()
            },
            answer: answer.map(Into::into),
            answered_by: by.map(Into::into),
            created_at: at,
            deadline_at: at + 3600,
            answered_at: answer.map(|_| at + 60),
        }
    }

    const HEARTBEAT: &str = "M-029 is blocked after its retry: Stuck: no heartbeat from claude-code for 2 min after an automatic retry. Retry it, or stop it?";
    const NO_PR: &str = "M-049 is blocked after its retry: antigravity finished twice without opening a pull request, so there is nothing to review. Retry it, or stop it?";
    const LOST: &str = "M-096 is blocked after its retry: Hermes kanban: pid 26144 exited with code 1 [managed worker lost: EOFError]. Retry it, or stop it?";

    #[test]
    fn the_real_escalations_get_their_classes() {
        assert_eq!(reason_class("stuck", HEARTBEAT), "no_heartbeat");
        assert_eq!(reason_class("stuck", NO_PR), "no_pr");
        assert_eq!(reason_class("stuck", LOST), "provider");
        assert_eq!(
            reason_class("stuck", "M-068 is blocked after its retry: Hermes kanban: Blocked on PR #16 (latestRelease sync): still open and now conflicts with main"),
            "conflict"
        );
        assert_eq!(
            reason_class(
                "stuck",
                "CI on o/r#7 still fails after a fix mission. Try another fix, or stop?"
            ),
            "ci_after_fix"
        );
        assert_eq!(
            reason_class("merge", "M-020 was approved"),
            "merge_allow_list"
        );
        assert_eq!(reason_class("budget", ""), "budget");
        assert_eq!(reason_class("destructive", ""), "destructive");
        assert_eq!(reason_class("plan_failed", ""), "plan_failed");
        assert_eq!(reason_class("stuck", "something new"), "other");
        for c in CLASSES.iter().filter(|c| **c != "other") {
            assert_ne!(class_label(c), "Anything else", "{c} needs its own label");
        }
    }

    #[test]
    fn three_identical_answers_in_a_row_make_a_rule_and_one_change_breaks_it() {
        assert_eq!(
            rule_from(&["retry", "retry", "retry"]),
            Some("retry".into())
        );
        assert_eq!(
            rule_from(&["stop", "stop", "stop", "retry"]),
            Some("stop".into())
        );
        assert_eq!(
            rule_from(&["retry", "stop", "stop", "stop"]),
            None,
            "the newest answer differs"
        );
        assert_eq!(rule_from(&["retry", "retry"]), None);
        assert_eq!(rule_from(&[]), None);
    }

    #[test]
    fn only_the_owner_s_own_decisions_are_precedents() {
        let history = vec![
            esc("E-1", "stuck", HEARTBEAT, Some("stop"), Some("studio"), 100),
            esc(
                "E-2",
                "stuck",
                HEARTBEAT,
                Some("stop"),
                Some("timeout"),
                200,
            ),
            esc("E-3", "stuck", HEARTBEAT, Some("stop"), Some("shadow"), 300),
            esc("E-4", "stuck", HEARTBEAT, Some("retry"), Some("phone"), 400),
            esc("E-5", "stuck", NO_PR, Some("stop"), Some("studio"), 500),
            esc("E-6", "stuck", HEARTBEAT, None, None, 600),
        ];
        let p: Vec<&str> = precedents(&history, "no_heartbeat", "")
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(
            p,
            vec!["E-4", "E-1"],
            "newest first, no timeout, no shadow, no other class"
        );
    }

    #[test]
    fn shadow_follows_the_owner_s_streak_and_never_touches_his_switches() {
        let mut history = vec![
            esc("E-7", "stuck", HEARTBEAT, Some("stop"), Some("studio"), 100),
            esc("E-8", "stuck", HEARTBEAT, Some("stop"), Some("studio"), 200),
        ];
        let open = esc("E-20", "stuck", HEARTBEAT, None, None, 1000);
        assert_eq!(
            decide(&open, &history, None),
            None,
            "two answers are not a rule"
        );

        history.push(esc(
            "E-9",
            "stuck",
            HEARTBEAT,
            Some("stop"),
            Some("studio"),
            300,
        ));
        let (a, src, why) = decide(&open, &history, None).unwrap();
        assert_eq!((a.as_str(), src), ("stop", "rule"));
        assert!(why.contains("E-9, E-8, E-7"), "{why}");

        history.push(esc(
            "E-10",
            "stuck",
            HEARTBEAT,
            Some("retry"),
            Some("studio"),
            400,
        ));
        assert_eq!(
            decide(&open, &history, None),
            None,
            "one overrule retires the rule"
        );

        let lost = esc("E-21", "stuck", LOST, None, None, 1000);
        let (a, src, _) = decide(&lost, &history, Some("retry")).unwrap();
        assert_eq!((a.as_str(), src), ("retry", "standing"));
        assert_eq!(
            decide(&lost, &history, Some("merge")),
            None,
            "not an option"
        );

        let merges: Vec<Escalation> = (1..=5)
            .map(|i| {
                esc(
                    &format!("E-{i}"),
                    "merge",
                    "approved",
                    Some("merge"),
                    Some("studio"),
                    i,
                )
            })
            .collect();
        let mut m = esc("E-30", "merge", "approved", None, None, 1000);
        m.options = vec!["merge".into(), "skip".into()];
        assert_eq!(
            decide(&m, &merges, Some("merge")),
            None,
            "never the allow list"
        );
        let b = esc("E-31", "budget", "", None, None, 1000);
        assert_eq!(decide(&b, &[], Some("retry")), None, "never a budget");
    }

    #[test]
    fn the_window_closes_before_the_deadline() {
        assert_eq!(due_at(1000, 1000 + 3600, 10), 1600);
        assert_eq!(due_at(1000, 1000 + 300, 10), 1000 + 240);
        assert_eq!(due_at(1000, 1030, 10), 1000, "never in the past");
    }

    #[test]
    fn a_plan_is_judged_against_what_happened() {
        let p = Plan {
            escalation: "E-5".into(),
            for_created_at: 100,
            class: "provider".into(),
            answer: "retry".into(),
            source: "rule".into(),
            reason: "r".into(),
            planned_at: 100,
            due_at: 700,
        };
        assert_eq!(
            outcome(&p, Some(&esc("E-5", "stuck", LOST, None, None, 100))),
            "pending"
        );
        assert_eq!(
            outcome(
                &p,
                Some(&esc(
                    "E-5",
                    "stuck",
                    LOST,
                    Some("retry"),
                    Some("shadow"),
                    100
                ))
            ),
            "answered"
        );
        assert_eq!(
            outcome(
                &p,
                Some(&esc(
                    "E-5",
                    "stuck",
                    LOST,
                    Some("retry"),
                    Some("phone"),
                    100
                ))
            ),
            "agreed"
        );
        assert_eq!(
            outcome(
                &p,
                Some(&esc(
                    "E-5",
                    "stuck",
                    LOST,
                    Some("stop"),
                    Some("studio"),
                    100
                ))
            ),
            "overruled"
        );
        assert_eq!(
            outcome(&p, Some(&esc("E-5", "stuck", LOST, None, None, 999))),
            "expired",
            "a reused id is another question"
        );
    }

    #[test]
    fn a_pass_plans_then_answers_after_the_window_unless_the_owner_answers_first() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let ask = |q: &str| {
            let (e, _) = escalation::raise(
                Some("G-1"),
                Some("M-1"),
                "stuck",
                q,
                &["retry", "stop"],
                "stop",
            )
            .unwrap();
            e
        };
        for _ in 0..3 {
            let e = ask(LOST);
            escalation::answer(&e.id, "retry", "studio").unwrap();
        }
        let e = ask(LOST);
        let now = e.created_at;
        let log = step(now).unwrap();
        assert_eq!(log.len(), 1, "{log:?}");
        let p = plan_of(&e.id).unwrap();
        assert_eq!((p.answer.as_str(), p.source.as_str()), ("retry", "rule"));
        assert_eq!(p.due_at, now + DEFAULT_WINDOW_MINUTES * 60);

        assert!(step(now + 60).unwrap().is_empty(), "inside the window");
        assert_eq!(escalation::get(&e.id).unwrap().unwrap().status, "open");
        step(p.due_at).unwrap();
        let done = escalation::get(&e.id).unwrap().unwrap();
        assert_eq!(done.answered_by.as_deref(), Some("shadow"));
        assert_eq!(done.answer.as_deref(), Some("retry"));

        // The owner answers inside the window: his answer stands and is recorded as an overrule.
        let e2 = ask(LOST);
        step(e2.created_at).unwrap();
        escalation::answer(&e2.id, "stop", "phone").unwrap();
        step(e2.created_at + 3600).unwrap();
        let s = status().unwrap();
        let rec = s.record.iter().find(|r| r.escalation == e2.id).unwrap();
        assert_eq!(rec.outcome, "overruled");
        assert_eq!(s.agreement, Some(0.0));
        assert_eq!(
            s.rules
                .iter()
                .find(|r| r.class == "provider")
                .unwrap()
                .learned,
            None,
            "his overrule broke the streak"
        );

        // Turned off, Shadow plans nothing.
        set(Some(false), None, None).unwrap();
        let e3 = ask(LOST);
        assert!(step(e3.created_at).unwrap().is_empty());
        assert!(plan_of(&e3.id).is_none());
        crate::store::set_test_db(None);
    }

    #[test]
    fn standing_answers_are_retry_or_stop_and_never_for_the_owner_s_switches() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let s = set(None, Some(5), Some(("provider".into(), "retry".into()))).unwrap();
        assert_eq!(s.window_minutes, 5);
        let r = s.rules.iter().find(|r| r.class == "provider").unwrap();
        assert_eq!(r.standing.as_deref(), Some("retry"));
        assert!(set(None, None, Some(("budget".into(), "retry".into()))).is_err());
        assert!(set(None, None, Some(("provider".into(), "merge".into()))).is_err());
        assert!(set(None, Some(0), None).is_err());
        let s = set(None, None, Some(("provider".into(), "".into()))).unwrap();
        assert_eq!(
            s.rules
                .iter()
                .find(|r| r.class == "provider")
                .unwrap()
                .standing,
            None
        );
        crate::store::set_test_db(None);
    }
}
