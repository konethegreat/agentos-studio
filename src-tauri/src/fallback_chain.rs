//! Self-built fallback chain (S3): builds Hermes `fallback_providers` from healthy lanes
//! (flat-rate first) via the Hermes CLI and demotes lanes that emit malformed tool calls
//! or empty replies in long tool loops.

use serde::Serialize;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use crate::agent_log::{self, NewEvent};
use crate::canary::{self, LaneState};
use crate::missions::{self, Mission};
use crate::quota_grid::{self, Lane};

const SYNC_INTERVAL_SECS: i64 = 30 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FallbackEntry {
    pub provider: String,
    pub model: String,
    pub flat_rate: bool,
    pub demoted: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FallbackChainReport {
    pub last_synced_at: Option<i64>,
    pub entries: Vec<FallbackEntry>,
    pub changed: bool,
    pub summary: String,
    pub last_error: Option<String>,
}

static REPORT: OnceLock<Mutex<FallbackChainReport>> = OnceLock::new();

fn report_lock() -> &'static Mutex<FallbackChainReport> {
    REPORT.get_or_init(|| Mutex::new(FallbackChainReport::default()))
}

pub fn status() -> FallbackChainReport {
    report_lock().lock().unwrap().clone()
}

pub fn step() {
    if crate::pause::paused() {
        return;
    }
    let now = chrono::Utc::now().timestamp();
    if let Some(last) = report_lock().lock().unwrap().last_synced_at {
        if now - last < SYNC_INTERVAL_SECS {
            return;
        }
    }
    let _ = sync_now();
}

pub fn sync_now() -> FallbackChainReport {
    let lanes = quota_grid::lanes(false);
    let canaries = canary::status();
    let missions_all = missions::list(200).unwrap_or_default();
    let current = read_current_cli_chain().unwrap_or_default();
    let rep = sync_with(&lanes, &canaries, &missions_all, &current, apply_cli_chain);
    *report_lock().lock().unwrap() = rep.clone();
    rep
}

fn is_loop_glitch(text: &str) -> bool {
    let low = text.to_ascii_lowercase();
    low.contains("malformed tool call")
        || low.contains("improperly formatted function call")
        || low.contains("invalid tool call")
        || low.contains("no reply")
        || low.contains("empty response")
        || low.contains("empty reply")
}

pub fn collect_demoted(canaries: &[LaneState], missions: &[Mission]) -> HashSet<(String, String)> {
    let mut demoted = HashSet::new();
    for c in canaries {
        if matches!(c.outcome.as_str(), "malformed_tool_call" | "no_reply") {
            demoted.insert((c.provider.clone(), c.model.clone()));
        }
    }
    for m in missions {
        let note = m.review_note.as_deref().unwrap_or("");
        if !is_loop_glitch(note) {
            continue;
        }
        if !m.provider.is_empty() && !m.model.is_empty() {
            demoted.insert((m.provider.clone(), m.model.clone()));
        }
    }
    demoted
}

pub fn build_chain(
    lanes: &[Lane],
    canaries: &[LaneState],
    missions: &[Mission],
) -> Vec<FallbackEntry> {
    let demoted = collect_demoted(canaries, missions);
    let mut candidates: Vec<&Lane> = lanes
        .iter()
        .filter(|l| l.open)
        .filter(|l| {
            canaries
                .iter()
                .find(|c| c.provider == l.provider && c.model == l.model)
                .map(|c| !c.closed)
                .unwrap_or(true)
        })
        .collect();

    candidates.sort_by(|a, b| {
        let a_dem = demoted.contains(&(a.provider.clone(), a.model.clone()));
        let b_dem = demoted.contains(&(b.provider.clone(), b.model.clone()));
        let a_flat = canary::probe_allowed(&a.provider, false);
        let b_flat = canary::probe_allowed(&b.provider, false);
        a_dem
            .cmp(&b_dem)
            .then_with(|| b_flat.cmp(&a_flat))
            .then_with(|| {
                let ah = a.headroom_pct.unwrap_or(0.0);
                let bh = b.headroom_pct.unwrap_or(0.0);
                bh.partial_cmp(&ah).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.provider.cmp(&b.provider))
            .then_with(|| a.model.cmp(&b.model))
    });

    candidates
        .into_iter()
        .map(|l| {
            let is_dem = demoted.contains(&(l.provider.clone(), l.model.clone()));
            let is_flat = canary::probe_allowed(&l.provider, false);
            let reason = if is_dem {
                "demoted (tool-loop glitch)".to_string()
            } else if is_flat {
                "healthy flat-rate".to_string()
            } else {
                "healthy metered".to_string()
            };
            FallbackEntry {
                provider: l.provider.clone(),
                model: l.model.clone(),
                flat_rate: is_flat,
                demoted: is_dem,
                reason,
            }
        })
        .collect()
}

pub fn sync_with<F>(
    lanes: &[Lane],
    canaries: &[LaneState],
    missions: &[Mission],
    current_cli: &[(String, String)],
    apply_fn: F,
) -> FallbackChainReport
where
    F: FnOnce(&[FallbackEntry]) -> Result<(), String>,
{
    let now = chrono::Utc::now().timestamp();
    let entries = build_chain(lanes, canaries, missions);
    let desired: Vec<(String, String)> = entries
        .iter()
        .map(|e| (e.provider.clone(), e.model.clone()))
        .collect();
    let changed = desired != current_cli;
    let mut last_error = None;
    let summary = if entries.is_empty() {
        "No healthy open lanes for fallback chain.".to_string()
    } else if changed {
        if let Err(e) = apply_fn(&entries) {
            last_error = Some(e.clone());
            format!("Fallback update failed: {e}")
        } else {
            let desc: Vec<String> = entries
                .iter()
                .map(|e| format!("{} ({})", e.model, e.provider))
                .collect();
            let msg = format!("Updated fallback chain: {}", desc.join(" -> "));
            let _ = agent_log::record(&NewEvent {
                kind: "note",
                from_agent: "fallback_chain",
                to_agent: "fleet",
                summary: "Self-built fallback chain",
                detail: &msg,
                ..Default::default()
            });
            msg
        }
    } else {
        format!("Fallback chain unchanged ({} entries).", entries.len())
    };

    FallbackChainReport {
        last_synced_at: Some(now),
        entries,
        changed,
        summary,
        last_error,
    }
}

pub fn parse_fallback_list(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        let Some(rest) = t.split_once('.').map(|(_, r)| r.trim()) else {
            continue;
        };
        if !t
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            continue;
        }
        if let (Some(open), Some(close)) = (rest.rfind("(via "), rest.rfind(')')) {
            if close > open + 5 {
                let model = rest[..open].trim().to_string();
                let provider = rest[open + 5..close].trim().to_string();
                if !model.is_empty() && !provider.is_empty() {
                    out.push((provider, model));
                }
            }
        }
    }
    out
}

fn read_current_cli_chain() -> Result<Vec<(String, String)>, String> {
    let out = std::process::Command::new("hermes")
        .args(["fallback", "list"])
        .output()
        .map_err(|e| format!("hermes fallback list: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(parse_fallback_list(&String::from_utf8_lossy(&out.stdout)))
}

fn apply_cli_chain(entries: &[FallbackEntry]) -> Result<(), String> {
    if entries.is_empty() {
        let out = std::process::Command::new("hermes")
            .args(["fallback", "clear"])
            .output()
            .map_err(|e| format!("hermes fallback clear: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        return Ok(());
    }
    let payload: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "provider": e.provider,
                "model": e.model,
            })
        })
        .collect();
    let json_arg = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let out = std::process::Command::new("hermes")
        .args(["config", "set", "fallback_providers", &json_arg])
        .output()
        .map_err(|e| format!("hermes config set fallback_providers: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(provider: &str, model: &str, headroom: f64, open: bool) -> Lane {
        Lane {
            provider: provider.into(),
            model: model.into(),
            primary: false,
            headroom_pct: Some(headroom),
            resets_at: None,
            exhausted_until: None,
            open,
            why: "ready".into(),
        }
    }

    #[test]
    fn orders_flat_rate_first_and_demotes_tool_loop_glitch_lanes() {
        let lanes = vec![
            lane("openrouter", "anthropic/claude-sonnet-4", 100.0, true),
            lane(
                "antigravity-subscription-directsdk",
                "gemini-4-hydrogen-high",
                100.0,
                true,
            ),
            lane(
                "claude-subscription-directsdk-experimental",
                "claude-sonnet-5-5",
                90.0,
                true,
            ),
            lane(
                "antigravity-subscription-directsdk",
                "gemini-4-broken",
                100.0,
                false,
            ),
        ];
        let canaries = vec![LaneState {
            provider: "antigravity-subscription-directsdk".into(),
            model: "gemini-4-hydrogen-high".into(),
            outcome: "malformed_tool_call".into(),
            detail: "malformed tool call".into(),
            at: 1,
            secs: 1.0,
            fails: 1,
            closed: false,
            skipped: false,
        }];
        let chain = build_chain(&lanes, &canaries, &[]);
        assert_eq!(chain.len(), 3);
        // Healthy flat-rate non-demoted comes first
        assert_eq!(
            chain[0].provider,
            "claude-subscription-directsdk-experimental"
        );
        assert!(!chain[0].demoted);
        // Healthy metered non-demoted comes before demoted flat-rate
        assert_eq!(chain[1].provider, "openrouter");
        assert!(!chain[1].demoted);
        // Demoted lane is last
        assert_eq!(chain[2].model, "gemini-4-hydrogen-high");
        assert!(chain[2].demoted);
    }

    #[test]
    fn parses_hermes_fallback_list_output() {
        let raw = "  Primary:   gemini-4-hydrogen-medium  (via antigravity-subscription-directsdk)
  Fallback chain (2 entries):
    1. gemini-4-hydrogen-high  (via antigravity-subscription-directsdk)
    2. claude-sonnet-5-5  (via claude-subscription-directsdk-experimental)";
        let parsed = parse_fallback_list(raw);
        assert_eq!(
            parsed,
            vec![
                (
                    "antigravity-subscription-directsdk".into(),
                    "gemini-4-hydrogen-high".into()
                ),
                (
                    "claude-subscription-directsdk-experimental".into(),
                    "claude-sonnet-5-5".into()
                ),
            ]
        );
    }

    #[test]
    fn sync_with_only_applies_when_chain_differs() {
        let db_dir = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(db_dir.path().join("studio.db")));
        let lanes = vec![lane(
            "claude-subscription-directsdk-experimental",
            "claude-sonnet-5-5",
            90.0,
            true,
        )];
        let mut called = 0;
        let rep1 = sync_with(
            &lanes,
            &[],
            &[],
            &[(
                "claude-subscription-directsdk-experimental".into(),
                "claude-sonnet-5-5".into(),
            )],
            |_| {
                called += 1;
                Ok(())
            },
        );
        assert!(!rep1.changed);
        assert_eq!(called, 0);

        let rep2 = sync_with(&lanes, &[], &[], &[], |_| {
            called += 1;
            Ok(())
        });
        assert!(rep2.changed);
        assert_eq!(called, 1);
        crate::store::set_test_db(None);
    }
}
