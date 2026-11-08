//! The agent registry ("agent cards"): one record per agent Studio can address.
//!
//! A card says how the agent starts (a headless command, or IDE only), what it is good at (from
//! settings first, then from measured mission outcomes), its cost model (flat subscription,
//! metered or local), its current quota headroom (`hermes usage --json` rate-limit windows for
//! subscriptions, OpenRouter credits for metered spend) and its health. The Agents screen and the
//! `agents` hub tool both read `cards()`, so a person and an agent see the same registry.
//!
//! Building a card is pure (`build`) over gathered inputs; only `cards()` touches the machine.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store;

/// Every agent the registry knows, in display order.
pub const AGENTS: [&str; 4] = ["hermes", "claude-code", "antigravity", "qoder"];

/// How long a quota reading stays fresh. `hermes usage` makes a network call per provider.
pub const QUOTA_TTL_SECS: i64 = 300;
/// Headroom under this percentage counts as low: non-urgent work waits for the reset.
pub const LOW_HEADROOM_PCT: f64 = 10.0;

const CLAUDE_PROVIDER: &str = "claude-subscription-directsdk-experimental";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Window {
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Quota {
    /// Where the reading came from: `hermes usage`, `openrouter credits`, or `none`.
    pub source: String,
    /// Percent left in the tightest window, when the provider reports windows.
    pub headroom_pct: Option<f64>,
    /// When the tightest window resets (RFC 3339), if known.
    pub resets_at: Option<String>,
    /// Remaining metered balance, for credit-based providers.
    pub remaining_usd: Option<f64>,
    pub windows: Vec<Window>,
    /// One line for the screen and the hub tool.
    pub summary: String,
    pub fetched_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Measured {
    /// Decided missions this agent implemented (approved or rejected).
    pub attempts: usize,
    pub approvals: usize,
    /// The router's smoothed score, 0 to 100.
    pub score: f64,
    /// Roles with at least two decided missions and an approval rate of 60 percent or more.
    pub strong_roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentCard {
    pub name: String,
    pub label: String,
    /// The command Studio starts it with, or "IDE only".
    pub start: String,
    pub headless: bool,
    pub installed: bool,
    pub strengths: Vec<String>,
    /// `settings` when the owner set them, `default` otherwise.
    pub strengths_source: String,
    /// What it can do that a mission may require: git, gh, notion, cli, kanban, browser.
    pub tools: Vec<String>,
    /// `flat`, `metered` or `local`.
    pub cost_model: String,
    pub provider: String,
    pub quota: Quota,
    pub measured: Measured,
    pub hub_identity: bool,
    /// `ready`, `ide only`, `low quota`, `read-only on the hub` or `not installed`.
    pub health: String,
    pub health_detail: String,
}

/// Everything `build` needs, gathered from the machine by `cards()` or written by a test.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub installed: bool,
    pub provider: String,
    pub quota: Quota,
    pub measured: Measured,
    pub strengths: Option<Vec<String>>,
    pub hub_identity: bool,
}

pub fn label(agent: &str) -> &'static str {
    match agent {
        "hermes" => "Hermes",
        "claude-code" => "Claude Code",
        "antigravity" => "Antigravity",
        "qoder" => "Qoder",
        _ => "Agent",
    }
}

/// Built-in strengths, used until the owner sets their own in settings. Pure.
pub fn default_strengths(agent: &str) -> Vec<String> {
    let s: &[&str] = match agent {
        "hermes" => &[
            "cli",
            "notion",
            "devops",
            "memory",
            "research",
            "scheduling",
        ],
        "claude-code" => &["coding", "refactoring", "tests", "review"],
        "antigravity" => &["coding", "frontend", "second opinion"],
        "qoder" => &["coding", "ide work"],
        _ => &[],
    };
    s.iter().map(|x| x.to_string()).collect()
}

/// What each agent can do here, for hard constraints in assignment. Pure.
pub fn default_tools(agent: &str) -> Vec<String> {
    let s: &[&str] = match agent {
        "hermes" => &["git", "gh", "cli", "notion", "kanban", "web"],
        "claude-code" => &["git", "gh", "tests"],
        "antigravity" => &["git", "gh", "tests"],
        "qoder" => &["git"],
        _ => &[],
    };
    s.iter().map(|x| x.to_string()).collect()
}

/// The command a headless agent is started with. Pure.
pub fn start_command(agent: &str) -> String {
    match agent {
        "hermes" => "hermes kanban (missions), hermes chat -Q --oneshot (asks)".into(),
        "claude-code" => {
            "claude -p --permission-mode acceptEdits --allowedTools <repo list>".into()
        }
        "antigravity" => {
            "agy --input-format stream-json --mode accept-edits --settings <file>".into()
        }
        _ => "IDE only".into(),
    }
}

/// The cost model of an agent on its provider. Pure.
pub fn cost_model(agent: &str, provider: &str) -> &'static str {
    match agent {
        "claude-code" | "antigravity" | "qoder" => "flat",
        _ => {
            let p = provider.trim().to_lowercase();
            if crate::usage::is_flat(&p) {
                "flat"
            } else if p == "custom" || p.contains("ollama") || p == "local" {
                "local"
            } else if p.is_empty() {
                "flat"
            } else {
                "metered"
            }
        }
    }
}

/// Parse `hermes usage --json` into a quota reading. The tightest window decides the headroom.
/// Pure.
pub fn parse_usage(json: &str, now: i64) -> Option<Quota> {
    let v: Value = serde_json::from_str(json.trim()).ok()?;
    let windows: Vec<Window> = v
        .get("windows")
        .and_then(Value::as_array)
        .map(|ws| {
            ws.iter()
                .filter_map(|w| {
                    Some(Window {
                        label: w.get("label")?.as_str()?.to_string(),
                        used_percent: w.get("used_percent")?.as_f64()?,
                        resets_at: w
                            .get("resets_at")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let details: Vec<String> = v
        .get("details")
        .and_then(Value::as_array)
        .map(|d| {
            d.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let unavailable = v
        .get("unavailable_reason")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());
    let tightest = windows
        .iter()
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent));
    let remaining_usd = details.iter().find_map(|d| {
        let rest = d.strip_prefix("Credits balance: $")?;
        rest.split_whitespace().next()?.parse::<f64>().ok()
    });
    let summary = if let Some(reason) = unavailable {
        format!("not reported: {reason}")
    } else if let Some(w) = tightest {
        format!(
            "{:.0}% left in {}{}",
            (100.0 - w.used_percent).max(0.0),
            w.label.to_lowercase(),
            w.resets_at
                .as_deref()
                .map(|r| format!(", resets {r}"))
                .unwrap_or_default()
        )
    } else if let Some(usd) = remaining_usd {
        format!("${usd:.2} credits left")
    } else if !details.is_empty() {
        details.join("; ")
    } else {
        "no limits reported".into()
    };
    Some(Quota {
        source: "hermes usage".into(),
        headroom_pct: tightest.map(|w| (100.0 - w.used_percent).max(0.0)),
        resets_at: tightest.and_then(|w| w.resets_at.clone()),
        remaining_usd,
        windows,
        summary,
        fetched_at: now,
    })
}

/// The quota-aware view of a card: is there room to run now? Unknown headroom counts as room,
/// because a provider that reports nothing cannot be shown to be out. Pure.
pub fn has_headroom(q: &Quota) -> bool {
    match (q.headroom_pct, q.remaining_usd) {
        (Some(h), _) => h >= LOW_HEADROOM_PCT,
        (None, Some(usd)) => usd > 0.05,
        _ => true,
    }
}

/// Assemble a card from its inputs. Pure.
pub fn build(agent: &str, i: &Inputs) -> AgentCard {
    let headless = crate::bus::is_headless(agent);
    let (strengths, strengths_source) = match i.strengths.as_ref().filter(|s| !s.is_empty()) {
        Some(s) => (s.clone(), "settings"),
        None => (default_strengths(agent), "default"),
    };
    let mut strengths = strengths;
    for r in &i.measured.strong_roles {
        let tag = format!("{r} (measured)");
        if !strengths.contains(&tag) {
            strengths.push(tag);
        }
    }
    let (health, detail) = if !i.installed {
        (
            "not installed",
            format!("{} is not installed on this machine", label(agent)),
        )
    } else if !headless {
        (
            "ide only",
            "Picks up bus work when its IDE is open; missions are reassigned after the IDE wait"
                .to_string(),
        )
    } else if !has_headroom(&i.quota) {
        (
            "low quota",
            format!("{}; non-urgent work waits for the reset", i.quota.summary),
        )
    } else if !i.hub_identity {
        (
            "read-only on the hub",
            "No agentos-hub identity yet: it can read but not claim or complete. Open Diagnostics to provision it".into(),
        )
    } else {
        ("ready", "Can take work now".into())
    };
    AgentCard {
        name: agent.to_string(),
        label: label(agent).to_string(),
        start: if headless {
            start_command(agent)
        } else {
            "IDE only".into()
        },
        headless,
        installed: i.installed,
        strengths,
        strengths_source: strengths_source.into(),
        tools: default_tools(agent),
        cost_model: cost_model(agent, &i.provider).into(),
        provider: i.provider.clone(),
        quota: i.quota.clone(),
        measured: i.measured.clone(),
        hub_identity: i.hub_identity,
        health: health.into(),
        health_detail: detail,
    }
}

/// Attempts, approvals, and the same two per role.
type Tally = (usize, usize, BTreeMap<String, (usize, usize)>);

/// Measured outcomes per implementing agent from decided missions. Pure.
pub fn measure(all: &[crate::missions::Mission]) -> BTreeMap<String, Measured> {
    let mut by: BTreeMap<String, Tally> = BTreeMap::new();
    for m in all
        .iter()
        .filter(|m| m.review_verdict.is_some() || m.status == "done")
    {
        let e = by.entry(m.agent.trim().to_lowercase()).or_default();
        let ok = m.review_verdict.as_deref() == Some("approve");
        e.0 += 1;
        e.1 += usize::from(ok);
        let r = e.2.entry(m.role.clone()).or_default();
        r.0 += 1;
        r.1 += usize::from(ok);
    }
    by.into_iter()
        .map(|(agent, (attempts, approvals, roles))| {
            let strong_roles = roles
                .into_iter()
                .filter(|(_, (n, ok))| *n >= 2 && (*ok as f64) / (*n as f64) >= 0.6)
                .map(|(r, _)| r)
                .collect();
            (
                agent,
                Measured {
                    attempts,
                    approvals,
                    score: crate::router::score(approvals, attempts, 0.0),
                    strong_roles,
                },
            )
        })
        .collect()
}

fn kv_key_strengths(agent: &str) -> String {
    format!("agent.strengths.{agent}")
}

/// The owner's strengths for an agent, comma separated in kv.
pub fn configured_strengths(agent: &str) -> Option<Vec<String>> {
    let c = store::open().ok()?;
    let raw = store::kv_get(&c, &kv_key_strengths(agent)).ok().flatten()?;
    let list: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (!list.is_empty()).then_some(list)
}

pub fn set_strengths(agent: &str, strengths: &[String]) -> anyhow::Result<()> {
    if !AGENTS.contains(&agent) {
        anyhow::bail!("unknown agent '{agent}'");
    }
    let c = store::open()?;
    let clean: Vec<String> = strengths
        .iter()
        .map(|s| s.trim().replace(',', " "))
        .filter(|s| !s.is_empty())
        .collect();
    if clean.is_empty() {
        store::kv_delete(&c, &kv_key_strengths(agent))
    } else {
        store::kv_set(&c, &kv_key_strengths(agent), &clean.join(", "))
    }
}

/// Hermes' active provider from its `config.yaml`.
pub fn hermes_provider() -> String {
    std::fs::read_to_string(crate::procs::hermes_config())
        .ok()
        .and_then(|y| crate::models::parse_active(&y, "provider"))
        .unwrap_or_default()
}

fn none_quota(summary: &str) -> Quota {
    Quota {
        source: "none".into(),
        summary: summary.into(),
        fetched_at: store::now(),
        ..Default::default()
    }
}

/// A fresh or cached quota reading for a provider through `hermes usage --json`.
pub fn quota_for(provider: &str, refresh: bool) -> Quota {
    let key = format!("agents.quota.{provider}");
    let now = store::now();
    if !refresh {
        if let Some(q) = store::open()
            .ok()
            .and_then(|c| store::kv_get(&c, &key).ok().flatten())
            .and_then(|s| serde_json::from_str::<Quota>(&s).ok())
            .filter(|q| now - q.fetched_at < QUOTA_TTL_SECS)
        {
            return q;
        }
    }
    let q = crate::procs::run_with_timeout(
        "hermes",
        &["usage", "--provider", provider, "--json"],
        None,
        None,
        std::time::Duration::from_secs(25),
    )
    .ok()
    .and_then(|(_, out, _)| parse_usage(&out, now))
    .unwrap_or_else(|| none_quota("not reported by hermes usage"));
    if let Ok(c) = store::open() {
        if let Ok(s) = serde_json::to_string(&q) {
            let _ = store::kv_set(&c, &key, &s);
        }
    }
    q
}

fn agent_installed(agent: &str) -> bool {
    match agent {
        "qoder" => {
            crate::procs::qoder_settings().is_file() || crate::procs::find("qoder").is_some()
        }
        a => crate::bus::headless_available(a),
    }
}

fn has_identity(agent: &str) -> bool {
    store::open()
        .ok()
        .and_then(|c| {
            store::kv_get(&c, &format!("hub.token.{agent}"))
                .ok()
                .flatten()
        })
        .is_some()
}

/// Every agent card, read from this machine. `refresh` forces new quota readings.
pub fn cards(refresh: bool) -> Vec<AgentCard> {
    let measured = measure(&crate::missions::list(1000).unwrap_or_default());
    let hp = hermes_provider();
    AGENTS
        .iter()
        .map(|a| {
            let provider = match *a {
                "hermes" => hp.clone(),
                "claude-code" => CLAUDE_PROVIDER.to_string(),
                "antigravity" => "antigravity".to_string(),
                _ => "qoder".to_string(),
            };
            let installed = agent_installed(a);
            let quota = match *a {
                _ if !installed => none_quota("not installed"),
                "claude-code" => quota_for(CLAUDE_PROVIDER, refresh),
                "hermes" if !hp.is_empty() => quota_for(&hp, refresh),
                _ => none_quota("this agent reports no quota to Studio"),
            };
            build(
                a,
                &Inputs {
                    installed,
                    provider,
                    quota,
                    measured: measured.get(*a).cloned().unwrap_or_default(),
                    strengths: configured_strengths(a),
                    hub_identity: has_identity(a),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const USAGE: &str = r#"{
      "provider": "claude-subscription-directsdk-experimental",
      "windows": [
        {"label": "Current session", "used_percent": 77.0, "resets_at": "2026-11-08T20:50:00+00:00"},
        {"label": "Current week", "used_percent": 48.0, "resets_at": "2026-11-14T00:00:00+00:00"}
      ],
      "details": [], "unavailable_reason": null
    }"#;

    #[test]
    fn the_tightest_window_decides_headroom() {
        let q = parse_usage(USAGE, 10).unwrap();
        assert_eq!(q.headroom_pct, Some(23.0));
        assert_eq!(q.resets_at.as_deref(), Some("2026-11-08T20:50:00+00:00"));
        assert!(
            q.summary.starts_with("23% left in current session"),
            "{}",
            q.summary
        );
        assert!(has_headroom(&q));
        let tight = USAGE.replace("77.0", "95.0");
        assert!(!has_headroom(&parse_usage(&tight, 10).unwrap()));
    }

    #[test]
    fn credits_are_read_from_the_details_of_a_metered_provider() {
        let j = r#"{"provider":"openrouter","windows":[],"details":["Credits balance: $2.80","API key usage: $2.19 total"],"unavailable_reason":null}"#;
        let q = parse_usage(j, 0).unwrap();
        assert_eq!(q.remaining_usd, Some(2.80));
        assert_eq!(q.headroom_pct, None);
        assert_eq!(q.summary, "$2.80 credits left");
        assert!(has_headroom(&q));
        let empty = j.replace("2.80", "0.00");
        assert!(!has_headroom(&parse_usage(&empty, 0).unwrap()));
        assert!(parse_usage("not json", 0).is_none());
    }

    #[test]
    fn cost_models_follow_the_provider() {
        assert_eq!(cost_model("claude-code", ""), "flat");
        assert_eq!(
            cost_model("hermes", "claude-subscription-directsdk-experimental"),
            "flat"
        );
        assert_eq!(cost_model("hermes", "openrouter"), "metered");
        assert_eq!(cost_model("hermes", "custom"), "local");
    }

    #[test]
    fn a_card_says_why_an_agent_cannot_take_work() {
        let ready = Inputs {
            installed: true,
            provider: "x".into(),
            hub_identity: true,
            ..Default::default()
        };
        assert_eq!(build("claude-code", &ready).health, "ready");
        assert_eq!(build("qoder", &ready).health, "ide only");
        assert_eq!(build("qoder", &ready).start, "IDE only");
        let missing = Inputs {
            installed: false,
            ..ready.clone()
        };
        assert_eq!(build("antigravity", &missing).health, "not installed");
        let anon = Inputs {
            hub_identity: false,
            ..ready.clone()
        };
        assert_eq!(build("hermes", &anon).health, "read-only on the hub");
        let low = Inputs {
            quota: parse_usage(&USAGE.replace("77.0", "96.0"), 0).unwrap(),
            ..ready.clone()
        };
        assert_eq!(build("claude-code", &low).health, "low quota");
        let own = Inputs {
            strengths: Some(vec!["rust".into()]),
            measured: Measured {
                strong_roles: vec!["implementer".into()],
                ..Default::default()
            },
            ..ready
        };
        let c = build("hermes", &own);
        assert_eq!(c.strengths_source, "settings");
        assert_eq!(c.strengths, vec!["rust", "implementer (measured)"]);
    }

    #[test]
    fn measured_outcomes_come_from_decided_missions_only() {
        let mk = |agent: &str, verdict: Option<&str>, status: &str| crate::missions::Mission {
            id: "M".into(),
            repo: "r".into(),
            goal: "g".into(),
            acceptance: "a".into(),
            role: "implementer".into(),
            agent: agent.into(),
            status: status.into(),
            provider: String::new(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: None,
            reviewer: None,
            review_verdict: verdict.map(String::from),
            pr_url: None,
            review_note: None,
            created_at: 0,
            updated_at: 0,
            verify: None,
        };
        let all = vec![
            mk("claude-code", Some("approve"), "done"),
            mk("claude-code", Some("approve"), "done"),
            mk("claude-code", Some("reject"), "queued"),
            mk("hermes", None, "running"),
        ];
        let m = measure(&all);
        assert_eq!(m["claude-code"].attempts, 3);
        assert_eq!(m["claude-code"].approvals, 2);
        assert_eq!(m["claude-code"].strong_roles, vec!["implementer"]);
        assert!(
            !m.contains_key("hermes"),
            "a running mission is not evidence"
        );
    }
}
