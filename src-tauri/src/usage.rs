//! Context and money, read from Hermes's own session store.
//!
//! Hermes records token counts, cost and compaction in `state.db`, so Studio can show what a session
//! has spent and how close it is to the end of its context window without guessing. That matters
//! because compaction is where quality drops: seeing 80 percent coming is the difference between
//! finishing a task and losing the thread.
//!
//! Credits are the one thing a provider has to tell us. OpenRouter exposes a credits endpoint; a
//! Claude or Antigravity subscription does not, so those report "flat rate" rather than inventing a
//! number.

use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Default)]
pub struct SessionUsage {
    pub session_id: String,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
    pub cost_usd: f64,
    pub messages: i64,
    pub compacted_messages: i64,
    pub context_limit: Option<i64>,
    /// Share of the window the last turn consumed, when both numbers are known.
    pub context_used_pct: Option<f64>,
    /// True when the window figure is derived from text length because Hermes stored no per-message tokens.
    pub context_estimated: bool,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Credits {
    pub provider: String,
    pub kind: String,
    pub remaining_usd: Option<f64>,
    pub used_usd: Option<f64>,
    pub total_usd: Option<f64>,
    pub note: String,
}

fn state_db() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(crate::settings::home)
        .join("hermes")
        .join("state.db")
}

fn models_cache() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(crate::settings::home)
        .join("hermes")
        .join("models_dev_cache.json")
}

/// Find `limit.context` for a model id in the models.dev cache, whatever its nesting.
pub fn context_limit(v: &Value, model: &str) -> Option<i64> {
    fn walk(v: &Value, model: &str, depth: usize) -> Option<i64> {
        if depth > 6 {
            return None;
        }
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    if k == model || k.ends_with(&format!("/{model}")) {
                        if let Some(n) = child
                            .get("limit")
                            .and_then(|l| l.get("context"))
                            .and_then(|c| c.as_i64())
                        {
                            return Some(n);
                        }
                    }
                    if let Some(n) = walk(child, model, depth + 1) {
                        return Some(n);
                    }
                }
                None
            }
            Value::Array(items) => items.iter().find_map(|i| walk(i, model, depth + 1)),
            _ => None,
        }
    }
    walk(v, model, 0)
}

/// The newest message time (epoch seconds) across Hermes sessions, read only. A worker that is
/// still talking to its model keeps this fresh, so it is the Hermes heartbeat Studio can see.
pub fn last_message_at(session_ids: &[String]) -> Option<i64> {
    if session_ids.is_empty() {
        return None;
    }
    let path = state_db();
    let c = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    session_ids
        .iter()
        .filter_map(|sid| {
            c.query_row(
                "SELECT MAX(timestamp) FROM messages WHERE session_id = ?1",
                [sid],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten()
        })
        .map(|t| t as i64)
        .max()
}

pub fn usage(session_id: &str) -> Result<Option<SessionUsage>> {
    let path = state_db();
    if !path.exists() {
        return Ok(None);
    }
    let c = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("open state.db")?;
    let row = c
        .query_row(
            "SELECT COALESCE(input_tokens,0), COALESCE(output_tokens,0), COALESCE(cache_read_tokens,0),
                    COALESCE(cache_write_tokens,0), COALESCE(reasoning_tokens,0),
                    COALESCE(actual_cost_usd, estimated_cost_usd, 0), COALESCE(message_count,0),
                    model, billing_provider
             FROM sessions WHERE id = ?1",
            [session_id],
            |r| {
                Ok(SessionUsage {
                    session_id: session_id.to_string(),
                    input_tokens: r.get(0)?,
                    output_tokens: r.get(1)?,
                    cache_read_tokens: r.get(2)?,
                    cache_write_tokens: r.get(3)?,
                    reasoning_tokens: r.get(4)?,
                    cost_usd: r.get(5)?,
                    messages: r.get(6)?,
                    model: r.get::<_, Option<String>>(7)?,
                    provider: r.get::<_, Option<String>>(8)?,
                    ..Default::default()
                })
            },
        )
        .optional()?;
    let Some(mut u) = row else { return Ok(None) };

    u.compacted_messages = c
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND compacted = 1",
            [session_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if let Some(model) = u.model.clone() {
        if let Ok(text) = std::fs::read_to_string(models_cache()) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                // Model ids can carry a variant suffix such as `[1m]`; the cache keys do not.
                let bare = model.split('[').next().unwrap_or(&model).trim().to_string();
                u.context_limit = context_limit(&v, &model).or_else(|| context_limit(&v, &bare));
            }
        }
        // What the window holds now. Hermes sometimes stores no per-message token count, so fall
        // back to a character estimate and say so, rather than showing a confident zero.
        let (tokens, chars): (i64, i64) = c
            .query_row(
                "SELECT COALESCE(SUM(token_count),0), COALESCE(SUM(LENGTH(content)),0)
                 FROM messages
                 WHERE session_id = ?1 AND COALESCE(compacted,0) = 0 AND COALESCE(active,1) = 1",
                [session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0));
        let used = if tokens > 0 {
            tokens
        } else {
            u.context_estimated = chars > 0;
            chars / 4
        };
        if let Some(limit) = u.context_limit {
            if limit > 0 {
                u.context_used_pct = Some((used as f64 / limit as f64 * 100.0).clamp(0.0, 100.0));
            }
        }
    }
    Ok(Some(u))
}

/// What is left at the provider. Only OpenRouter can answer this truthfully.
/// Session ids that actually recorded token usage, newest first. Used by the live check.
pub fn recent_sessions_with_tokens(limit: usize) -> Vec<String> {
    let Ok(c) = Connection::open_with_flags(
        state_db(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    let Ok(mut st) = c.prepare(
        "SELECT id FROM sessions WHERE COALESCE(input_tokens,0) > 0 ORDER BY last_activity_at DESC LIMIT ?1",
    ) else {
        return Vec::new();
    };
    st.query_map([limit as i64], |r| r.get::<_, String>(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

pub fn credits() -> Vec<Credits> {
    let mut out = Vec::new();
    let key = std::env::var("OPENROUTER_API_KEY").ok().or_else(|| {
        let path = dirs::data_local_dir()?.join("hermes").join(".env");
        let text = std::fs::read_to_string(path).ok()?;
        crate::notion::parse_env(&text, "OPENROUTER_API_KEY")
    });
    match key {
        Some(k) => {
            let req = ureq::http::Request::builder()
                .uri("https://openrouter.ai/api/v1/credits")
                .header("Authorization", format!("Bearer {k}"))
                .body(String::new());
            let parsed = req.ok().and_then(|r| ureq::run(r).ok()).and_then(|res| {
                let text = res.into_body().read_to_string().ok()?;
                serde_json::from_str::<Value>(&text).ok()
            });
            let data = parsed.and_then(|v| v.get("data").cloned());
            let total = data
                .as_ref()
                .and_then(|d| d.get("total_credits"))
                .and_then(|n| n.as_f64());
            let used = data
                .as_ref()
                .and_then(|d| d.get("total_usage"))
                .and_then(|n| n.as_f64());
            out.push(Credits {
                provider: "openrouter".into(),
                kind: "metered".into(),
                remaining_usd: match (total, used) {
                    (Some(t), Some(u)) => Some((t - u).max(0.0)),
                    _ => None,
                },
                used_usd: used,
                total_usd: total,
                note: if total.is_some() {
                    "Live from the OpenRouter credits endpoint".into()
                } else {
                    "Key present, but the credits endpoint could not be read".into()
                },
            });
        }
        None => out.push(Credits {
            provider: "openrouter".into(),
            kind: "metered".into(),
            note: "No OpenRouter key, so nothing is metered".into(),
            ..Default::default()
        }),
    }
    for p in ["claude-subscription", "antigravity-oauth"] {
        out.push(Credits {
            provider: p.into(),
            kind: "flat".into(),
            note: "Subscription: no per-token balance to read, so it cannot run out mid-task"
                .into(),
            ..Default::default()
        });
    }
    out
}

/// Subscriptions are flat rate: a session on them costs nothing at the margin, whatever Hermes
/// estimates. Only metered providers count against a mission budget. Pure.
pub fn is_flat(provider: &str) -> bool {
    let p = provider.trim().to_lowercase();
    p.starts_with("claude-subscription") || p == "antigravity-oauth"
}

/// Metered spend of Hermes sessions, from `state.db`: each session's `estimated_cost_usd` (or
/// `actual_cost_usd` when Hermes has one), skipping flat-rate providers. Unknown ids count zero.
pub fn metered_cost_of(c: &rusqlite::Connection, session_ids: &[String]) -> f64 {
    let mut total = 0.0;
    for id in session_ids.iter().filter(|s| !s.trim().is_empty()) {
        let row: Option<(Option<String>, Option<f64>, Option<f64>)> = c
            .query_row(
                "SELECT billing_provider, estimated_cost_usd, actual_cost_usd FROM sessions WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        if let Some((provider, est, actual)) = row {
            if provider.as_deref().is_some_and(is_flat) {
                continue;
            }
            total += actual.filter(|a| *a > 0.0).or(est).unwrap_or(0.0).max(0.0);
        }
    }
    total
}

pub fn metered_cost(session_ids: &[String]) -> f64 {
    crate::hermes_engine::open_ro()
        .map(|c| metered_cost_of(&c, session_ids))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_metered_sessions_count_against_a_budget() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE sessions (id TEXT, billing_provider TEXT, estimated_cost_usd REAL, actual_cost_usd REAL);
             INSERT INTO sessions VALUES ('a', 'claude-subscription-directsdk-experimental', 4.0, NULL);
             INSERT INTO sessions VALUES ('b', 'openrouter', 0.25, NULL);
             INSERT INTO sessions VALUES ('c', 'openrouter', 0.10, 0.30);
             INSERT INTO sessions VALUES ('d', 'antigravity-oauth', 9.0, NULL);",
        )
        .unwrap();
        let ids: Vec<String> = ["a", "b", "c", "d", "missing"].map(String::from).to_vec();
        assert!((metered_cost_of(&c, &ids) - 0.55).abs() < 1e-9);
    }
    use serde_json::json;

    #[test]
    fn context_limit_is_found_at_any_depth() {
        let v = json!({"providers": {"anthropic": {"models": {"claude-x": {"limit": {"context": 200000}}}}}});
        assert_eq!(context_limit(&v, "claude-x"), Some(200_000));
        assert_eq!(context_limit(&v, "missing"), None);
    }

    #[test]
    fn credits_never_invent_a_balance_for_flat_rates() {
        let c = credits();
        let flat: Vec<_> = c.iter().filter(|x| x.kind == "flat").collect();
        assert_eq!(flat.len(), 2);
        assert!(flat.iter().all(|x| x.remaining_usd.is_none()));
        assert!(flat.iter().all(|x| !x.note.is_empty()));
    }
}
