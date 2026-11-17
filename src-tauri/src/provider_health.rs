//! Connection health for the subscriptions and keys Hermes runs on (docs/CONNECTIONS.md).
//!
//! Signed-in state comes from Hermes itself (`hermes auth status <provider>`), plugin state from
//! `hermes plugins show`, and what actually failed from the `API call failed (attempt n/m)` lines
//! Hermes writes to `logs/errors.log` (they carry `provider=` and the upstream summary). Studio
//! never reads a token. Every fix is a Hermes or Claude CLI command the owner starts with a click.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::procs;

/// Failures older than this no longer count against a provider.
const RECENT_SECS: i64 = 6 * 3600;
/// A rate limit with no readable reset time is assumed to last this long.
const LIMIT_DEFAULT_SECS: i64 = 3600;
/// Read at most this much of the end of `errors.log`.
const TAIL_BYTES: u64 = 512 * 1024;
pub const HEALTH_KEY: &str = "provider.health";
pub const AUTOFIX_LOG_KEY: &str = "provider.autofix.log";
const AUTOFIX_COOLDOWN_SECS: i64 = 900;
const DEGRADED_TTL_SECS: i64 = 1800;
/// Failure kinds that a fresh attempt usually clears on its own.
const TRANSIENT_KINDS: [&str; 3] = ["timeout", "broken_pipe", "server_error"];

/// How many credentials `hermes auth reset` touched ("Reset status on 2 <provider> credentials").
/// Pure; 0 when the line is missing, so an empty pool never counts as a fix.
fn reset_count(out: &str) -> usize {
    out.split("Reset status on ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

pub struct Known {
    pub id: &'static str,
    pub label: &'static str,
    /// Hermes plugin that provides it, if any.
    pub plugin: Option<&'static str>,
}

pub const KNOWN: [Known; 3] = [
    Known {
        id: "claude-subscription-directsdk-experimental",
        label: "Claude subscription",
        plugin: Some("claude-subscription-directsdk-experimental"),
    },
    Known {
        id: "antigravity-subscription-directsdk",
        label: "Antigravity",
        plugin: Some("antigravity-subscription-directsdk"),
    },
    Known {
        id: "openrouter",
        label: "OpenRouter",
        plugin: None,
    },
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub at: i64,
    pub provider: String,
    /// rate_limit, auth, timeout, other
    pub kind: String,
    pub summary: String,
    /// The reset time the provider gave, as written ("11:50pm (Africa/Johannesburg)").
    pub resets: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub id: String,
    pub label: String,
    /// The provider Hermes is set to use, or one in its fallback chain.
    pub in_use: bool,
    pub signed_in: Option<bool>,
    pub plugin_enabled: Option<bool>,
    /// ok, signed_out, plugin_disabled, rate_limited, failing, not_set_up
    pub state: String,
    pub detail: String,
    pub last_failure: Option<Failure>,
    /// A fix the Connections card offers: (action id, button label).
    pub fix: Option<(String, String)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub providers: Vec<Health>,
    pub active: Option<String>,
    pub fallbacks: Vec<String>,
    /// A line for the owner when work would stop on one failure (no fallback configured).
    pub advice: Option<String>,
    pub checked_at: i64,
}

impl Report {
    /// Providers in use that cannot run right now.
    pub fn problems(&self) -> impl Iterator<Item = &Health> {
        self.providers
            .iter()
            .filter(|h| h.in_use && !matches!(h.state.as_str(), "ok" | "not_set_up"))
    }
}

fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let i = line.find(&format!(" {name}="))? + name.len() + 2;
    let rest = &line[i..];
    Some(rest.split(' ').next().unwrap_or(rest))
}

/// Classify an upstream summary. Pure.
pub fn classify(summary: &str) -> (&'static str, Option<String>) {
    let s = summary.to_lowercase();
    let resets = summary
        .find("resets ")
        .map(|i| {
            summary[i + 7..]
                .split(" (request_id")
                .next()
                .unwrap_or("")
                .trim()
        })
        .map(|r| r.trim_end_matches(')').trim().to_string())
        .map(|r| {
            if r.contains('(') && !r.ends_with(')') {
                format!("{r})")
            } else {
                r
            }
        })
        .filter(|r| !r.is_empty());
    let kind = if s.contains("429")
        || s.contains("rate limit")
        || s.contains("hit your") && s.contains("limit")
    {
        "rate_limit"
    } else if s.contains("401")
        || s.contains("403")
        || s.contains("unauthorized")
        || s.contains("not logged in")
        || s.contains("logged out")
        || s.contains("expired")
        || s.contains("invalid_grant")
        || s.contains("authentication")
    {
        "auth"
    } else if s.contains("timed out") || s.contains("timeout") {
        "timeout"
    } else if s.contains("broken pipe")
        || s.contains("brokenpipeerror")
        || s.contains("eoferror")
        || s.contains("connection reset")
        || s.contains("connection refused")
        || s.contains("connection aborted")
        || s.contains("stream drop")
        || s.contains("stream exhausted")
        || s.contains("managed worker lost")
        || s.contains("process exited")
    {
        "broken_pipe"
    } else if s.contains("http 500")
        || s.contains("http 502")
        || s.contains("http 503")
        || s.contains("http 504")
        || s.contains("internal server error")
        || s.contains("bad gateway")
        || s.contains("service unavailable")
        || s.contains("overloaded")
    {
        "server_error"
    } else {
        "other"
    };
    (kind, resets)
}

fn parse_ts(line: &str) -> Option<i64> {
    let ts = line.get(..19)?;
    let naive = chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S").ok()?;
    naive
        .and_local_timezone(chrono::Local)
        .single()
        .map(|t| t.timestamp())
}

/// Every `API call failed (attempt` line, oldest first. Pure.
pub fn failures(log: &str) -> Vec<Failure> {
    log.lines()
        .filter(|l| l.contains("API call failed (attempt"))
        .filter_map(|l| {
            let at = parse_ts(l)?;
            let provider = field(l, "provider")?.to_string();
            let summary = l
                .find(" summary=")
                .map(|i| l[i + 9..].trim().to_string())
                .or_else(|| field(l, "error_type").map(String::from))
                .unwrap_or_default();
            let (kind, resets) = classify(&summary);
            Some(Failure {
                at,
                provider,
                kind: kind.into(),
                summary: summary.chars().take(240).collect(),
                resets,
            })
        })
        .collect()
}

/// `hermes auth status <id>` prints `<id>: logged in` or `<id>: logged out`. Pure.
pub fn parse_auth(text: &str) -> Option<bool> {
    let l = text.to_lowercase();
    if l.contains(": logged out") || l.contains("not logged in") {
        Some(false)
    } else if l.contains(": logged in") {
        Some(true)
    } else {
        None
    }
}

/// `hermes fallback list` provider ids; empty when none are configured. Pure.
pub fn parse_fallbacks(text: &str, known: &[&str]) -> Vec<String> {
    if text.contains("No fallback providers configured") {
        return Vec::new();
    }
    known
        .iter()
        .filter(|k| text.contains(*k))
        .map(|k| k.to_string())
        .collect()
}

/// Decide one provider's state. Pure.
#[allow(clippy::too_many_arguments)]
pub fn judge(
    k: &Known,
    in_use: bool,
    signed_in: Option<bool>,
    plugin_enabled: Option<bool>,
    last: Option<Failure>,
    now: i64,
) -> Health {
    let recent = last.filter(|f| now - f.at <= RECENT_SECS);
    let (state, detail, fix): (&str, String, Option<(&str, &str)>) = if k.plugin.is_some()
        && plugin_enabled == Some(false)
    {
        (
            "plugin_disabled",
            format!(
                "Hermes has the {} plugin but it is disabled, so this provider cannot run",
                k.plugin.unwrap_or("")
            ),
            Some(("enable_plugin", "Enable plugin")),
        )
    } else if signed_in == Some(false) {
        let fix = match k.id {
            "openrouter" => None,
            "antigravity-subscription-directsdk" | "antigravity-oauth" => {
                Some(("sign_in_antigravity", "Sign in to Antigravity"))
            }
            _ => Some(("sign_in_claude", "Sign in to Claude")),
        };
        if in_use || k.id != "openrouter" {
            (
                "signed_out",
                "Signed out: Hermes cannot use this provider until you sign in again".into(),
                fix,
            )
        } else {
            ("not_set_up", "No key yet".into(), None)
        }
    } else if let Some(f) = recent.as_ref().filter(|f| f.kind == "rate_limit") {
        // A limit that names its reset holds until then; otherwise for an hour.
        let open = f.resets.is_some() || now - f.at <= LIMIT_DEFAULT_SECS;
        if open && now - f.at <= RECENT_SECS {
            (
                "rate_limited",
                match &f.resets {
                    Some(r) => format!("Usage limit reached; it resets {r}. Hermes stops until then unless a fallback is set"),
                    None => "Usage limit reached recently. Hermes stops on it unless a fallback is set".into(),
                },
                Some(("add_fallback", "Add a fallback provider")),
            )
        } else {
            ("ok", "Signed in".into(), None)
        }
    } else if let Some(f) = recent.as_ref().filter(|f| f.kind == "auth") {
        (
            "failing",
            format!(
                "Recent sign-in errors: {}",
                f.summary.chars().take(120).collect::<String>()
            ),
            Some(("refresh", "Refresh sign-in")),
        )
    } else if let Some(f) = recent.as_ref().filter(|f| {
        now - f.at <= 1800 && matches!(f.kind.as_str(), "timeout" | "broken_pipe" | "server_error")
    }) {
        let prefix = match f.kind.as_str() {
            "broken_pipe" => "Stream or worker pipe broke in the last half hour",
            "server_error" => "Upstream server error in the last half hour",
            _ => "Calls timed out in the last half hour",
        };
        (
            "failing",
            format!(
                "{prefix}: {}",
                f.summary.chars().take(120).collect::<String>()
            ),
            Some(("auto_heal", "Troubleshoot & fix")),
        )
    } else if signed_in == Some(true) {
        ("ok", "Signed in".into(), None)
    } else {
        ("not_set_up", "Not set up".into(), None)
    };
    Health {
        id: k.id.into(),
        label: k.label.into(),
        in_use,
        signed_in,
        plugin_enabled,
        state: state.into(),
        detail,
        last_failure: recent,
        fix: fix.map(|(a, b)| (a.into(), b.into())),
    }
}

/// The advice line: work stops on one failure when nothing else is in the chain. Pure.
pub fn advice(active: Option<&str>, fallbacks: &[String], providers: &[Health]) -> Option<String> {
    let active = active?;
    if !fallbacks.is_empty() {
        return None;
    }
    let others: Vec<&str> = providers
        .iter()
        .filter(|h| h.id != active && h.state == "ok")
        .map(|h| h.label.as_str())
        .collect();
    if others.is_empty() {
        return Some("Hermes has no fallback provider, so a usage limit or sign-out stops all work. Connect a second provider, then add it as a fallback.".into());
    }
    Some(format!(
        "Hermes has no fallback provider, so a usage limit stops all work. {} is signed in and can take over: add it as a fallback.",
        others.join(" or ")
    ))
}

fn tail(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn run_text(args: &[&str]) -> String {
    procs::run_with_timeout(
        "hermes",
        args,
        None,
        None,
        std::time::Duration::from_secs(30),
    )
    .map(|(_, o, e)| format!("{o}\n{e}"))
    .unwrap_or_default()
}

/// Check every known provider now (a few seconds: three `hermes auth status` calls).
pub fn check(now: i64) -> Report {
    let yaml = std::fs::read_to_string(procs::hermes_config()).unwrap_or_default();
    let active = crate::models::parse_active(&yaml, "provider");
    let ids: Vec<&str> = KNOWN.iter().map(|k| k.id).collect();
    let fallbacks = parse_fallbacks(&run_text(&["fallback", "list"]), &ids);
    let fails = failures(&tail(&procs::hermes_home().join("logs").join("errors.log")));
    let handles: Vec<_> = KNOWN
        .iter()
        .map(|k| {
            let id = k.id;
            let plugin = k.plugin;
            std::thread::spawn(move || {
                // An API-key provider is ready when its key is in the Hermes env file; `hermes
                // auth status` reports only pooled OAuth sign-ins and says logged out for it.
                let signed = if id == "openrouter" {
                    Some(crate::connect::env_has("OPENROUTER_API_KEY"))
                } else {
                    parse_auth(&run_text(&["auth", "status", id]))
                };
                let enabled = plugin
                    .map(|p| crate::connect::plugin_enabled_in(&run_text(&["plugins", "show", p])));
                (signed, enabled)
            })
        })
        .collect();
    let providers: Vec<Health> = KNOWN
        .iter()
        .zip(handles)
        .map(|(k, h)| {
            let (signed, enabled) = h.join().unwrap_or((None, None));
            let last = fails.iter().rev().find(|f| f.provider == k.id).cloned();
            let in_use = active.as_deref() == Some(k.id) || fallbacks.iter().any(|f| f == k.id);
            judge(k, in_use, signed, enabled, last, now)
        })
        .collect();
    let advice = advice(active.as_deref(), &fallbacks, &providers);
    Report {
        providers,
        active,
        fallbacks,
        advice,
        checked_at: now,
    }
}

/// The command a fix runs, in its own console window so sign-ins and pickers work. Pure.
pub fn fix_command(provider: &str, action: &str) -> Option<(&'static str, Vec<String>)> {
    let known = KNOWN.iter().find(|k| k.id == provider)?;
    let p = provider.to_string();
    Some(match action {
        "sign_in_claude" => ("claude", vec!["auth".into(), "login".into()]),
        "sign_in_antigravity" => (
            "hermes",
            vec![
                "auth".into(),
                "add".into(),
                "antigravity-subscription-directsdk".into(),
            ],
        ),
        "enable_plugin" => (
            "hermes",
            vec!["plugins".into(), "enable".into(), known.plugin?.into()],
        ),
        "refresh" => ("hermes", vec!["auth".into(), "refresh".into(), p]),
        "add_fallback" => ("hermes", vec!["fallback".into(), "add".into()]),
        _ => return None,
    })
}

pub fn run_fix(provider: &str, action: &str) -> anyhow::Result<()> {
    if action == "auto_heal" {
        let _ = auto_heal_now()?;
        return Ok(());
    }
    let (tool, args) = fix_command(provider, action)
        .ok_or_else(|| anyhow::anyhow!("unknown fix '{action}' for '{provider}'"))?;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    procs::spawn_console(tool, &refs)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoFixEntry {
    pub at: i64,
    pub provider: String,
    pub issue: String,
    pub steps: Vec<String>,
    /// fixed, switched_fallback, needs_user, degraded
    pub outcome: String,
    pub detail: String,
}

fn degraded_key(provider: &str) -> String {
    format!("provider.degraded.{provider}")
}

fn cooldown_key(provider: &str) -> String {
    format!("provider.autofix.cooldown.{provider}")
}

pub fn is_degraded_in(c: &rusqlite::Connection, provider: &str, now: i64) -> Option<String> {
    let raw = crate::store::kv_get(c, &degraded_key(provider))
        .ok()
        .flatten()?;
    let (until_s, reason) = raw.split_once('|')?;
    let until: i64 = until_s.trim().parse().ok()?;
    (until > now).then(|| reason.to_string())
}

pub fn set_degraded_in(c: &rusqlite::Connection, provider: &str, until: i64, reason: &str) {
    let clean: String = reason.chars().take(160).collect();
    let _ = crate::store::kv_set(c, &degraded_key(provider), &format!("{until}|{clean}"));
}

pub fn clear_degraded_in(c: &rusqlite::Connection, provider: &str) {
    let _ = crate::store::kv_set(c, &degraded_key(provider), "");
}

pub fn autofix_log_in(c: &rusqlite::Connection) -> Vec<AutoFixEntry> {
    crate::store::kv_get(c, AUTOFIX_LOG_KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn push_autofix_log_in(c: &rusqlite::Connection, entry: AutoFixEntry) {
    let mut list = autofix_log_in(c);
    list.insert(0, entry);
    list.truncate(30);
    let _ = crate::store::kv_set(
        c,
        AUTOFIX_LOG_KEY,
        &serde_json::to_string(&list).unwrap_or_default(),
    );
}

fn run_hermes_ok(args: &[&str]) -> (bool, String) {
    match procs::run_with_timeout(
        "hermes",
        args,
        None,
        None,
        std::time::Duration::from_secs(30),
    ) {
        Ok((code, out, err)) => {
            let combined = format!("{out} {err}").trim().to_string();
            (code == Some(0), combined.chars().take(160).collect())
        }
        Err(e) => (false, format!("{e:#}").chars().take(160).collect()),
    }
}

/// Diagnose and auto-fix unhealthy providers non-interactively.
/// Uses a 15-minute cooldown per provider unless `bypass_cooldown` is true.
pub fn auto_heal_in(
    c: &rusqlite::Connection,
    report: &Report,
    now: i64,
    bypass_cooldown: bool,
) -> Vec<AutoFixEntry> {
    auto_heal_with(c, report, now, bypass_cooldown, run_hermes_ok)
}

fn auto_heal_with(
    c: &rusqlite::Connection,
    report: &Report,
    now: i64,
    bypass_cooldown: bool,
    mut exec: impl FnMut(&[&str]) -> (bool, String),
) -> Vec<AutoFixEntry> {
    let mut out = Vec::new();
    let has_healthy_other = |id: &str| {
        report
            .providers
            .iter()
            .any(|p| p.id != id && p.state == "ok")
    };
    for h in &report.providers {
        if matches!(h.state.as_str(), "ok" | "not_set_up") {
            clear_degraded_in(c, &h.id);
            continue;
        }
        if !bypass_cooldown {
            let cd: i64 = crate::store::kv_get(c, &cooldown_key(&h.id))
                .ok()
                .flatten()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            if cd > now {
                continue;
            }
        }
        let _ = crate::store::kv_set(
            c,
            &cooldown_key(&h.id),
            &(now + AUTOFIX_COOLDOWN_SECS).to_string(),
        );
        let mut steps = Vec::new();
        let (outcome, detail) = match h.state.as_str() {
            "plugin_disabled" => {
                if let Some(plugin) = KNOWN.iter().find(|k| k.id == h.id).and_then(|k| k.plugin) {
                    let (ok_en, msg) = exec(&["plugins", "enable", plugin]);
                    steps.push(format!(
                        "hermes plugins enable {plugin}: {}",
                        if ok_en { "ok" } else { &msg }
                    ));
                    let (ok_show, show_out) = exec(&["plugins", "show", plugin]);
                    if ok_en && ok_show && crate::connect::plugin_enabled_in(&show_out) {
                        clear_degraded_in(c, &h.id);
                        (
                            "fixed".to_string(),
                            format!("Enabled Hermes plugin {plugin}"),
                        )
                    } else {
                        set_degraded_in(c, &h.id, now + DEGRADED_TTL_SECS, &h.detail);
                        (
                            "needs_user".to_string(),
                            format!("Plugin {plugin} could not be enabled automatically: {msg}"),
                        )
                    }
                } else {
                    ("needs_user".to_string(), h.detail.clone())
                }
            }
            "rate_limited" => {
                let closed = crate::quota_grid::learn_from_log(now);
                steps.push(format!(
                    "quota_grid.learn_from_log: closed {} model(s)",
                    closed.len()
                ));
                set_degraded_in(c, &h.id, now + DEGRADED_TTL_SECS, &h.detail);
                if has_healthy_other(&h.id) {
                    ("switched_fallback".to_string(), "Marked rate-limited provider degraded so quota grid steers to healthy fallback".to_string())
                } else {
                    ("degraded".to_string(), h.detail.clone())
                }
            }
            "signed_out" => {
                if h.id != "openrouter" {
                    let (ok_ref, msg) = exec(&["auth", "refresh", &h.id]);
                    steps.push(format!(
                        "hermes auth refresh {}: {}",
                        h.id,
                        if ok_ref { "ok" } else { &msg }
                    ));
                    let (_, st_out) = exec(&["auth", "status", &h.id]);
                    if parse_auth(&st_out) == Some(true) {
                        clear_degraded_in(c, &h.id);
                        (
                            "fixed".to_string(),
                            format!("Refreshed sign-in for {}", h.label),
                        )
                    } else {
                        set_degraded_in(c, &h.id, now + DEGRADED_TTL_SECS, &h.detail);
                        (
                            "needs_user".to_string(),
                            format!("{} is signed out and requires interactive login", h.label),
                        )
                    }
                } else {
                    set_degraded_in(c, &h.id, now + DEGRADED_TTL_SECS, &h.detail);
                    (
                        "needs_user".to_string(),
                        "OpenRouter API key is missing".to_string(),
                    )
                }
            }
            _ => {
                // failing: auth, timeout, broken_pipe, server_error
                let mut refreshed = false;
                if h.id != "openrouter" {
                    let (ok_ref, msg_ref) = exec(&["auth", "refresh", &h.id]);
                    steps.push(format!(
                        "hermes auth refresh {}: {}",
                        h.id,
                        if ok_ref { "ok" } else { &msg_ref }
                    ));
                    let (ok_rst, msg_rst) = exec(&["auth", "reset", &h.id]);
                    steps.push(format!(
                        "hermes auth reset {}: {}",
                        h.id,
                        if ok_rst { "ok" } else { &msg_rst }
                    ));
                    let (_, st_out) = exec(&["auth", "status", &h.id]);
                    let signed = parse_auth(&st_out);
                    steps.push(format!("hermes auth status {}: {:?}", h.id, signed));
                    if signed == Some(false) {
                        set_degraded_in(c, &h.id, now + DEGRADED_TTL_SECS, &h.detail);
                        let entry = AutoFixEntry {
                            at: now,
                            provider: h.id.clone(),
                            issue: h.state.clone(),
                            steps,
                            outcome: "needs_user".into(),
                            detail: format!(
                                "{} is signed out after refresh; sign in required",
                                h.label
                            ),
                        };
                        push_autofix_log_in(c, entry.clone());
                        out.push(entry);
                        continue;
                    }
                    // `auth reset` exits 0 even when the pool is empty (plugin-backed
                    // subscriptions), so only a reset that touched a credential counts.
                    refreshed = ok_ref || (ok_rst && reset_count(&msg_rst) > 0);
                }
                let transient = h
                    .last_failure
                    .as_ref()
                    .is_some_and(|f| TRANSIENT_KINDS.contains(&f.kind.as_str()));
                if has_healthy_other(&h.id) {
                    set_degraded_in(c, &h.id, now + DEGRADED_TTL_SECS, &h.detail);
                    (
                        "switched_fallback".to_string(),
                        format!(
                            "Marked {} degraded for 30 minutes so missions use a healthy fallback",
                            h.label
                        ),
                    )
                } else if refreshed {
                    clear_degraded_in(c, &h.id);
                    (
                        "retrying".to_string(),
                        format!(
                            "Refreshed {} credentials and cleared cooldowns; the next attempt retries",
                            h.label
                        ),
                    )
                } else if transient {
                    // No fallback to steer to: closing the only lane would hold every
                    // mission, so leave it open and let the mission retry absorb it.
                    clear_degraded_in(c, &h.id);
                    (
                        "retrying".to_string(),
                        format!(
                            "{} had a transient failure and no healthy fallback; missions retry automatically",
                            h.label
                        ),
                    )
                } else {
                    set_degraded_in(c, &h.id, now + DEGRADED_TTL_SECS, &h.detail);
                    (
                        "degraded".to_string(),
                        format!(
                            "Auto-troubleshoot could not restore {}; marked degraded",
                            h.label
                        ),
                    )
                }
            }
        };
        let entry = AutoFixEntry {
            at: now,
            provider: h.id.clone(),
            issue: h.state.clone(),
            steps,
            outcome: outcome.clone(),
            detail: detail.clone(),
        };
        push_autofix_log_in(c, entry.clone());
        let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
            kind: "provider_heal",
            from_agent: "provider health",
            summary: &format!("{} ({}): {}", h.label, outcome, detail),
            ..Default::default()
        });
        out.push(entry);
    }
    out
}

pub fn auto_heal_now() -> anyhow::Result<Vec<AutoFixEntry>> {
    let now = chrono::Utc::now().timestamp();
    let report = check(now);
    let c = crate::store::open()?;
    let _ = record_in(&c, &report);
    Ok(auto_heal_in(&c, &report, now, true))
}

pub fn auto_heal_if_due() -> anyhow::Result<Vec<AutoFixEntry>> {
    let now = chrono::Utc::now().timestamp();
    let report = check(now);
    let c = crate::store::open()?;
    let _ = record_in(&c, &report);
    Ok(auto_heal_in(&c, &report, now, false))
}

/// The last report saved by the monitor.
pub fn last_in(c: &rusqlite::Connection) -> Option<Report> {
    crate::store::kv_get(c, HEALTH_KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// Save the report and return the problems that are new since the previous one (a provider
/// whose state changed into a problem), for Windows notifications.
pub fn record_in(c: &rusqlite::Connection, report: &Report) -> Vec<Health> {
    let before = last_in(c);
    let _ = crate::store::kv_set(
        c,
        HEALTH_KEY,
        &serde_json::to_string(report).unwrap_or_default(),
    );
    report
        .problems()
        .filter(|h| {
            before
                .as_ref()
                .and_then(|b| b.providers.iter().find(|p| p.id == h.id))
                .map_or(true, |p| p.state != h.state)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "2026-11-11 23:32:43,325 WARNING [local-x] agent.conversation_loop: API call failed (attempt 2/3) error_type=ClaudeAPIError thread=t provider=claude-subscription-directsdk-experimental base_url=process://x model=m summary=HTTP 429: Incomplete upstream response (first upstream attempt: status 429, upstream said: This request would exceed your account's rate limit. Please try again later.): You've hit your 5-hour limit \u{b7} resets 11:50pm (Africa/Johannesburg) (request_id=req_1)
2026-11-11 23:33:00,000 ERROR agent.conversation_loop: API call failed after 3 retries. HTTP 429
2026-11-10 20:08:56,968 WARNING [local-y] agent.conversation_loop: API call failed (attempt 1/3) error_type=TimeoutError thread=t provider=antigravity-oauth base_url=agy://local model=g summary=Non-streaming API call timed out
";

    #[test]
    fn hermes_failure_lines_become_classified_failures() {
        let f = failures(LOG);
        assert_eq!(f.len(), 2, "only attempt lines carry the provider");
        assert_eq!(f[0].provider, "claude-subscription-directsdk-experimental");
        assert_eq!(f[0].kind, "rate_limit");
        assert_eq!(
            f[0].resets.as_deref(),
            Some("11:50pm (Africa/Johannesburg)")
        );
        assert_eq!(f[1].provider, "antigravity-oauth");
        assert_eq!(f[1].kind, "timeout");
        assert_eq!(classify("HTTP 401 Unauthorized: token expired").0, "auth");
        assert_eq!(classify("[Errno 32] Broken pipe").0, "broken_pipe");
        assert_eq!(classify("HTTP 503 Service Unavailable").0, "server_error");
        assert_eq!(classify("boom").0, "other");
    }

    #[test]
    fn auth_and_fallback_output_parse() {
        assert_eq!(
            parse_auth("antigravity-oauth: logged in\n  detail: 13 models"),
            Some(true)
        );
        assert_eq!(parse_auth("openrouter: logged out"), Some(false));
        assert_eq!(parse_auth("something else"), None);
        assert!(parse_fallbacks("  No fallback providers configured.", &["openrouter"]).is_empty());
        assert_eq!(
            parse_fallbacks(
                "1. openrouter / deepseek",
                &["openrouter", "antigravity-oauth"]
            ),
            ["openrouter"]
        );
    }

    #[test]
    fn a_rate_limited_active_provider_is_a_problem_with_a_fallback_fix() {
        let f = failures(LOG);
        let now = f[0].at + 600;
        let h = judge(
            &KNOWN[0],
            true,
            Some(true),
            Some(true),
            Some(f[0].clone()),
            now,
        );
        assert_eq!(h.state, "rate_limited");
        assert!(h.detail.contains("11:50pm"));
        assert_eq!(h.fix.as_ref().map(|x| x.0.as_str()), Some("add_fallback"));
        // Hours later the failure no longer counts.
        let later = judge(
            &KNOWN[0],
            true,
            Some(true),
            Some(true),
            Some(f[0].clone()),
            now + RECENT_SECS + 10,
        );
        assert_eq!(later.state, "ok");
        // A disabled plugin beats everything; a signed-out Claude offers sign-in.
        assert_eq!(
            judge(&KNOWN[0], true, Some(true), Some(false), None, now).state,
            "plugin_disabled"
        );
        let out = judge(&KNOWN[0], true, Some(false), Some(true), None, now);
        assert_eq!(out.state, "signed_out");
        assert_eq!(out.fix.unwrap().0, "sign_in_claude");
        // An unused OpenRouter with no key is just not set up.
        assert_eq!(
            judge(&KNOWN[2], false, Some(false), None, None, now).state,
            "not_set_up"
        );
        // An old timeout is not a current failure.
        let t = judge(
            &KNOWN[1],
            true,
            Some(true),
            Some(true),
            Some(f[1].clone()),
            f[1].at + 3600,
        );
        assert_eq!(t.state, "ok");
    }

    #[test]
    fn advice_names_a_signed_in_provider_when_no_fallback_exists() {
        let ok = |k: &Known| judge(k, false, Some(true), Some(true), None, 0);
        let ps = vec![ok(&KNOWN[0]), ok(&KNOWN[1])];
        let a = advice(Some(KNOWN[0].id), &[], &ps).unwrap();
        assert!(a.contains("Antigravity"));
        assert!(advice(
            Some(KNOWN[0].id),
            &["antigravity-subscription-directsdk".into()],
            &ps
        )
        .is_none());
        assert!(advice(None, &[], &ps).is_none());
    }

    #[test]
    fn fixes_are_fixed_commands_only() {
        assert_eq!(
            fix_command("antigravity-subscription-directsdk", "sign_in_antigravity")
                .unwrap()
                .1,
            ["auth", "add", "antigravity-subscription-directsdk"]
        );
        assert_eq!(
            fix_command("openrouter", "enable_plugin"),
            None,
            "no plugin to enable"
        );
        assert_eq!(fix_command("openrouter", "rm -rf"), None);
        assert_eq!(fix_command("unknown", "refresh"), None);
    }

    /// Live: ask the real Hermes about every provider and print the report.
    /// `cargo test --lib -- --ignored provider_health_live --nocapture`
    #[test]
    #[ignore]
    fn provider_health_live() {
        let r = check(chrono::Utc::now().timestamp());
        for h in &r.providers {
            println!(
                "{} in_use={} signed_in={:?} plugin={:?} state={} | {}",
                h.id, h.in_use, h.signed_in, h.plugin_enabled, h.state, h.detail
            );
        }
        println!(
            "active={:?} fallbacks={:?} advice={:?}",
            r.active, r.fallbacks, r.advice
        );
        assert_eq!(r.providers.len(), KNOWN.len());
        assert!(
            r.providers.iter().any(|h| h.signed_in.is_some()),
            "hermes auth status answered"
        );
    }

    #[test]
    fn only_new_problems_are_announced() {
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let c = crate::store::open().unwrap();
        let mut r = Report {
            providers: vec![judge(&KNOWN[0], true, Some(false), Some(true), None, 0)],
            ..Default::default()
        };
        assert_eq!(record_in(&c, &r).len(), 1);
        assert!(
            record_in(&c, &r).is_empty(),
            "same state is not announced twice"
        );
        r.providers[0] = judge(&KNOWN[0], true, Some(true), Some(true), None, 0);
        assert!(record_in(&c, &r).is_empty());
        crate::store::set_test_db(None);
    }

    #[test]
    fn auto_heal_enables_disabled_plugin_and_steers_around_broken_pipe() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let c = crate::store::open().unwrap();
        let now = 1_700_000_000;
        let pipe_fail = Failure {
            at: now - 60,
            provider: KNOWN[0].id.into(),
            kind: "broken_pipe".into(),
            summary: "[Errno 32] Broken pipe".into(),
            resets: None,
        };
        let report = Report {
            providers: vec![
                judge(
                    &KNOWN[0],
                    true,
                    Some(true),
                    Some(true),
                    Some(pipe_fail),
                    now,
                ),
                judge(&KNOWN[1], true, Some(true), Some(false), None, now),
                judge(&KNOWN[2], true, Some(true), Some(true), None, now),
            ],
            ..Default::default()
        };
        assert_eq!(report.providers[0].state, "failing");
        assert_eq!(report.providers[1].state, "plugin_disabled");
        let entries = auto_heal_with(&c, &report, now, false, |args| match args {
            ["plugins", "enable", _] => (true, "enabled".into()),
            ["plugins", "show", _] => (true, "Status: enabled".into()),
            ["auth", "refresh", _] => (true, "ok".into()),
            ["auth", "reset", _] => (true, "ok".into()),
            ["auth", "status", id] => (true, format!("{id}: logged in")),
            _ => (false, "unexpected".into()),
        });
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].provider, KNOWN[0].id);
        assert_eq!(entries[0].outcome, "switched_fallback");
        assert!(is_degraded_in(&c, KNOWN[0].id, now + 10).is_some());
        assert_eq!(entries[1].outcome, "fixed");
        assert!(is_degraded_in(&c, KNOWN[1].id, now + 10).is_none());
        assert_eq!(autofix_log_in(&c).len(), 2);
        // Cooldown prevents re-running immediately.
        assert!(auto_heal_with(&c, &report, now + 10, false, |_| (true, "ok".into())).is_empty());
        crate::store::set_test_db(None);
    }

    #[test]
    fn an_empty_credential_pool_is_never_reported_as_fixed() {
        assert_eq!(
            reset_count("Reset status on 0 antigravity-subscription-directsdk credentials"),
            0
        );
        assert_eq!(reset_count("Reset status on 2 openai credentials"), 2);
        assert_eq!(reset_count("nothing"), 0);

        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let c = crate::store::open().unwrap();
        let now = 1_700_000_000;
        let fail = |kind: &str| Failure {
            at: now - 60,
            provider: KNOWN[0].id.into(),
            kind: kind.into(),
            summary: "x".into(),
            resets: None,
        };
        // The real Hermes answers for a plugin-backed subscription: refresh fails, reset is a no-op.
        let real = |args: &[&str]| match args {
            ["auth", "refresh", id] => (false, format!("No {id} credentials in the pool.")),
            ["auth", "reset", id] => (true, format!("Reset status on 0 {id} credentials")),
            ["auth", "status", id] => (true, format!("{id}: logged in")),
            _ => (false, "unexpected".into()),
        };
        // Transient, only provider: stays open and retries, never "fixed".
        let r = Report {
            providers: vec![judge(
                &KNOWN[0],
                true,
                Some(true),
                Some(true),
                Some(fail("broken_pipe")),
                now,
            )],
            ..Default::default()
        };
        let e = auto_heal_with(&c, &r, now, true, real);
        assert_eq!(e[0].outcome, "retrying");
        assert!(is_degraded_in(&c, KNOWN[0].id, now + 10).is_none());
        // Not transient, only provider, nothing changed: degraded, not fixed.
        let r = Report {
            providers: vec![judge(
                &KNOWN[0],
                true,
                Some(true),
                Some(true),
                Some(fail("auth")),
                now,
            )],
            ..Default::default()
        };
        let e = auto_heal_with(&c, &r, now, true, real);
        assert_ne!(e[0].outcome, "fixed");
        crate::store::set_test_db(None);
    }
}
