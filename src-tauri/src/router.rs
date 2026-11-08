//! The router.
//!
//! It scores models from what actually happened in past missions: how often the reviewer approved,
//! and what the work cost. By default it only shows what it would choose (shadow). With
//! "Let the router pick" on, a new Hermes mission that has no explicit model and no
//! `.agentos/routing.json` entry for its role takes the router's pick, but only when that pick has
//! enough evidence and a clear lead; the reason is recorded on the mission. It never changes the
//! global model, a mission that is already pinned, or a running mission.
//!
//! With few samples the score is smoothed toward the middle, because one approval out of one is not
//! evidence. The smoothing falls away as samples accumulate.

use anyhow::Result;
use serde::Serialize;

use crate::missions;

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
pub struct Suggestion {
    pub role: String,
    pub provider: String,
    pub model: String,
    pub attempts: usize,
    pub approvals: usize,
    pub score: f64,
    pub avg_cost_usd: f64,
    pub reason: String,
}

/// Prior strength: how many imaginary neutral outcomes every candidate starts with.
const PRIOR: f64 = 2.0;

/// Laplace-smoothed approval rate, minus a small cost penalty. Pure, so it is testable.
pub fn score(approvals: usize, attempts: usize, avg_cost: f64) -> f64 {
    let rate = (approvals as f64 + PRIOR * 0.5) / (attempts as f64 + PRIOR);
    let cost_penalty = (avg_cost / 5.0).min(0.3);
    (rate * 100.0) - cost_penalty
}

pub fn suggestions() -> Result<Vec<Suggestion>> {
    Ok(suggestions_from(&missions::list(500)?))
}

/// What the router decides right now for one role: the pin, or why it declines.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Verdict {
    pub role: String,
    pub pick: Option<Suggestion>,
    pub decline: Option<String>,
}

/// The router's current decision for every role that has Hermes history. Pure.
pub fn verdicts_from(all: &[missions::Mission], now: i64) -> Vec<Verdict> {
    let mut roles: Vec<String> = all
        .iter()
        .filter(|m| m.agent == "hermes")
        .map(|m| m.role.clone())
        .collect();
    roles.sort();
    roles.dedup();
    roles
        .into_iter()
        .map(|role| match pick(all, &role, now) {
            Ok(s) => Verdict {
                role,
                pick: Some(s),
                decline: None,
            },
            Err(why) => Verdict {
                role,
                pick: None,
                decline: Some(why),
            },
        })
        .collect()
}

pub fn verdicts() -> Result<Vec<Verdict>> {
    Ok(verdicts_from(
        &missions::list(500)?,
        chrono::Utc::now().timestamp(),
    ))
}

/// Scores from a set of missions. Pure.
pub fn suggestions_from(all: &[missions::Mission]) -> Vec<Suggestion> {
    let mut buckets: std::collections::BTreeMap<(String, String, String), (usize, usize, f64)> =
        std::collections::BTreeMap::new();
    for m in all
        .iter()
        .filter(|m| m.status == "done" || m.review_verdict.is_some())
    {
        let key = (m.role.clone(), m.provider.clone(), m.model.clone());
        let e = buckets.entry(key).or_insert((0, 0, 0.0));
        e.0 += 1;
        if m.review_verdict.as_deref() == Some("approve") {
            e.1 += 1;
        }
        e.2 += m.spent_usd;
    }
    let mut out: Vec<Suggestion> = buckets
        .into_iter()
        .map(|((role, provider, model), (attempts, approvals, spent))| {
            let avg = if attempts > 0 {
                spent / attempts as f64
            } else {
                0.0
            };
            let s = score(approvals, attempts, avg);
            let reason = if attempts < 3 {
                format!("{approvals}/{attempts} approved, too few samples to trust yet")
            } else {
                format!("{approvals}/{attempts} approved, ${avg:.2} average")
            };
            Suggestion {
                role,
                provider,
                model: if model.is_empty() {
                    "inherit".into()
                } else {
                    model
                },
                attempts,
                approvals,
                score: (s * 10.0).round() / 10.0,
                avg_cost_usd: avg,
                reason,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.role.cmp(&b.role).then_with(|| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    out
}

/// Decided missions a candidate needs before the router may pin it.
pub const MIN_EVIDENCE: usize = 5;
/// Points the best candidate must lead the next one by.
pub const MIN_LEAD: f64 = 5.0;
pub const KV_AUTO: &str = "router.auto";

pub fn auto_enabled() -> bool {
    crate::store::open()
        .ok()
        .and_then(|c| crate::store::kv_get(&c, KV_AUTO).ok().flatten())
        .is_some_and(|v| v.trim() == "1")
}

pub fn set_auto(on: bool) -> Result<()> {
    let c = crate::store::open()?;
    crate::store::kv_set(&c, KV_AUTO, if on { "1" } else { "0" })
}

/// Only outcomes this recent count: models, prices and prompts change.
pub const WINDOW_DAYS: i64 = 60;

fn decided(m: &missions::Mission) -> bool {
    m.status == "done" || m.review_verdict.is_some()
}

/// The router's pick for a role among Hermes missions, or why it declines. Evidence is the role's
/// own decided missions from the last 60 days: at least 5 of them, at least 5 for the winning
/// candidate, and a clear lead over the next. Pure.
pub fn pick(
    all: &[missions::Mission],
    role: &str,
    now: i64,
) -> std::result::Result<Suggestion, String> {
    let since = now - WINDOW_DAYS * 86_400;
    let hermes: Vec<missions::Mission> = all
        .iter()
        .filter(|m| m.agent == "hermes" && m.role == role && m.updated_at >= since && decided(m))
        .cloned()
        .collect();
    if hermes.len() < MIN_EVIDENCE {
        return Err(format!(
            "{} decided Hermes {role} missions in the last {WINDOW_DAYS} days, needs {MIN_EVIDENCE}",
            hermes.len()
        ));
    }
    let ranked: Vec<Suggestion> = suggestions_from(&hermes);
    let Some(best) = ranked.first() else {
        return Err(format!("no decided Hermes {role} missions yet"));
    };
    if best.attempts < MIN_EVIDENCE {
        return Err(format!(
            "best candidate {} has {} decided missions, needs {MIN_EVIDENCE}",
            best.model, best.attempts
        ));
    }
    if best.model == "inherit" || best.provider.is_empty() {
        return Err("the best record is the inherited default, so nothing to pin".into());
    }
    if let Some(next) = ranked.get(1) {
        if best.score - next.score < MIN_LEAD {
            return Err(format!(
                "{} leads {} by {:.1} points, needs {MIN_LEAD}",
                best.model,
                next.model,
                best.score - next.score
            ));
        }
    }
    Ok(best.clone())
}

/// Where a new mission's provider and model come from.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
pub struct Pin {
    pub provider: String,
    pub model: String,
    /// `explicit`, `routing.json`, `router` or `inherit`.
    pub source: String,
    pub reason: String,
    /// What the router would have chosen, or why not, whether or not it was allowed to act.
    /// Kept so its judgement can be compared with what actually happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<String>,
}

/// One line for the shadow log. Pure.
pub fn shadow_line(r: &std::result::Result<Suggestion, String>) -> String {
    match r {
        Ok(s) => format!(
            "would pick {}:{} ({}, score {})",
            s.provider, s.model, s.reason, s.score
        ),
        Err(why) => format!("would decline: {why}"),
    }
}

/// Resolve a new mission's pin. An explicit choice always wins, then the repository's committed
/// policy, then the router when it is allowed and confident. Pure.
pub fn resolve_pin(
    provider: &str,
    model: &str,
    policy: Option<&crate::routing::RoleChoice>,
    router: Option<std::result::Result<Suggestion, String>>,
) -> Pin {
    if !provider.trim().is_empty() || !model.trim().is_empty() {
        return Pin {
            provider: provider.trim().into(),
            model: model.trim().into(),
            source: "explicit".into(),
            reason: String::new(),
            shadow: None,
        };
    }
    if let Some(p) = policy.filter(|p| !p.provider.trim().is_empty()) {
        return Pin {
            provider: p.provider.trim().into(),
            model: p.model.trim().into(),
            source: "routing.json".into(),
            reason: "the repository's .agentos/routing.json".into(),
            shadow: None,
        };
    }
    match router {
        Some(Ok(s)) => Pin {
            reason: format!("router: {} (score {})", s.reason, s.score),
            provider: s.provider,
            model: s.model,
            source: "router".into(),
            shadow: None,
        },
        Some(Err(why)) => Pin {
            provider: String::new(),
            model: String::new(),
            source: "inherit".into(),
            reason: format!("router declined: {why}"),
            shadow: None,
        },
        None => Pin {
            provider: String::new(),
            model: String::new(),
            source: "inherit".into(),
            reason: String::new(),
            shadow: None,
        },
    }
}

/// Create a mission with the shared precedence (explicit, routing.json, router, inherit) and
/// record how its model was chosen. Used by the New mission form and by Notion.
#[allow(clippy::too_many_arguments)]
pub fn create_mission(
    repo: &str,
    goal: &str,
    acceptance: &str,
    role: &str,
    agent: &str,
    provider: &str,
    model: &str,
    budget_usd: f64,
) -> Result<missions::Mission> {
    let policy = crate::routing::defaults_for(&crate::review_agent::repo_dir(repo), role);
    // Always judge Hermes missions, so the shadow log exists even when the router may not act.
    let judged = (agent.trim() == "hermes")
        .then(|| {
            missions::list(500)
                .map(|all| pick(&all, role, chrono::Utc::now().timestamp()))
                .ok()
        })
        .flatten();
    let router = judged.clone().filter(|_| auto_enabled());
    let mut pin = resolve_pin(provider, model, policy.as_ref(), router);
    pin.shadow = judged.as_ref().map(shadow_line);
    let m = missions::create(
        repo,
        goal,
        acceptance,
        role,
        agent,
        &pin.provider,
        &pin.model,
        budget_usd,
    )?;
    record_pin(&m.id, &pin);
    Ok(m)
}

fn pin_key(id: &str) -> String {
    format!("mission.pin.{id}")
}

pub fn record_pin(id: &str, pin: &Pin) {
    if pin.source == "explicit" && pin.shadow.is_none() {
        return;
    }
    if let (Ok(c), Ok(v)) = (crate::store::open(), serde_json::to_string(pin)) {
        let _ = crate::store::kv_set(&c, &pin_key(id), &v);
    }
}

/// How each listed mission got its model, for the ones Studio chose.
pub fn pins(ids: &[String]) -> std::collections::BTreeMap<String, Pin> {
    let Ok(c) = crate::store::open() else {
        return Default::default();
    };
    ids.iter()
        .filter_map(|id| {
            let raw = crate::store::kv_get(&c, &pin_key(id)).ok()??;
            let pin: Pin = serde_json::from_str(&raw).ok()?;
            Some((id.clone(), pin))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn evidence_beats_a_single_lucky_run() {
        let lucky = score(1, 1, 0.0);
        let proven = score(8, 10, 0.0);
        assert!(proven > lucky, "{proven} should beat {lucky}");
    }

    #[test]
    fn cost_penalises_without_dominating() {
        let cheap = score(3, 4, 0.0);
        let dear = score(3, 4, 50.0);
        assert!(dear < cheap);
        assert!(dear > 50.0, "cost must not erase a good approval rate");
    }

    fn m(
        id: &str,
        role: &str,
        agent: &str,
        provider: &str,
        model: &str,
        verdict: &str,
    ) -> missions::Mission {
        missions::Mission {
            id: id.into(),
            repo: "r".into(),
            goal: "g".into(),
            acceptance: "a".into(),
            role: role.into(),
            agent: agent.into(),
            status: "done".into(),
            provider: provider.into(),
            model: model.into(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: None,
            reviewer: None,
            review_verdict: Some(verdict.into()),
            pr_url: None,
            review_note: None,
            created_at: 0,
            updated_at: NOW - 86_400,
            verify: None,
        }
    }

    fn history(n_good: usize, n_bad: usize, model: &str) -> Vec<missions::Mission> {
        (0..n_good)
            .map(|i| {
                m(
                    &format!("g{model}{i}"),
                    "implementer",
                    "hermes",
                    "openrouter",
                    model,
                    "approve",
                )
            })
            .chain((0..n_bad).map(|i| {
                m(
                    &format!("b{model}{i}"),
                    "implementer",
                    "hermes",
                    "openrouter",
                    model,
                    "request_changes",
                )
            }))
            .collect()
    }

    #[test]
    fn the_router_picks_only_with_evidence_and_a_lead() {
        let few = history(3, 0, "a");
        assert!(pick(&few, "implementer", NOW)
            .unwrap_err()
            .contains("needs 5"));
        let mut close = history(5, 1, "a");
        close.extend(history(5, 1, "b"));
        assert!(pick(&close, "implementer", NOW)
            .unwrap_err()
            .contains("leads"));
        let mut clear = history(9, 1, "a");
        clear.extend(history(2, 6, "b"));
        assert_eq!(pick(&clear, "implementer", NOW).unwrap().model, "a");
        assert!(
            pick(&clear, "reviewer", NOW).is_err(),
            "other roles are separate"
        );
        let claude: Vec<_> = history(9, 0, "a")
            .into_iter()
            .map(|mut x| {
                x.agent = "claude-code".into();
                x
            })
            .collect();
        assert!(
            pick(&claude, "implementer", NOW).is_err(),
            "only Hermes missions are auto-pinned"
        );
    }

    #[test]
    fn evidence_is_per_role_and_recent() {
        // Plenty of approvals, but older than 60 days: no pick.
        let old: Vec<_> = history(9, 1, "a")
            .into_iter()
            .map(|mut x| {
                x.updated_at = NOW - 61 * 86_400;
                x
            })
            .collect();
        assert!(pick(&old, "implementer", NOW)
            .unwrap_err()
            .contains("in the last 60 days"));
        // Other roles' missions do not count toward this role.
        let mut mixed: Vec<_> = history(9, 0, "a")
            .into_iter()
            .map(|mut x| {
                x.role = "reviewer".into();
                x
            })
            .collect();
        mixed.extend(history(4, 0, "a"));
        assert!(pick(&mixed, "implementer", NOW)
            .unwrap_err()
            .contains("4 decided"));
        // Undecided (running, queued) missions are not evidence.
        let mut pending = history(4, 0, "a");
        pending.extend((0..3).map(|i| {
            let mut x = m(
                &format!("q{i}"),
                "implementer",
                "hermes",
                "openrouter",
                "a",
                "approve",
            );
            x.status = "running".into();
            x.review_verdict = None;
            x
        }));
        assert!(pick(&pending, "implementer", NOW).is_err());
    }

    #[test]
    fn verdicts_decline_until_the_evidence_is_there() {
        let few = verdicts_from(&history(3, 0, "a"), NOW);
        assert_eq!(few.len(), 1);
        assert!(few[0].pick.is_none());
        assert!(
            few[0].decline.as_deref().unwrap().contains("needs 5"),
            "{few:?}"
        );
        let many = verdicts_from(&history(9, 1, "a"), NOW);
        assert_eq!(
            many[0].pick.as_ref().map(|s| s.model.as_str()),
            Some("a"),
            "{many:?}"
        );
    }

    #[test]
    fn the_shadow_log_says_what_it_would_do() {
        let s = Suggestion {
            role: "implementer".into(),
            provider: "openrouter".into(),
            model: "a".into(),
            attempts: 9,
            approvals: 8,
            score: 80.0,
            avg_cost_usd: 0.0,
            reason: "8/9 approved".into(),
        };
        assert_eq!(
            shadow_line(&Ok(s)),
            "would pick openrouter:a (8/9 approved, score 80)"
        );
        assert_eq!(
            shadow_line(&Err("too few".into())),
            "would decline: too few"
        );
    }

    #[test]
    fn an_explicit_choice_beats_policy_beats_router() {
        let policy = crate::routing::RoleChoice {
            provider: "nous".into(),
            model: "x".into(),
        };
        let sug = Suggestion {
            role: "implementer".into(),
            provider: "openrouter".into(),
            model: "a".into(),
            attempts: 9,
            approvals: 8,
            score: 80.0,
            avg_cost_usd: 0.1,
            reason: "8/9 approved".into(),
        };
        assert_eq!(
            resolve_pin("p", "m", Some(&policy), Some(Ok(sug.clone()))).source,
            "explicit"
        );
        assert_eq!(
            resolve_pin("", "", Some(&policy), Some(Ok(sug.clone()))).source,
            "routing.json"
        );
        let r = resolve_pin("", "", None, Some(Ok(sug)));
        assert_eq!((r.source.as_str(), r.model.as_str()), ("router", "a"));
        let d = resolve_pin("", "", None, Some(Err("too few".into())));
        assert_eq!((d.source.as_str(), d.model.as_str()), ("inherit", ""));
        assert!(d.reason.contains("too few"));
        assert_eq!(resolve_pin("", "", None, None).reason, "");
    }

    #[test]
    fn empty_history_is_neutral_not_confident() {
        let s = score(0, 0, 0.0);
        assert!((s - 50.0).abs() < 1.0, "got {s}");
    }
}
