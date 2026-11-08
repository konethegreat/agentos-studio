//! Quota-aware assignment: the router picks the agent, not only the model.
//!
//! A mission created for agent `auto` gets its agent when it starts (`missions::start`, the single
//! dispatch path for the board button and the scheduler). The decision is deterministic and logged:
//!
//! 1. Hard constraints: installed, runs headless, has every tool the goal needs (a goal that names
//!    Notion, kanban or cron needs Hermes).
//! 2. Measured success for this task class (the mission's role): the router's smoothed approval
//!    score per agent, counted only with at least `MIN_ROLE_EVIDENCE` decided missions, compared in
//!    10-point bands so noise does not reorder agents.
//! 3. Cost: a flat subscription with headroom first, then local, then metered.
//!
//! When the best choice would spend metered money only because the flat subscriptions are out of
//! headroom, a mission that is not marked urgent is deferred until the earliest window resets
//! instead. Nothing here calls a model to decide.

use serde::Serialize;

use crate::agents::{self, AgentCard};
use crate::missions::Mission;

pub const AUTO: &str = "auto";
pub const MIN_ROLE_EVIDENCE: usize = 3;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum Assignment {
    Agent { agent: String, reason: String },
    Defer { until: i64, reason: String },
    Refuse(String),
}

/// Tools a goal needs beyond git, read from its words. Pure.
pub fn required_tools(goal: &str, acceptance: &str) -> Vec<&'static str> {
    let text = format!("{goal} {acceptance}").to_lowercase();
    let mut out = Vec::new();
    if text.contains("notion") {
        out.push("notion");
    }
    if text.contains("kanban") || text.contains("cron") || text.contains("hermes cli") {
        out.push("kanban");
    }
    out
}

/// 0 flat with headroom, 1 local, 2 metered, 3 flat but out of headroom. Pure.
pub fn cost_rank(c: &AgentCard) -> u8 {
    match c.cost_model.as_str() {
        "flat" if agents::has_headroom(&c.quota) => 0,
        "flat" => 3,
        "local" => 1,
        _ => 2,
    }
}

/// Preference among equals: the strongest coder on a flat plan first. Pure.
fn preference(agent: &str) -> u8 {
    match agent {
        "claude-code" => 0,
        "antigravity" => 1,
        "hermes" => 2,
        _ => 3,
    }
}

/// Approval band of an agent for a role, from decided missions it implemented. Pure.
pub fn success_band(history: &[Mission], agent: &str, role: &str) -> (i64, usize) {
    let decided: Vec<&Mission> = history
        .iter()
        .filter(|m| m.agent.eq_ignore_ascii_case(agent) && m.role == role)
        .filter(|m| m.review_verdict.is_some())
        .collect();
    let n = decided.len();
    if n < MIN_ROLE_EVIDENCE {
        return (5, n);
    }
    let ok = decided
        .iter()
        .filter(|m| m.review_verdict.as_deref() == Some("approve"))
        .count();
    ((crate::router::score(ok, n, 0.0) / 10.0).floor() as i64, n)
}

/// The earliest reset among flat agents that are out of headroom, in epoch seconds. Pure.
fn earliest_reset(cards: &[&AgentCard]) -> Option<i64> {
    cards
        .iter()
        .filter(|c| cost_rank(c) == 3)
        .filter_map(|c| c.quota.resets_at.as_deref())
        .filter_map(|r| chrono::DateTime::parse_from_rfc3339(r).ok())
        .map(|d| d.timestamp())
        .min()
}

/// Choose the agent for a mission. Pure over its inputs.
pub fn assign(
    m: &Mission,
    cards: &[AgentCard],
    history: &[Mission],
    urgent: bool,
    now: i64,
) -> Assignment {
    let needs = required_tools(&m.goal, &m.acceptance);
    let fit: Vec<&AgentCard> = cards
        .iter()
        .filter(|c| c.installed && c.headless)
        .filter(|c| needs.iter().all(|t| c.tools.iter().any(|x| x == t)))
        .collect();
    if fit.is_empty() {
        return Refuse(if needs.is_empty() {
            "no installed headless agent can take this mission".into()
        } else {
            format!("no installed headless agent has {}", needs.join(" and "))
        });
    }
    let mut ranked: Vec<(&AgentCard, i64, usize, u8)> = fit
        .iter()
        .map(|c| {
            let (band, n) = success_band(history, &c.name, &m.role);
            (*c, band, n, cost_rank(c))
        })
        .collect();
    // Success band first (higher is better), then cost, then preference.
    ranked.sort_by_key(|(c, band, _, cost)| (std::cmp::Reverse(*band), *cost, preference(&c.name)));
    let runnable: Vec<&(&AgentCard, i64, usize, u8)> = ranked.iter().filter(|r| r.3 < 3).collect();
    let Some(best) = runnable.first() else {
        let until = earliest_reset(&fit).unwrap_or(now + 3600);
        return Defer {
            until,
            reason: "every fitting agent is out of quota headroom; waiting for the reset".into(),
        };
    };
    let (card, band, n, cost) = **best;
    // Metered spend only because flat plans are exhausted: wait unless the owner marked it urgent.
    if cost == 2 && !urgent {
        if let Some(until) = earliest_reset(&fit).filter(|u| *u > now) {
            return Defer {
                until,
                reason: format!(
                    "the flat subscriptions are out of headroom and {} is metered; not urgent, so it waits for the reset",
                    card.name
                ),
            };
        }
    }
    let evidence = if n >= MIN_ROLE_EVIDENCE {
        format!("{n} decided {} missions, score band {band}0", m.role)
    } else {
        format!(
            "fewer than {MIN_ROLE_EVIDENCE} decided {} missions, so success is neutral",
            m.role
        )
    };
    let cost_text = match cost {
        0 => format!("flat subscription with headroom ({})", card.quota.summary),
        1 => "local".to_string(),
        _ => "metered (marked urgent)".to_string(),
    };
    let tools = if needs.is_empty() {
        String::new()
    } else {
        format!("; needs {}", needs.join(", "))
    };
    Agent {
        agent: card.name.clone(),
        reason: format!("{}: {evidence}; {cost_text}{tools}", card.name),
    }
}

use Assignment::{Agent, Defer, Refuse};

fn kv_key(kind: &str, id: &str) -> String {
    format!("mission.{kind}.{id}")
}

pub fn urgent(id: &str) -> bool {
    crate::store::open()
        .ok()
        .and_then(|c| {
            crate::store::kv_get(&c, &kv_key("urgent", id))
                .ok()
                .flatten()
        })
        .as_deref()
        == Some("1")
}

pub fn set_urgent(id: &str, on: bool) -> anyhow::Result<()> {
    let c = crate::store::open()?;
    if on {
        crate::store::kv_set(&c, &kv_key("urgent", id), "1")
    } else {
        crate::store::kv_delete(&c, &kv_key("urgent", id))
    }
}

/// Until when a deferred mission waits, if it does.
pub fn deferred_until(id: &str) -> Option<i64> {
    crate::store::open()
        .ok()
        .and_then(|c| {
            crate::store::kv_get(&c, &kv_key("defer", id))
                .ok()
                .flatten()
        })
        .and_then(|v| v.parse().ok())
}

/// Resolve an `auto` mission's agent now, with live cards. Records the decision in the exchange
/// log; a deferral is recorded once per wait.
pub fn resolve(m: &Mission) -> anyhow::Result<String> {
    let now = crate::store::now();
    let cards = agents::cards(false);
    let history = crate::missions::list(1000).unwrap_or_default();
    match assign(m, &cards, &history, urgent(&m.id), now) {
        Agent { agent, reason } => {
            crate::missions::set_agent(&m.id, &agent)?;
            if let Ok(c) = crate::store::open() {
                let _ = crate::store::kv_delete(&c, &kv_key("defer", &m.id));
            }
            let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
                mission_id: Some(&m.id),
                kind: "assign",
                from_agent: "router",
                to_agent: &agent,
                summary: &reason,
                ..Default::default()
            });
            Ok(agent)
        }
        Defer { until, reason } => {
            if deferred_until(&m.id) != Some(until) {
                if let Ok(c) = crate::store::open() {
                    let _ = crate::store::kv_set(&c, &kv_key("defer", &m.id), &until.to_string());
                }
                let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
                    mission_id: Some(&m.id),
                    kind: "defer",
                    from_agent: "router",
                    summary: &reason,
                    detail: &format!("until {until}"),
                    ..Default::default()
                });
            }
            anyhow::bail!("{}: deferred until {until}: {reason}", m.id)
        }
        Refuse(reason) => anyhow::bail!("{}: {reason}", m.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{build, parse_usage, Inputs};

    fn mission(goal: &str, role: &str) -> Mission {
        Mission {
            id: "M-020".into(),
            repo: "r".into(),
            goal: goal.into(),
            acceptance: "a".into(),
            role: role.into(),
            agent: AUTO.into(),
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
        }
    }

    fn quota(used: f64) -> crate::agents::Quota {
        parse_usage(
            &format!(r#"{{"windows":[{{"label":"Current session","used_percent":{used},"resets_at":"2026-11-08T22:00:00+00:00"}}]}}"#),
            0,
        )
        .unwrap()
    }

    fn card(agent: &str, provider: &str, used: Option<f64>) -> AgentCard {
        build(
            agent,
            &Inputs {
                installed: true,
                provider: provider.into(),
                quota: used.map(quota).unwrap_or_default(),
                hub_identity: true,
                ..Default::default()
            },
        )
    }

    fn fleet(claude_used: f64, hermes_provider: &str, hermes_used: Option<f64>) -> Vec<AgentCard> {
        vec![
            card("hermes", hermes_provider, hermes_used),
            card("claude-code", "claude-subscription", Some(claude_used)),
            card("qoder", "qoder", None),
        ]
    }

    fn agent_of(a: &Assignment) -> &str {
        match a {
            Agent { agent, .. } => agent,
            other => panic!("expected an agent, got {other:?}"),
        }
    }

    #[test]
    fn hard_constraints_come_first() {
        let cards = fleet(10.0, "claude-subscription", Some(10.0));
        let a = assign(
            &mission("Update the Notion board with the release", "implementer"),
            &cards,
            &[],
            false,
            0,
        );
        assert_eq!(agent_of(&a), "hermes", "only Hermes has Notion");
        let a = assign(
            &mission("Add a function", "implementer"),
            &cards,
            &[],
            false,
            0,
        );
        assert_eq!(
            agent_of(&a),
            "claude-code",
            "flat, headroom, preferred coder"
        );
        let ide_only = vec![card("qoder", "qoder", None)];
        assert!(matches!(
            assign(&mission("x", "implementer"), &ide_only, &[], false, 0),
            Refuse(_)
        ));
    }

    #[test]
    fn measured_success_for_the_role_outranks_preference() {
        let cards = fleet(10.0, "claude-subscription", Some(10.0));
        let mut history = Vec::new();
        for verdict in ["approve", "approve", "approve", "approve"] {
            let mut m = mission("x", "implementer");
            m.agent = "hermes".into();
            m.review_verdict = Some(verdict.into());
            history.push(m);
        }
        for verdict in ["reject", "reject", "approve"] {
            let mut m = mission("x", "implementer");
            m.agent = "claude-code".into();
            m.review_verdict = Some(verdict.into());
            history.push(m);
        }
        let a = assign(
            &mission("Add a function", "implementer"),
            &cards,
            &history,
            false,
            0,
        );
        assert_eq!(agent_of(&a), "hermes");
        // Another role has no evidence, so preference and cost decide.
        let a = assign(
            &mission("Add a function", "reviewer"),
            &cards,
            &history,
            false,
            0,
        );
        assert_eq!(agent_of(&a), "claude-code");
    }

    #[test]
    fn exhausted_flat_plans_defer_non_urgent_work_instead_of_paying() {
        // Claude out of headroom, Hermes on metered OpenRouter.
        let cards = fleet(97.0, "openrouter", None);
        let reset = chrono::DateTime::parse_from_rfc3339("2026-11-08T22:00:00+00:00")
            .unwrap()
            .timestamp();
        let a = assign(
            &mission("Add a function", "implementer"),
            &cards,
            &[],
            false,
            reset - 600,
        );
        assert!(
            matches!(&a, Defer { until, .. } if *until == reset),
            "{a:?}"
        );
        let a = assign(
            &mission("Add a function", "implementer"),
            &cards,
            &[],
            true,
            reset - 600,
        );
        assert_eq!(agent_of(&a), "hermes", "urgent work may spend");
        // With Antigravity installed (flat, no reported quota) the work goes there instead.
        let mut with_agy = cards.clone();
        with_agy.push(card("antigravity", "antigravity", None));
        let a = assign(
            &mission("Add a function", "implementer"),
            &with_agy,
            &[],
            false,
            reset - 600,
        );
        assert_eq!(agent_of(&a), "antigravity");
        // Everything exhausted: wait for the reset.
        let only_claude = vec![card("claude-code", "claude-subscription", Some(99.0))];
        assert!(matches!(
            assign(&mission("x", "implementer"), &only_claude, &[], true, 0),
            Defer { .. }
        ));
    }

    #[test]
    fn goals_name_the_tools_they_need() {
        assert_eq!(required_tools("Sync to Notion", ""), vec!["notion"]);
        assert_eq!(required_tools("Add a cron job", ""), vec!["kanban"]);
        assert!(required_tools("Fix the parser", "tests pass").is_empty());
    }
}
