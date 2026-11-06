//! Hermes engine: binary discovery, the live status heartbeat, the provider switchboard
//! and the session store (`state.db`).
//!
//! Reads go straight to Hermes's files (`config.yaml`, `.env` key names, `state.db`, the model
//! caches). Writes that Hermes validates or indexes (provider settings, API keys, session
//! deletion) go through the `hermes` CLI, so Studio never corrupts a live config or leaves
//! orphaned rows behind. Session titles are written to `state.db` directly.

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Local, TimeZone};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use serde_yaml::Value as Yaml;

use crate::procs;

pub const EV_STATUS: &str = "agentos://hermes-status";
pub const OLLAMA_URL: &str = "http://localhost:11434/v1";

// ---------------------------------------------------------------------------------------------
// Config access
// ---------------------------------------------------------------------------------------------

pub fn read_config() -> Result<Yaml> {
    let path = procs::hermes_config();
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn ystr(v: Option<&Yaml>) -> Option<String> {
    match v? {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Number(n) => Some(n.to_string()),
        Yaml::Bool(b) => Some(b.to_string()),
        _ => None,
    }
    .filter(|s| !s.trim().is_empty())
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
}

/// `model:` may be a scalar model id or a mapping with `default`, `provider` and `base_url`.
pub fn model_config(cfg: &Yaml) -> ModelConfig {
    match cfg.get("model") {
        Some(Yaml::String(s)) => ModelConfig {
            provider: "auto".into(),
            model: s.clone(),
            base_url: None,
        },
        Some(m @ Yaml::Mapping(_)) => ModelConfig {
            provider: ystr(m.get("provider")).unwrap_or_else(|| "auto".into()),
            model: ystr(m.get("default")).unwrap_or_default(),
            base_url: ystr(m.get("base_url")),
        },
        _ => ModelConfig {
            provider: "auto".into(),
            ..Default::default()
        },
    }
}

/// Names of keys that hold a non-empty value in Hermes's `.env`. Values are never returned.
pub fn env_key_names(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let l = l.strip_prefix("export ").unwrap_or(l);
            let (k, v) = l.split_once('=')?;
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
            (!k.trim().is_empty() && !v.is_empty()).then(|| k.trim().to_string())
        })
        .collect()
}

fn env_keys() -> BTreeSet<String> {
    let mut keys = fs::read_to_string(procs::hermes_home().join(".env"))
        .map(|t| env_key_names(&t))
        .unwrap_or_default();
    for (k, v) in std::env::vars() {
        if k.ends_with("_API_KEY") && !v.trim().is_empty() {
            keys.insert(k);
        }
    }
    keys
}

fn enabled_plugins(cfg: &Yaml) -> Vec<String> {
    cfg.get("plugins")
        .and_then(|p| p.get("enabled"))
        .and_then(Yaml::as_sequence)
        .map(|s| s.iter().filter_map(|v| ystr(Some(v))).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Provider catalog
// ---------------------------------------------------------------------------------------------

pub struct ProviderDef {
    pub id: &'static str,
    pub label: &'static str,
    /// Provider id Hermes understands (`model.provider`).
    pub hermes: &'static str,
    pub kind: &'static str,
    pub key_env: &'static [&'static str],
    pub base_url: Option<&'static str>,
    /// Key in Hermes's models.dev cache.
    pub models_dev: &'static str,
    pub defaults: &'static [&'static str],
    pub blurb: &'static str,
}

pub const SUBSCRIPTION_PLUGIN: &str = "claude-subscription-directsdk-experimental";

pub const PROVIDERS: &[ProviderDef] = &[
    ProviderDef {
        id: "claude-subscription",
        label: "Claude Subscription (DirectSDK)",
        hermes: SUBSCRIPTION_PLUGIN,
        kind: "subscription",
        key_env: &[],
        base_url: Some("process://claude-subscription-directsdk-experimental"),
        models_dev: "anthropic",
        defaults: &[
            "claude-sonnet-5-5[1m]",
            "claude-opus-5-5[1m]",
            "claude-haiku-5-5[1m]",
        ],
        blurb: "Runs on your Claude Pro or Max plan through Claude Code. No API key.",
    },
    ProviderDef {
        id: "openrouter",
        label: "OpenRouter",
        hermes: "openrouter",
        kind: "api_key",
        key_env: &["OPENROUTER_API_KEY"],
        base_url: None,
        models_dev: "openrouter",
        defaults: &[
            "anthropic/claude-sonnet-4.6",
            "google/gemini-2.5-pro",
            "openai/gpt-5.1",
        ],
        blurb: "Hundreds of models behind one key. Any OpenRouter model id works.",
    },
    ProviderDef {
        id: "gemini",
        label: "Google Gemini",
        hermes: "gemini",
        kind: "api_key",
        key_env: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        base_url: None,
        models_dev: "google",
        defaults: &["gemini-2.5-pro", "gemini-2.5-flash", "gemini-2.0-flash"],
        blurb: "Gemini models with a Google AI Studio key.",
    },
    ProviderDef {
        id: "anthropic",
        label: "Anthropic API",
        hermes: "anthropic",
        kind: "api_key",
        key_env: &["ANTHROPIC_API_KEY"],
        base_url: None,
        models_dev: "anthropic",
        defaults: &["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5"],
        blurb: "Pay-as-you-go Claude through the Anthropic Console.",
    },
    ProviderDef {
        id: "openai",
        label: "OpenAI API",
        hermes: "openai-api",
        kind: "api_key",
        key_env: &["OPENAI_API_KEY"],
        base_url: None,
        models_dev: "openai",
        defaults: &["gpt-5.1", "gpt-4.1", "gpt-4.1-mini"],
        blurb: "GPT models with an OpenAI platform key.",
    },
    ProviderDef {
        id: "deepseek",
        label: "DeepSeek",
        hermes: "deepseek",
        kind: "api_key",
        key_env: &["DEEPSEEK_API_KEY"],
        base_url: None,
        models_dev: "deepseek",
        defaults: &["deepseek-v4-pro", "deepseek-v4-flash"],
        blurb: "DeepSeek models with a DeepSeek platform key.",
    },
    ProviderDef {
        id: "nous",
        label: "Nous Portal",
        hermes: "nous",
        kind: "oauth",
        key_env: &["NOUS_API_KEY"],
        base_url: None,
        models_dev: "nous",
        defaults: &["Hermes-4-405B", "Hermes-4-70B"],
        blurb: "Hermes models through Nous Portal sign-in.",
    },
    ProviderDef {
        id: "local",
        label: "Local (Ollama / llama.cpp / vLLM)",
        hermes: "custom",
        kind: "local",
        key_env: &[],
        base_url: Some(OLLAMA_URL),
        models_dev: "",
        defaults: &[],
        blurb: "Models on this machine. Ollama is detected on localhost:11434.",
    },
];

pub fn provider_def(id: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|p| p.id == id || p.hermes == id)
}

/// Map a `model.provider` value from config.yaml to a catalog id.
pub fn catalog_id_for(hermes_provider: &str, base_url: Option<&str>) -> Option<&'static str> {
    if hermes_provider == "custom" {
        let local = base_url
            .map(|u| u.contains("localhost") || u.contains("127.0.0.1"))
            .unwrap_or(true);
        return local.then_some("local");
    }
    PROVIDERS
        .iter()
        .find(|p| p.hermes == hermes_provider)
        .map(|p| p.id)
}

#[derive(Serialize, Clone, Debug)]
pub struct ProviderView {
    pub id: String,
    pub label: String,
    pub hermes_provider: String,
    pub kind: String,
    pub key_env: Vec<String>,
    pub key_set: bool,
    pub ready: bool,
    pub active: bool,
    pub detail: String,
    pub blurb: String,
    pub default_model: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProviderConfig {
    pub active: ModelConfig,
    pub active_id: Option<String>,
    pub active_label: String,
    pub providers: Vec<ProviderView>,
    pub config_path: String,
    pub env_path: String,
}

pub fn claude_logged_in() -> Option<bool> {
    let out = procs::run("claude", &["auth", "status"])?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if let Ok(v) = serde_json::from_str::<Value>(text.trim()) {
        if let Some(b) = v.get("loggedIn").and_then(Value::as_bool) {
            return Some(b);
        }
    }
    let lower = text.to_lowercase();
    Some(
        out.status.success()
            && !lower.contains("not logged in")
            && !lower.contains("\"loggedin\": false"),
    )
}

pub fn provider_label(cfg: &ModelConfig) -> String {
    catalog_id_for(&cfg.provider, cfg.base_url.as_deref())
        .and_then(provider_def)
        .map(|p| p.label.to_string())
        .unwrap_or_else(|| cfg.provider.clone())
}

pub fn get_provider_config() -> Result<ProviderConfig> {
    let cfg = read_config()?;
    let active = model_config(&cfg);
    let active_id = catalog_id_for(&active.provider, active.base_url.as_deref()).map(String::from);
    let keys = env_keys();
    let plugins = enabled_plugins(&cfg);
    let ollama = ollama_models().ok();
    let mut providers = Vec::new();
    for p in PROVIDERS {
        let key_set = p.key_env.iter().any(|k| keys.contains(*k));
        let (ready, detail) = match p.kind {
            "subscription" => {
                let plugin = plugins.iter().any(|x| x == SUBSCRIPTION_PLUGIN);
                if plugin {
                    (
                        true,
                        "Plugin enabled; signs in through Claude Code".to_string(),
                    )
                } else {
                    (
                        false,
                        "Plugin not enabled yet; Activate enables it".to_string(),
                    )
                }
            }
            "local" => match &ollama {
                Some(m) if !m.is_empty() => {
                    (true, format!("Ollama running with {} model(s)", m.len()))
                }
                Some(_) => (
                    false,
                    "Ollama is running but has no models pulled".to_string(),
                ),
                None => (false, "No Ollama server on localhost:11434".to_string()),
            },
            "oauth" => {
                let signed_in = key_set
                    || fs::read_to_string(procs::hermes_home().join("auth.json"))
                        .map(|t| t.contains(&format!("\"{}\"", p.hermes)))
                        .unwrap_or(false);
                if signed_in {
                    (true, "Signed in to Nous Portal".to_string())
                } else {
                    (
                        false,
                        "Not signed in; Sign in opens the Nous Portal flow".to_string(),
                    )
                }
            }
            _ if key_set => (true, format!("{} is set", p.key_env[0])),
            _ => (false, format!("Needs {}", p.key_env.join(" or "))),
        };
        let is_active = active_id.as_deref() == Some(p.id);
        providers.push(ProviderView {
            id: p.id.into(),
            label: p.label.into(),
            hermes_provider: p.hermes.into(),
            kind: p.kind.into(),
            key_env: p.key_env.iter().map(|s| s.to_string()).collect(),
            key_set,
            ready,
            active: is_active,
            detail,
            blurb: p.blurb.into(),
            default_model: if is_active {
                active.model.clone()
            } else {
                p.defaults
                    .first()
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            },
        });
    }
    Ok(ProviderConfig {
        active_label: provider_label(&active),
        active_id,
        active,
        providers,
        config_path: procs::hermes_config().display().to_string(),
        env_path: procs::hermes_home().join(".env").display().to_string(),
    })
}

fn hermes_ok(args: &[&str]) -> Result<String> {
    let (code, out, err) =
        procs::run_with_timeout("hermes", args, Some("y\n"), None, Duration::from_secs(90))?;
    let text = format!("{}{}", out, err);
    let clean = text
        .lines()
        .map(procs::clean_line)
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if code != Some(0) {
        bail!(
            "hermes {} failed: {}",
            args.first().copied().unwrap_or(""),
            clean
        );
    }
    Ok(clean)
}

/// Switch Hermes's default provider and model. API keys are handed to `hermes config set`,
/// which stores them in Hermes's `.env` through its own credential lifecycle.
pub fn set_active_provider(
    provider: &str,
    model: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<(ProviderConfig, Vec<String>)> {
    let def = provider_def(provider).ok_or_else(|| anyhow!("Unknown provider '{provider}'"))?;
    let model = model.trim();
    if model.is_empty() {
        bail!("Pick a model for {}", def.label);
    }
    let mut log = Vec::new();
    if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
        let Some(env) = def.key_env.first() else {
            bail!("{} does not use an API key", def.label);
        };
        hermes_ok(&["config", "set", env, key])?;
        log.push(format!("Stored {env} in Hermes's .env"));
    }
    match def.kind {
        "api_key" => {
            if !def.key_env.iter().any(|k| env_keys().contains(*k)) {
                bail!("Add your {} first", def.key_env.join(" or "));
            }
        }
        "subscription" => {
            if claude_logged_in() == Some(false) {
                bail!("Claude Code is not signed in. Press Sign in on the Claude Subscription card first.");
            }
            let cfg = read_config()?;
            if !enabled_plugins(&cfg)
                .iter()
                .any(|p| p == SUBSCRIPTION_PLUGIN)
            {
                hermes_ok(&["plugins", "enable", SUBSCRIPTION_PLUGIN])?;
                log.push(format!("Enabled plugin {SUBSCRIPTION_PLUGIN}"));
            }
        }
        _ => {}
    }
    hermes_ok(&["config", "set", "model.provider", def.hermes])?;
    hermes_ok(&["config", "set", "model.default", model])?;
    let url = base_url
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .or(def.base_url);
    match url {
        Some(u) => {
            hermes_ok(&["config", "set", "model.base_url", u])?;
        }
        None => {
            let cfg = read_config()?;
            if model_config(&cfg).base_url.is_some() {
                hermes_ok(&["config", "unset", "model.base_url"])?;
            }
        }
    }
    log.push(format!("Hermes now uses {} with {model}", def.label));
    let view = get_provider_config()?;
    if view.active.provider != def.hermes || view.active.model != model {
        bail!(
            "Hermes reported {} / {} after the switch",
            view.active.provider,
            view.active.model
        );
    }
    Ok((view, log))
}

/// Open the sign-in flow for providers that use OAuth or a subscription.
pub fn login_provider(provider: &str) -> Result<String> {
    let def = provider_def(provider).ok_or_else(|| anyhow!("Unknown provider '{provider}'"))?;
    match def.kind {
        "subscription" => {
            procs::spawn_console("claude", &["auth", "login"])?;
            Ok("Opened the Claude sign-in window".into())
        }
        "oauth" => {
            procs::spawn_console("hermes", &["auth", "add", def.hermes])?;
            Ok(format!("Opened the {} sign-in window", def.label))
        }
        _ => bail!("{} uses an API key; paste it on the card", def.label),
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct ProviderTest {
    pub ok: bool,
    pub latency_ms: u128,
    pub reply: String,
    pub error: Option<String>,
}

/// End-to-end check: one real turn through Hermes with the given provider and model.
pub fn test_provider(provider: &str, model: &str, base_url: Option<&str>) -> Result<ProviderTest> {
    let def = provider_def(provider).ok_or_else(|| anyhow!("Unknown provider '{provider}'"))?;
    let mut args: Vec<String> = [
        "chat",
        "-Q",
        "--oneshot",
        "--max-turns",
        "1",
        "--ignore-rules",
        "--provider",
        def.hermes,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if !model.trim().is_empty() {
        args.push("-m".into());
        args.push(model.trim().into());
    }
    if let Some(u) = base_url
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .or(def.base_url)
    {
        if def.kind == "local" {
            args.push("--base-url".into());
            args.push(u.into());
        }
    }
    args.push("-q".into());
    args.push("Reply with exactly the word OK and nothing else.".into());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let started = Instant::now();
    let (code, out, err) =
        procs::run_with_timeout("hermes", &refs, None, None, Duration::from_secs(150))?;
    let latency_ms = started.elapsed().as_millis();
    // The probe is not a conversation: remove its session so history stays clean.
    if let Some(sid) = format!("{out}\n{err}")
        .lines()
        .find_map(|l| {
            procs::clean_line(l)
                .trim()
                .strip_prefix("session_id:")
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
    {
        let _ = procs::run_with_timeout(
            "hermes",
            &["sessions", "delete", "--yes", &sid],
            None,
            None,
            Duration::from_secs(30),
        );
    }
    let reply = out
        .lines()
        .map(procs::clean_line)
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("session_id:") && !t.starts_with("Session:")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let ok = code == Some(0) && !reply.is_empty();
    Ok(ProviderTest {
        ok,
        latency_ms,
        reply: reply.chars().take(400).collect(),
        error: (!ok).then(|| {
            let e = err
                .lines()
                .map(procs::clean_line)
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>();
            e.iter()
                .rev()
                .take(6)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        }),
    })
}

// ---------------------------------------------------------------------------------------------
// Model discovery
// ---------------------------------------------------------------------------------------------

/// Model names from a local Ollama server (`/api/tags`), using a plain HTTP/1.0 request.
pub fn ollama_models() -> Result<Vec<String>> {
    let addr: SocketAddr = "127.0.0.1:11434".parse()?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(250))?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.write_all(b"GET /api/tags HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut buf = String::new();
    s.read_to_string(&mut buf)?;
    let body = buf
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .ok_or_else(|| anyhow!("malformed response from Ollama"))?;
    parse_ollama_tags(body)
}

pub fn parse_ollama_tags(body: &str) -> Result<Vec<String>> {
    let v: Value = serde_json::from_str(body.trim())?;
    Ok(v.get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("name").or_else(|| m.get("model")))
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default())
}

fn cached_models(file: &str, key: &str) -> Vec<String> {
    if key.is_empty() {
        return Vec::new();
    }
    let Ok(text) = fs::read_to_string(procs::hermes_home().join(file)) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    match v.get(key).and_then(|e| e.get("models")) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        Some(Value::Object(o)) => o.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

pub fn merge_unique(groups: Vec<Vec<String>>, cap: usize) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for g in groups {
        for m in g {
            let m = m.trim().to_string();
            if !m.is_empty() && seen.insert(m.clone()) {
                out.push(m);
                if out.len() >= cap {
                    return out;
                }
            }
        }
    }
    out
}

pub fn list_available_models(provider: &str) -> Result<Vec<String>> {
    let def = provider_def(provider).ok_or_else(|| anyhow!("Unknown provider '{provider}'"))?;
    let active = read_config().map(|c| model_config(&c)).unwrap_or_default();
    let current = if active.provider == def.hermes {
        vec![active.model.clone()]
    } else {
        Vec::new()
    };
    if def.kind == "local" {
        let live = ollama_models().unwrap_or_default();
        return Ok(merge_unique(vec![current, live], 400));
    }
    Ok(merge_unique(
        vec![
            current,
            cached_models("provider_models_cache.json", def.hermes),
            def.defaults.iter().map(|s| s.to_string()).collect(),
            cached_models("models_dev_cache.json", def.models_dev),
        ],
        400,
    ))
}

// ---------------------------------------------------------------------------------------------
// Status heartbeat
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug, Default)]
pub struct HermesStatus {
    pub installed: bool,
    pub hermes_path: Option<String>,
    pub version: Option<String>,
    pub claude_path: Option<String>,
    pub claude_version: Option<String>,
    pub provider: String,
    pub provider_id: Option<String>,
    pub provider_label: String,
    pub model: String,
    pub mcp_total: usize,
    pub mcp_enabled: usize,
    pub sessions: i64,
    pub home: String,
    pub config_ok: bool,
    pub error: Option<String>,
    pub path_healed: usize,
    pub checked_at: String,
}

/// Hermes version, Claude Code version, and when they were read.
type VersionCache = Option<(Option<String>, Option<String>, Instant)>;

#[derive(Default)]
pub struct HermesState {
    versions: Mutex<VersionCache>,
    pub path_healed: Mutex<usize>,
}

fn short_version(v: Option<String>) -> Option<String> {
    v.map(|s| {
        let s = s.trim().to_string();
        let re = regex::Regex::new(r"v?(\d+\.\d+\.\d+)").expect("static regex");
        re.captures(&s).map(|c| format!("v{}", &c[1])).unwrap_or(s)
    })
}

pub fn mcp_counts(cfg: &Yaml) -> (usize, usize) {
    let Some(map) = cfg.get("mcp_servers").and_then(Yaml::as_mapping) else {
        return (0, 0);
    };
    let enabled = map
        .values()
        .filter(|v| v.get("enabled").and_then(Yaml::as_bool).unwrap_or(true))
        .count();
    (map.len(), enabled)
}

impl HermesState {
    pub fn status(&self, refresh_versions: bool) -> HermesStatus {
        let hermes_path = procs::find("hermes");
        let claude_path = procs::find("claude");
        let (version, claude_version) = {
            let cached = self.versions.lock().ok().and_then(|g| g.clone());
            match cached {
                Some((h, c, at))
                    if !refresh_versions && at.elapsed() < Duration::from_secs(600) =>
                {
                    (h, c)
                }
                _ => {
                    let h = short_version(procs::version("hermes"));
                    let c = short_version(procs::version("claude"));
                    if let Ok(mut g) = self.versions.lock() {
                        *g = Some((h.clone(), c.clone(), Instant::now()));
                    }
                    (h, c)
                }
            }
        };
        let mut st = HermesStatus {
            installed: hermes_path.is_some(),
            hermes_path: hermes_path.map(|p| p.display().to_string()),
            claude_path: claude_path.map(|p| p.display().to_string()),
            version,
            claude_version,
            home: procs::hermes_home().display().to_string(),
            path_healed: self.path_healed.lock().map(|g| *g).unwrap_or(0),
            checked_at: crate::vault::now(),
            ..Default::default()
        };
        match read_config() {
            Ok(cfg) => {
                let m = model_config(&cfg);
                st.provider_label = provider_label(&m);
                st.provider_id =
                    catalog_id_for(&m.provider, m.base_url.as_deref()).map(String::from);
                st.provider = m.provider;
                st.model = m.model;
                (st.mcp_total, st.mcp_enabled) = mcp_counts(&cfg);
                st.config_ok = true;
            }
            Err(e) => st.error = Some(format!("{e:#}")),
        }
        st.sessions = open_ro()
            .and_then(|c| Ok(c.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?))
            .unwrap_or(0);
        st
    }
}

// ---------------------------------------------------------------------------------------------
// Session store
// ---------------------------------------------------------------------------------------------

pub fn state_db() -> PathBuf {
    procs::hermes_home().join("state.db")
}

fn open_ro() -> Result<Connection> {
    open_db(&state_db(), true)
}

fn open_db(path: &Path, read_only: bool) -> Result<Connection> {
    if !path.exists() {
        bail!("Hermes session store not found at {}", path.display());
    }
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
    };
    let c = Connection::open_with_flags(path, flags)
        .with_context(|| format!("opening {}", path.display()))?;
    c.busy_timeout(Duration::from_secs(5))?;
    Ok(c)
}

fn columns(c: &Connection, table: &str) -> Result<BTreeSet<String>> {
    let mut stmt = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    Ok(cols)
}

fn col_or(cols: &BTreeSet<String>, name: &str, fallback: &str) -> String {
    if cols.contains(name) {
        format!("s.{name}")
    } else {
        fallback.to_string()
    }
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub source: String,
    pub model: String,
    pub provider: String,
    pub started_at: f64,
    pub last_activity: f64,
    pub message_count: i64,
    pub tool_call_count: i64,
    pub cwd: String,
    pub repo_root: String,
    pub cost_usd: f64,
    pub preview: String,
    pub pinned: bool,
}

pub fn list_sessions_in(
    c: &Connection,
    query: Option<&str>,
    limit: usize,
) -> Result<Vec<SessionSummary>> {
    let cols = columns(c, "sessions")?;
    let mut filters = vec!["COALESCE(s.message_count, 0) > 0".to_string()];
    for flag in ["hidden", "archived"] {
        if cols.contains(flag) {
            filters.push(format!("COALESCE(s.{flag}, 0) = 0"));
        }
    }
    let activity = if cols.contains("last_activity_at") {
        "COALESCE(s.last_activity_at, s.ended_at, s.started_at)"
    } else {
        "COALESCE(s.ended_at, s.started_at)"
    };
    let preview = "(SELECT substr(m.content, 1, 200) FROM messages m WHERE m.session_id = s.id AND m.role = 'user' AND COALESCE(m.content, '') <> '' ORDER BY m.id LIMIT 1)";
    let sql = format!(
        "SELECT s.id, {title}, COALESCE(s.source, ''), COALESCE(s.model, ''), {provider}, s.started_at, {activity}, \
         COALESCE(s.message_count, 0), COALESCE(s.tool_call_count, 0), {cwd}, {root}, {cost}, COALESCE({preview}, ''), {pinned} \
         FROM sessions s WHERE {filters} {search} ORDER BY {activity} DESC LIMIT ?1",
        title = col_or(&cols, "title", "NULL"),
        provider = col_or(&cols, "billing_provider", "''"),
        cwd = col_or(&cols, "cwd", "''"),
        root = col_or(&cols, "git_repo_root", "''"),
        cost = col_or(&cols, "estimated_cost_usd", "0"),
        pinned = col_or(&cols, "pinned", "0"),
        filters = filters.join(" AND "),
        search = if query.map(str::trim).filter(|q| !q.is_empty()).is_some() {
            format!(
                "AND (COALESCE({t}, '') LIKE ?2 OR s.id LIKE ?2 OR COALESCE({cwd}, '') LIKE ?2 OR COALESCE({preview}, '') LIKE ?2)",
                t = col_or(&cols, "title", "NULL"),
                cwd = col_or(&cols, "cwd", "''"),
            )
        } else {
            String::new()
        },
    );
    let mut stmt = c.prepare(&sql)?;
    let map = |r: &rusqlite::Row| -> rusqlite::Result<SessionSummary> {
        let preview: String = r.get(12)?;
        let title: Option<String> = r.get(1)?;
        Ok(SessionSummary {
            id: r.get(0)?,
            title: title
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| first_words(&preview, 60)),
            source: r.get(2)?,
            model: r.get(3)?,
            provider: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            started_at: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
            last_activity: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
            message_count: r.get(7)?,
            tool_call_count: r.get(8)?,
            cwd: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
            repo_root: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
            cost_usd: r.get::<_, Option<f64>>(11)?.unwrap_or(0.0),
            preview,
            pinned: r.get::<_, Option<i64>>(13)?.unwrap_or(0) != 0,
        })
    };
    let rows = match query.map(str::trim).filter(|q| !q.is_empty()) {
        Some(q) => stmt
            .query_map(params![limit as i64, format!("%{q}%")], map)?
            .collect::<std::result::Result<Vec<_>, _>>()?,
        None => stmt
            .query_map(params![limit as i64], map)?
            .collect::<std::result::Result<Vec<_>, _>>()?,
    };
    Ok(rows)
}

fn first_words(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return if one_line.is_empty() {
            "Untitled session".into()
        } else {
            one_line
        };
    }
    format!("{}...", one_line.chars().take(max - 3).collect::<String>())
}

pub fn list_hermes_sessions(query: Option<&str>, limit: usize) -> Result<Vec<SessionSummary>> {
    list_sessions_in(&open_ro()?, query, limit.clamp(1, 1000))
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ToolCallView {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct ChatMessage {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub tool_calls: Vec<ToolCallView>,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
    pub timestamp: f64,
}

/// Text from a message `content` column, which may hold plain text or a JSON list of parts.
pub fn content_text(raw: &str) -> String {
    let t = raw.trim_start();
    if t.starts_with('[') {
        if let Ok(Value::Array(parts)) = serde_json::from_str::<Value>(t) {
            let texts: Vec<String> = parts
                .iter()
                .filter_map(|p| {
                    p.get("text")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .or_else(|| p.as_str().map(String::from))
                })
                .collect();
            if !texts.is_empty() {
                return texts.join("\n");
            }
        }
    }
    raw.to_string()
}

/// Tool calls in either OpenAI (`function.name/arguments`) or Anthropic (`name/input`) shape.
pub fn parse_tool_calls(raw: &str) -> Vec<ToolCallView> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|c| {
            let f = c.get("function");
            let name = f
                .and_then(|f| f.get("name"))
                .or_else(|| c.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string();
            let args = f
                .and_then(|f| f.get("arguments"))
                .or_else(|| c.get("input"))
                .or_else(|| c.get("arguments"))
                .map(|a| match a {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            let id = c
                .get("id")
                .or_else(|| c.get("call_id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            ToolCallView {
                id,
                name,
                arguments: args,
            }
        })
        .collect()
}

pub fn load_messages_in(
    c: &Connection,
    session_id: &str,
    limit: usize,
) -> Result<Vec<ChatMessage>> {
    let cols = columns(c, "messages")?;
    let active = if cols.contains("active") {
        "AND COALESCE(active, 1) = 1"
    } else {
        ""
    };
    let order = if cols.contains("display_order") {
        "COALESCE(display_order, id), id"
    } else {
        "id"
    };
    let sql = format!(
        "SELECT id, role, COALESCE(content, ''), tool_calls, tool_name, tool_call_id, timestamp FROM \
         (SELECT * FROM messages WHERE session_id = ?1 {active} ORDER BY {order} DESC LIMIT ?2) ORDER BY {order}"
    );
    let mut stmt = c.prepare(&sql)?;
    let rows = stmt
        .query_map(params![session_id, limit as i64], |r| {
            let content: String = r.get(2)?;
            let calls: Option<String> = r.get(3)?;
            Ok(ChatMessage {
                id: r.get(0)?,
                role: r.get(1)?,
                content: content_text(&content),
                tool_calls: calls.as_deref().map(parse_tool_calls).unwrap_or_default(),
                tool_name: r.get(4)?,
                tool_call_id: r.get(5)?,
                timestamp: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn load_session_messages(session_id: &str) -> Result<Vec<ChatMessage>> {
    let c = open_ro()?;
    let exists: Option<String> = c
        .query_row("SELECT id FROM sessions WHERE id = ?1", [session_id], |r| {
            r.get(0)
        })
        .optional()?;
    if exists.is_none() {
        bail!("Session {session_id} not found");
    }
    load_messages_in(&c, session_id, 2000)
}

pub fn rename_session_in(c: &Connection, session_id: &str, title: &str) -> Result<()> {
    let cols = columns(c, "sessions")?;
    let n = if cols.contains("title_source") {
        c.execute(
            "UPDATE sessions SET title = ?1, title_source = 'user' WHERE id = ?2",
            params![title, session_id],
        )?
    } else {
        c.execute(
            "UPDATE sessions SET title = ?1 WHERE id = ?2",
            params![title, session_id],
        )?
    };
    if n == 0 {
        bail!("Session {session_id} not found");
    }
    Ok(())
}

pub fn rename_session(session_id: &str, title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() {
        bail!("Title cannot be empty");
    }
    let direct = open_db(&state_db(), false).and_then(|c| rename_session_in(&c, session_id, title));
    match direct {
        Ok(()) => Ok(()),
        Err(e) if e.to_string().contains("not found") => Err(e),
        Err(_) => hermes_ok(&["sessions", "rename", session_id, title]).map(|_| ()),
    }
}

/// Deletion runs through Hermes: sessions are referenced by usage, admission and worker
/// tables plus the FTS index and JSONL transcripts, which Hermes cleans up together.
pub fn delete_session(session_id: &str) -> Result<()> {
    hermes_ok(&["sessions", "delete", "--yes", session_id])?;
    let still: Option<String> = open_ro()?
        .query_row("SELECT id FROM sessions WHERE id = ?1", [session_id], |r| {
            r.get(0)
        })
        .optional()?;
    if still.is_some() {
        bail!("Hermes kept session {session_id}");
    }
    Ok(())
}

fn fmt_ts(ts: f64) -> String {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn quote_block(text: &str) -> String {
    text.lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!(
        "{}\n... ({} more characters)",
        text.chars().take(max).collect::<String>(),
        text.chars().count() - max
    )
}

pub fn session_markdown(s: &SessionSummary, msgs: &[ChatMessage]) -> String {
    let yq = |v: &str| serde_json::to_string(v).unwrap_or_else(|_| "\"\"".into());
    let mut md = format!(
        "---\ntype: hermes-session\nsession_id: {}\ntitle: {}\nmodel: {}\nprovider: {}\ncwd: {}\nstarted: {}\nexported: {}\nmessages: {}\ntool_calls: {}\ntags: [hermes, session]\n---\n\n# {}\n\n> [!info] Hermes session\n> `{}` - {} messages, {} tool calls, started {}{}\n",
        s.id,
        yq(&s.title),
        yq(&s.model),
        yq(&s.provider),
        yq(&s.cwd),
        fmt_ts(s.started_at),
        crate::vault::now(),
        s.message_count,
        s.tool_call_count,
        s.title,
        s.id,
        s.message_count,
        s.tool_call_count,
        fmt_ts(s.started_at),
        if s.cwd.is_empty() { String::new() } else { format!(" in `{}`", s.cwd) },
    );
    for m in msgs {
        match m.role.as_str() {
            "user" => {
                md.push_str(&format!(
                    "\n## You ({})\n\n{}\n",
                    fmt_ts(m.timestamp),
                    m.content.trim()
                ));
            }
            "assistant" => {
                if !m.content.trim().is_empty() {
                    md.push_str(&format!(
                        "\n## Hermes ({})\n\n{}\n",
                        fmt_ts(m.timestamp),
                        m.content.trim()
                    ));
                }
                for call in &m.tool_calls {
                    md.push_str(&format!(
                        "\n> [!example]- Tool call: {}\n{}\n",
                        call.name,
                        quote_block(&format!("```json\n{}\n```", clip(&call.arguments, 2000)))
                    ));
                }
            }
            "tool" => {
                md.push_str(&format!(
                    "\n> [!quote]- Result: {}\n{}\n",
                    m.tool_name.as_deref().unwrap_or("tool"),
                    quote_block(&format!("```text\n{}\n```", clip(m.content.trim(), 2000)))
                ));
            }
            _ => {}
        }
    }
    md
}

pub fn export_session_to_obsidian(session_id: &str, vault: &crate::vault::Vault) -> Result<String> {
    let c = open_ro()?;
    let summary = list_sessions_in(&c, Some(session_id), 5)?
        .into_iter()
        .find(|s| s.id == session_id)
        .ok_or_else(|| anyhow!("Session {session_id} not found"))?;
    let msgs = load_messages_in(&c, session_id, 5000)?;
    let date = Local
        .timestamp_opt(summary.started_at as i64, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
    let rel = format!(
        "03_Memory/Hermes_Sessions/{date}_{}.md",
        crate::vault::slug(&summary.title, 60)
    );
    let path = vault.resolve_rel(&rel)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    vault.write_note(&rel, &session_markdown(&summary, &msgs))?;
    Ok(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(s: &str) -> Yaml {
        serde_yaml::from_str(s).unwrap()
    }

    #[test]
    fn reads_model_mapping_and_scalar() {
        let m = model_config(&yaml(
            "model:\n  default: \"claude-opus-5-5[1m]\"\n  provider: \"claude-subscription-directsdk-experimental\"\n  base_url: \"process://claude-subscription-directsdk-experimental\"\n",
        ));
        assert_eq!(m.model, "claude-opus-5-5[1m]");
        assert_eq!(
            catalog_id_for(&m.provider, m.base_url.as_deref()),
            Some("claude-subscription")
        );
        let s = model_config(&yaml("model: anthropic/claude-sonnet-4\n"));
        assert_eq!(
            (s.provider.as_str(), s.model.as_str()),
            ("auto", "anthropic/claude-sonnet-4")
        );
        let none = model_config(&yaml("other: 1\n"));
        assert_eq!(none.provider, "auto");
    }

    #[test]
    fn maps_every_catalog_provider_both_ways() {
        for p in PROVIDERS {
            let back = catalog_id_for(p.hermes, p.base_url);
            assert_eq!(back, Some(p.id), "{}", p.id);
            assert!(provider_def(p.id).is_some());
        }
        assert_eq!(
            catalog_id_for("custom", Some("http://127.0.0.1:8000/v1")),
            Some("local")
        );
        assert_eq!(
            catalog_id_for("custom", Some("https://api.example.com/v1")),
            None
        );
        assert_eq!(catalog_id_for("unknown-provider", None), None);
    }

    #[test]
    fn env_names_only_counts_values() {
        let keys = env_key_names(
            "# OPENROUTER_API_KEY=sk\nOPENAI_API_KEY=\nexport GEMINI_API_KEY=\"g\"\nDEEPSEEK_API_KEY='d'\nnot a line\n",
        );
        assert_eq!(
            keys.into_iter().collect::<Vec<_>>(),
            vec!["DEEPSEEK_API_KEY".to_string(), "GEMINI_API_KEY".to_string()]
        );
    }

    #[test]
    fn parses_ollama_and_merges_models() {
        let m = parse_ollama_tags(r#"{"models":[{"name":"llama3.3:70b"},{"model":"qwen3:8b"}]}"#)
            .unwrap();
        assert_eq!(m, vec!["llama3.3:70b", "qwen3:8b"]);
        let merged = merge_unique(
            vec![
                vec!["a".into(), "b".into()],
                vec!["b".into(), " ".into(), "c".into()],
            ],
            10,
        );
        assert_eq!(merged, vec!["a", "b", "c"]);
        assert_eq!(
            merge_unique(vec![vec!["a".into(), "b".into()]], 1),
            vec!["a"]
        );
    }

    #[test]
    fn parses_tool_calls_in_both_shapes() {
        let openai = parse_tool_calls(
            r#"[{"id":"toolu_1","type":"function","function":{"name":"mcp__agentos_hub__fetch_pending_tasks","arguments":"{\"agent_name\":\"all\"}"}}]"#,
        );
        assert_eq!(openai[0].name, "mcp__agentos_hub__fetch_pending_tasks");
        assert_eq!(openai[0].arguments, r#"{"agent_name":"all"}"#);
        let anthropic =
            parse_tool_calls(r#"[{"id":"t2","name":"terminal","input":{"command":"ls"}}]"#);
        assert_eq!(anthropic[0].arguments, r#"{"command":"ls"}"#);
        assert!(parse_tool_calls("not json").is_empty());
        assert_eq!(
            content_text(r#"[{"type":"text","text":"hi"},{"type":"text","text":"there"}]"#),
            "hi\nthere"
        );
        assert_eq!(content_text("[plain] text"), "[plain] text");
    }

    fn fixture_db() -> (tempfile::TempDir, Connection) {
        let d = tempfile::tempdir().unwrap();
        let c = Connection::open(d.path().join("state.db")).unwrap();
        c.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, source TEXT NOT NULL, model TEXT, started_at REAL NOT NULL, ended_at REAL, \
             message_count INTEGER DEFAULT 0, tool_call_count INTEGER DEFAULT 0, cwd TEXT, git_repo_root TEXT, billing_provider TEXT, \
             estimated_cost_usd REAL, title TEXT, title_source TEXT, last_activity_at REAL, archived INTEGER DEFAULT 0, hidden INTEGER DEFAULT 0, pinned INTEGER DEFAULT 0);
             CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT, \
             tool_call_id TEXT, tool_calls TEXT, tool_name TEXT, timestamp REAL NOT NULL, active INTEGER NOT NULL DEFAULT 1, display_order INTEGER);
             INSERT INTO sessions VALUES ('s1','cli','claude-opus-5-5',100,110,4,1,'C:/repo','C:/repo','claude-subscription-directsdk-experimental',0.12,'Fix the auth flow','llm',200,0,0,1);
             INSERT INTO sessions VALUES ('s2','oneshot','m',50,60,2,0,'','','p',0,NULL,NULL,300,0,0,0);
             INSERT INTO sessions VALUES ('s3','oneshot','m',50,60,0,0,'','','p',0,'empty',NULL,400,0,0,0);
             INSERT INTO sessions VALUES ('s4','cli','m',50,60,2,0,'','','p',0,'archived one',NULL,500,1,0,0);
             INSERT INTO messages (session_id, role, content, timestamp, display_order) VALUES ('s1','user','Please fix the login bug',101,1);
             INSERT INTO messages (session_id, role, content, tool_calls, timestamp, display_order) VALUES ('s1','assistant','', '[{\"id\":\"c1\",\"function\":{\"name\":\"terminal\",\"arguments\":\"{}\"}}]',102,2);
             INSERT INTO messages (session_id, role, content, tool_call_id, tool_name, timestamp, display_order) VALUES ('s1','tool','ok','c1','terminal',103,3);
             INSERT INTO messages (session_id, role, content, timestamp, display_order, active) VALUES ('s1','assistant','stale draft',104,4,0);
             INSERT INTO messages (session_id, role, content, timestamp, display_order) VALUES ('s1','assistant','Fixed it.',105,5);
             INSERT INTO messages (session_id, role, content, timestamp, display_order) VALUES ('s2','user','What is the weather like on the moon today and tomorrow and next week please',51,1);",
        )
        .unwrap();
        (d, c)
    }

    #[test]
    fn session_store_lists_filters_and_titles() {
        let (_d, c) = fixture_db();
        let all = list_sessions_in(&c, None, 50).unwrap();
        assert_eq!(
            all.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["s2", "s1"]
        );
        assert!(
            all[0].title.ends_with("..."),
            "untitled sessions use the first prompt: {}",
            all[0].title
        );
        assert_eq!(all[1].preview, "Please fix the login bug");
        assert!(all[1].pinned);
        let hits = list_sessions_in(&c, Some("login"), 50).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "s1");
    }

    #[test]
    fn session_store_loads_active_messages_and_renames() {
        let (_d, c) = fixture_db();
        let msgs = load_messages_in(&c, "s1", 100).unwrap();
        assert_eq!(msgs.len(), 4, "inactive rows are skipped");
        assert_eq!(msgs[1].tool_calls[0].name, "terminal");
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(msgs[3].content, "Fixed it.");
        rename_session_in(&c, "s1", "Auth flow fix").unwrap();
        let t: (String, String) = c
            .query_row(
                "SELECT title, title_source FROM sessions WHERE id='s1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(t, ("Auth flow fix".to_string(), "user".to_string()));
        assert!(rename_session_in(&c, "nope", "x").is_err());
        let summary = list_sessions_in(&c, Some("s1"), 5).unwrap().remove(0);
        let md = session_markdown(&summary, &msgs);
        assert!(md.contains("type: hermes-session"));
        assert!(md.contains("## You"));
        assert!(md.contains("Tool call: terminal"));
        assert!(md.contains("> [!quote]- Result: terminal"));
        assert!(!md.contains('\u{2014}') && !md.contains('\u{2013}'));
    }
}
