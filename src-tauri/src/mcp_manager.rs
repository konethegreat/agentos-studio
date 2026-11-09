//! Universal MCP registry: one view of every MCP server configured in Hermes
//! (`config.yaml`), Claude Code (`~/.claude.json`), Qoder (`~/.qoder/settings.json`) and
//! Antigravity (`~/.gemini/config/mcp_config.json`), plus add, remove, enable, test and the
//! one-click catalog.
//!
//! Hermes and Claude Code are changed through their own CLIs (`hermes mcp add/remove`,
//! `claude mcp add/remove`) so their validation runs. Qoder and Antigravity have no CLI, so
//! their JSON files are merged in place with a one-time backup and the original line endings.
//! Stdio servers are tested by a native MCP client (`initialize` + `tools/list`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use serde_yaml::Value as Yaml;

use crate::procs;

pub const AGENTS: [&str; 4] = ["hermes", "claude-code", "qoder", "antigravity"];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerDef {
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub env: BTreeMap<String, String>,
    pub auth: Option<String>,
    pub enabled: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct AgentReg {
    pub agent: String,
    pub enabled: bool,
    pub matches_primary: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct McpServer {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    /// Names only; values stay in the agents' config files.
    pub env_keys: Vec<String>,
    pub auth: Option<String>,
    pub agents: Vec<AgentReg>,
    pub is_agentos_hub: bool,
    pub legacy_python: bool,
    /// agentos-hub only: the registration does not point at this app version's staged hub binary,
    /// so agents run an older hub than the app (see `stage_hub_binary`).
    pub hub_outdated: bool,
}

// ---------------------------------------------------------------------------------------------
// Reading the four sources
// ---------------------------------------------------------------------------------------------

fn yaml_str(v: Option<&Yaml>) -> Option<String> {
    match v? {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Number(n) => Some(n.to_string()),
        Yaml::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn hermes_servers(cfg: &Yaml) -> BTreeMap<String, ServerDef> {
    let mut out = BTreeMap::new();
    let Some(map) = cfg.get("mcp_servers").and_then(Yaml::as_mapping) else {
        return out;
    };
    for (k, v) in map {
        let Some(name) = yaml_str(Some(k)) else {
            continue;
        };
        let url = yaml_str(v.get("url"));
        let env = v
            .get("env")
            .and_then(Yaml::as_mapping)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| {
                        Some((yaml_str(Some(k))?, yaml_str(Some(v)).unwrap_or_default()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.insert(
            name,
            ServerDef {
                transport: if url.is_some() {
                    "http".into()
                } else {
                    "stdio".into()
                },
                command: yaml_str(v.get("command")),
                args: v
                    .get("args")
                    .and_then(Yaml::as_sequence)
                    .map(|s| s.iter().filter_map(|a| yaml_str(Some(a))).collect())
                    .unwrap_or_default(),
                url,
                env,
                auth: yaml_str(v.get("auth")),
                enabled: v.get("enabled").and_then(Yaml::as_bool).unwrap_or(true),
            },
        );
    }
    out
}

/// `mcpServers` from a Claude, Qoder or Antigravity style JSON document.
pub fn json_servers(doc: &Value) -> BTreeMap<String, ServerDef> {
    let mut out = BTreeMap::new();
    let Some(map) = doc.get("mcpServers").and_then(Value::as_object) else {
        return out;
    };
    for (name, v) in map {
        let url = v
            .get("url")
            .or_else(|| v.get("serverUrl"))
            .or_else(|| v.get("httpUrl"))
            .and_then(Value::as_str)
            .map(String::from);
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        out.insert(
            name.clone(),
            ServerDef {
                transport: if url.is_some() || kind == "http" || kind == "sse" {
                    "http".into()
                } else {
                    "stdio".into()
                },
                command: v.get("command").and_then(Value::as_str).map(String::from),
                args: v
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default(),
                url,
                env: v
                    .get("env")
                    .and_then(Value::as_object)
                    .map(|m| {
                        m.iter()
                            .map(|(k, v)| {
                                (
                                    k.clone(),
                                    v.as_str()
                                        .map(String::from)
                                        .unwrap_or_else(|| v.to_string()),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                auth: None,
                enabled: !v.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            },
        );
    }
    out
}

fn read_json(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_else(|| json!({}))
}

fn agent_file(agent: &str) -> Option<PathBuf> {
    match agent {
        "claude-code" => Some(procs::claude_json()),
        "qoder" => Some(procs::qoder_settings()),
        "antigravity" => Some(procs::gemini_mcp_config()),
        _ => None,
    }
}

/// Every agent's servers: agent id -> (server name -> definition).
pub fn all_sources() -> BTreeMap<String, BTreeMap<String, ServerDef>> {
    let mut out = BTreeMap::new();
    out.insert(
        "hermes".to_string(),
        crate::hermes_engine::read_config()
            .map(|c| hermes_servers(&c))
            .unwrap_or_default(),
    );
    for agent in ["claude-code", "qoder", "antigravity"] {
        let path = agent_file(agent).unwrap_or_default();
        out.insert(agent.to_string(), json_servers(&read_json(&path)));
    }
    out
}

fn same_target(a: &ServerDef, b: &ServerDef) -> bool {
    let norm = |s: &str| s.replace('\\', "/").to_lowercase();
    match (&a.url, &b.url) {
        (Some(x), Some(y)) => x.trim_end_matches('/') == y.trim_end_matches('/'),
        (None, None) => {
            a.command.as_deref().map(norm) == b.command.as_deref().map(norm)
                && a.args.iter().map(|s| norm(s)).collect::<Vec<_>>()
                    == b.args.iter().map(|s| norm(s)).collect::<Vec<_>>()
        }
        _ => false,
    }
}

pub fn merge_registry(sources: &BTreeMap<String, BTreeMap<String, ServerDef>>) -> Vec<McpServer> {
    let names: BTreeSet<&String> = sources.values().flat_map(|m| m.keys()).collect();
    let mut out = Vec::new();
    for name in names {
        let primary = AGENTS
            .iter()
            .find_map(|a| sources.get(*a).and_then(|m| m.get(name)))
            .cloned()
            .unwrap_or_default();
        let agents = AGENTS
            .iter()
            .filter_map(|a| {
                let d = sources.get(*a)?.get(name)?;
                Some(AgentReg {
                    agent: a.to_string(),
                    enabled: d.enabled,
                    matches_primary: same_target(d, &primary),
                })
            })
            .collect();
        let legacy_python = sources.values().filter_map(|m| m.get(name)).any(|d| {
            d.args.iter().any(|a| a.ends_with("agent_bus_server.py"))
                || d.command
                    .as_deref()
                    .is_some_and(|c| c.ends_with("agent_bus_server.py"))
        });
        let mut env_keys: Vec<String> = primary.env.keys().cloned().collect();
        env_keys.sort();
        out.push(McpServer {
            name: name.clone(),
            transport: primary.transport.clone(),
            command: primary.command.clone(),
            args: primary.args.clone(),
            url: primary.url.clone(),
            env_keys,
            auth: primary.auth.clone(),
            agents,
            is_agentos_hub: name == "agentos-hub",
            legacy_python,
            hub_outdated: false,
        });
    }
    out
}

pub fn get_all_mcp_servers() -> Vec<McpServer> {
    let mut servers = merge_registry(&all_sources());
    mark_hub_outdated(
        &mut servers,
        &hub_binary_for(&hub_root(), env!("CARGO_PKG_VERSION")),
    );
    servers
}

/// Agents whose agentos-hub registration is present and enabled, which is the set a per-agent
/// tool policy can affect. Falls back to the four known agents when nothing is registered yet,
/// so a first-run machine can prepare the policy before the agents are connected.
pub fn hub_agents() -> Vec<String> {
    let mut out: Vec<String> = get_all_mcp_servers()
        .into_iter()
        .find(|s| s.is_agentos_hub)
        .map(|s| {
            s.agents
                .into_iter()
                .filter(|a| a.enabled)
                .map(|a| a.agent)
                .collect()
        })
        .unwrap_or_default();
    if out.is_empty() {
        out = AGENTS.iter().map(|a| a.to_string()).collect();
    }
    out.sort();
    out.dedup();
    out
}

/// Flag agentos-hub when any agent's registration is not this version's staged binary.
pub fn mark_hub_outdated(servers: &mut [McpServer], expected: &Path) {
    let norm = |s: &str| s.replace('\\', "/").to_lowercase();
    let want = norm(&expected.display().to_string());
    let sources = all_sources();
    for s in servers.iter_mut().filter(|s| s.is_agentos_hub) {
        s.hub_outdated = AGENTS.iter().any(|a| {
            sources
                .get(*a)
                .and_then(|m| m.get(&s.name))
                .is_some_and(|d| d.command.as_deref().map(norm) != Some(want.clone()))
        });
    }
}

// ---------------------------------------------------------------------------------------------
// The agentos-hub binary agents run
// ---------------------------------------------------------------------------------------------

/// Hub binaries live outside both the build tree and the app install folder, one folder per
/// version: `%LOCALAPPDATA%/AgentOS Hub/<version>/agentos-studio.exe`. Agent sessions hold the hub
/// exe open for as long as they run, so registering the app's own exe would freeze the app (no
/// installer or updater could replace it), and a single shared path could never be upgraded
/// while any session is alive. A new version gets a new folder; old ones stay for the sessions
/// still using them.
pub fn hub_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(crate::settings::home)
        .join("AgentOS Hub")
}

pub fn hub_binary_for(root: &Path, version: &str) -> PathBuf {
    root.join(version).join("agentos-studio.exe")
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) if x.len() == y.len() => fs::read(a).ok() == fs::read(b).ok(),
        _ => false,
    }
}

/// Copy `src` to the versioned hub path under `root`. Idempotent: an identical copy is reused,
/// and the copy goes through a temporary file so a half-written exe is never registered.
pub fn stage_hub_binary_at(src: &Path, root: &Path, version: &str) -> Result<PathBuf> {
    let dst = hub_binary_for(root, version);
    if same_bytes(src, &dst) {
        return Ok(dst);
    }
    let dir = dst.parent().context("hub path has no folder")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join("agentos-studio.exe.partial");
    fs::copy(src, &tmp)
        .with_context(|| format!("copying {} to {}", src.display(), tmp.display()))?;
    if dst.exists() {
        fs::remove_file(&dst).with_context(|| {
            format!(
                "{} differs from this build and is in use; close the agent sessions using it, or bump the version",
                dst.display()
            )
        })?;
    }
    fs::rename(&tmp, &dst).with_context(|| format!("placing {}", dst.display()))?;
    Ok(dst)
}

/// Stage this running app's exe as the hub for its own version.
pub fn stage_hub_binary() -> Result<PathBuf> {
    stage_hub_binary_at(
        &std::env::current_exe()?,
        &hub_root(),
        env!("CARGO_PKG_VERSION"),
    )
}

fn find_def(name: &str) -> Result<ServerDef> {
    let sources = all_sources();
    AGENTS
        .iter()
        .find_map(|a| sources.get(*a).and_then(|m| m.get(name)))
        .cloned()
        .ok_or_else(|| anyhow!("No MCP server named '{name}' in any agent config"))
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct ServerSpec {
    pub name: String,
    pub transport: String,
    pub command_or_url: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env_vars: BTreeMap<String, String>,
    #[serde(default)]
    pub auth: Option<String>,
}

impl ServerSpec {
    pub fn validate(&self) -> Result<()> {
        let ok_name = !self.name.is_empty()
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !ok_name {
            bail!("Server names use letters, digits, '-' and '_' only");
        }
        match self.transport.as_str() {
            "stdio" | "http" => {}
            other => bail!("Unknown transport '{other}' (use stdio or http)"),
        }
        if self.command_or_url.trim().is_empty() {
            bail!("Give a command (stdio) or URL (http)");
        }
        if self.transport == "http" && !self.command_or_url.starts_with("http") {
            bail!("HTTP servers need an http(s) URL");
        }
        for k in self.env_vars.keys() {
            if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                bail!("Invalid environment variable name '{k}'");
            }
        }
        Ok(())
    }

    fn json_entry(&self, agent: &str) -> Value {
        if self.transport == "http" {
            let key = if agent == "antigravity" {
                "serverUrl"
            } else {
                "url"
            };
            return json!({ key: self.command_or_url });
        }
        let mut v = json!({"command": self.command_or_url, "args": self.args});
        if !self.env_vars.is_empty() {
            v["env"] = json!(self.env_vars);
        }
        v
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct AgentOutcome {
    pub agent: String,
    pub ok: bool,
    pub detail: String,
}

fn outcome(agent: &str, r: Result<String>) -> AgentOutcome {
    match r {
        Ok(detail) => AgentOutcome {
            agent: agent.into(),
            ok: true,
            detail,
        },
        Err(e) => AgentOutcome {
            agent: agent.into(),
            ok: false,
            detail: format!("{e:#}"),
        },
    }
}

fn cli(tool: &str, args: &[&str], stdin: Option<&str>, secs: u64) -> Result<String> {
    let (code, out, err) =
        procs::run_with_timeout(tool, args, stdin, None, Duration::from_secs(secs))?;
    let text = format!("{out}\n{err}")
        .lines()
        .map(procs::clean_line)
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if code != Some(0) {
        bail!(
            "{tool} {} failed: {}",
            args.iter().take(2).copied().collect::<Vec<_>>().join(" "),
            text
        );
    }
    Ok(text)
}

fn last_line(s: &str) -> String {
    s.lines().last().unwrap_or("done").trim().to_string()
}

/// Write `doc` back to `path`, keeping CRLF if the file used it and backing it up once.
pub fn write_json_preserving(path: &Path, doc: &Value) -> Result<()> {
    let original = fs::read_to_string(path).ok();
    if let Some(orig) = &original {
        let backup = PathBuf::from(format!("{}.bak-agentos-studio", path.display()));
        if !backup.exists() {
            fs::write(&backup, orig)?;
        }
    }
    let crlf = original.as_deref().is_some_and(|t| t.contains("\r\n"));
    let mut text = serde_json::to_string_pretty(doc)? + "\n";
    if crlf {
        text = text.replace('\n', "\r\n");
    }
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("agentos-tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn json_upsert(agent: &str, spec: &ServerSpec) -> Result<String> {
    let path = agent_file(agent).ok_or_else(|| anyhow!("no config file for {agent}"))?;
    let mut doc = read_json(&path);
    if !doc.is_object() {
        doc = json!({});
    }
    if !doc.get("mcpServers").is_some_and(Value::is_object) {
        doc["mcpServers"] = json!({});
    }
    doc["mcpServers"][&spec.name] = spec.json_entry(agent);
    write_json_preserving(&path, &doc)?;
    Ok(format!("Wrote {} into {}", spec.name, path.display()))
}

fn json_remove(agent: &str, name: &str) -> Result<String> {
    let path = agent_file(agent).ok_or_else(|| anyhow!("no config file for {agent}"))?;
    let mut doc = read_json(&path);
    let removed = doc
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .and_then(|m| m.remove(name))
        .is_some();
    if !removed {
        return Ok(format!("{name} was not in {}", path.display()));
    }
    write_json_preserving(&path, &doc)?;
    Ok(format!("Removed {name} from {}", path.display()))
}

/// Only the command differs: change that one key, so Hermes keeps everything else it stores for
/// the server (timeouts, enabled state, per-tool settings) instead of a remove and re-add.
fn only_command_changes(existing: &ServerDef, spec: &ServerSpec) -> bool {
    // Hermes stores Windows paths with forward slashes; the same path with backslashes is the
    // same argument, not a change that needs a remove and re-add.
    let norm = |s: &String| s.replace('\\', "/").to_lowercase();
    existing.transport == "stdio"
        && spec.transport == "stdio"
        && existing
            .args
            .iter()
            .map(norm)
            .eq(spec.args.iter().map(norm))
        && existing.env == spec.env_vars
}

fn hermes_upsert(spec: &ServerSpec, existing: Option<&ServerDef>) -> Result<String> {
    if let Some(def) = existing {
        if only_command_changes(def, spec) {
            let key = format!("mcp_servers.{}.command", spec.name);
            cli(
                "hermes",
                &["config", "set", &key, &spec.command_or_url],
                None,
                60,
            )?;
            return Ok(format!("Pointed {} at {}", spec.name, spec.command_or_url));
        }
        cli("hermes", &["mcp", "remove", &spec.name], Some("y\n"), 60)?;
    }
    let mut args: Vec<String> = vec!["mcp".into(), "add".into(), spec.name.clone()];
    if spec.transport == "http" {
        args.push("--url".into());
        args.push(spec.command_or_url.clone());
        if let Some(auth) = spec.auth.as_deref().filter(|a| !a.is_empty()) {
            args.push("--auth".into());
            args.push(auth.into());
        }
    } else {
        args.push("--connect-timeout".into());
        args.push("60".into());
        if !spec.env_vars.is_empty() {
            args.push("--env".into());
            for (k, v) in &spec.env_vars {
                args.push(format!("{k}={v}"));
            }
        }
        args.push("--command".into());
        args.push(spec.command_or_url.clone());
        if !spec.args.is_empty() {
            args.push("--args".into());
            args.extend(spec.args.iter().cloned());
        }
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    cli("hermes", &refs, Some("y\ny\n"), 300).map(|o| last_line(&o))
}

fn claude_upsert(spec: &ServerSpec, present: bool) -> Result<String> {
    if present {
        cli(
            "claude",
            &["mcp", "remove", &spec.name, "-s", "user"],
            None,
            60,
        )?;
    }
    let mut args: Vec<String> = vec!["mcp".into(), "add".into()];
    if spec.transport == "http" {
        args.extend([
            "--transport".into(),
            "http".into(),
            "--scope".into(),
            "user".into(),
        ]);
        args.push(spec.name.clone());
        args.push(spec.command_or_url.clone());
    } else {
        args.push(spec.name.clone());
        args.extend(["--scope".into(), "user".into()]);
        for (k, v) in &spec.env_vars {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        args.push("--".into());
        args.push(spec.command_or_url.clone());
        args.extend(spec.args.iter().cloned());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    cli("claude", &refs, None, 120).map(|o| last_line(&o))
}

pub fn add_mcp_server(spec: &ServerSpec, targets: &[String]) -> Result<Vec<AgentOutcome>> {
    spec.validate()?;
    let sources = all_sources();
    let present = |agent: &str| {
        sources
            .get(agent)
            .is_some_and(|m| m.contains_key(&spec.name))
    };
    let mut results = Vec::new();
    for agent in AGENTS.iter().filter(|a| targets.iter().any(|t| t == *a)) {
        let r = match *agent {
            "hermes" => hermes_upsert(spec, sources.get("hermes").and_then(|m| m.get(&spec.name))),
            "claude-code" => claude_upsert(spec, present("claude-code")),
            other => json_upsert(other, spec),
        };
        results.push(outcome(agent, r));
    }
    if results.is_empty() {
        bail!("Pick at least one agent");
    }
    Ok(results)
}

pub fn remove_mcp_server(name: &str) -> Result<Vec<AgentOutcome>> {
    let sources = all_sources();
    let mut results = Vec::new();
    for agent in AGENTS {
        if !sources.get(agent).is_some_and(|m| m.contains_key(name)) {
            continue;
        }
        let r = match agent {
            "hermes" => {
                cli("hermes", &["mcp", "remove", name], Some("y\n"), 60).map(|o| last_line(&o))
            }
            "claude-code" => cli("claude", &["mcp", "remove", name, "-s", "user"], None, 60)
                .map(|o| last_line(&o)),
            other => json_remove(other, name),
        };
        results.push(outcome(agent, r));
    }
    if results.is_empty() {
        bail!("No agent has a server named '{name}'");
    }
    Ok(results)
}

/// Turn a server on or off for Hermes without removing it.
pub fn set_mcp_enabled(name: &str, enabled: bool) -> Result<String> {
    let cfg = crate::hermes_engine::read_config()?;
    if !hermes_servers(&cfg).contains_key(name) {
        bail!("Hermes has no MCP server named '{name}'");
    }
    let key = format!("mcp_servers.{name}.enabled");
    cli(
        "hermes",
        &[
            "config",
            "set",
            &key,
            if enabled { "true" } else { "false" },
        ],
        None,
        60,
    )?;
    Ok(format!(
        "{name} {} for Hermes",
        if enabled { "enabled" } else { "disabled" }
    ))
}

/// Copy a server's primary definition to agents that do not have it yet.
pub fn sync_server_to_agents(name: &str, targets: &[String]) -> Result<Vec<AgentOutcome>> {
    let def = find_def(name)?;
    let spec = ServerSpec {
        name: name.into(),
        transport: def.transport.clone(),
        command_or_url: def.url.clone().or(def.command.clone()).unwrap_or_default(),
        args: def.args.clone(),
        env_vars: def.env.clone(),
        auth: def.auth.clone(),
    };
    add_mcp_server(&spec, targets)
}

/// Point `agentos-hub` in every agent at this version's staged hub binary
/// (`--mcp-stdio --vault <vault>`). Sessions already running keep the binary they started with.
pub fn cutover_agentos_hub(vault: &Path) -> Result<Vec<AgentOutcome>> {
    let exe = stage_hub_binary()?.display().to_string();
    let sources = all_sources();
    let mut out = Vec::new();
    for agent in AGENTS {
        // Keep each agent's own environment (its hub identity token among it).
        let mut spec = agentos_hub_spec(&exe, vault);
        if let Some(d) = sources.get(agent).and_then(|m| m.get("agentos-hub")) {
            spec.env_vars = d.env.clone();
        }
        out.extend(add_mcp_server(&spec, &[agent.to_string()])?);
    }
    // A new registration has no token yet: give it one now, not at the next start.
    out.extend(provision_hub_tokens());
    Ok(out)
}

/// Give every agent that has an agentos-hub entry its own identity token, unless its entry already
/// carries a valid one. The token is written into the entry (Hermes through `hermes config set`,
/// Claude Code through `claude mcp`, Qoder and Antigravity in their JSON) and only its hash is
/// kept. Outcomes never contain the token.
pub fn provision_hub_tokens() -> Vec<AgentOutcome> {
    use crate::hub_identity as id;
    let sources = all_sources();
    let mut out = Vec::new();
    for agent in id::AGENTS {
        let Some(def) = sources.get(agent).and_then(|m| m.get("agentos-hub")) else {
            continue;
        };
        let valid = def
            .env
            .get(id::ENV)
            .and_then(|t| id::agent_for(t))
            .is_some_and(|a| a == agent);
        if valid {
            continue;
        }
        let token = id::generate();
        let r = set_hub_token(agent, def, &token).and_then(|_| id::store(agent, &token));
        out.push(outcome(
            agent,
            r.map(|_| format!("{agent} has its own agentos-hub identity")),
        ));
    }
    out
}

fn set_hub_token(agent: &str, def: &ServerDef, token: &str) -> Result<()> {
    let key = crate::hub_identity::ENV;
    match agent {
        "hermes" => {
            let path = format!("mcp_servers.agentos-hub.env.{key}");
            // Hermes masks the value in its own output; never echo ours.
            cli("hermes", &["config", "set", &path, token], None, 60)
                .map(|_| ())
                .map_err(|_| anyhow!("hermes config set for the agentos-hub identity failed"))
        }
        "claude-code" => {
            let mut env = def.env.clone();
            env.insert(key.into(), token.into());
            let spec = ServerSpec {
                name: "agentos-hub".into(),
                transport: "stdio".into(),
                command_or_url: def.command.clone().unwrap_or_default(),
                args: def.args.clone(),
                env_vars: env,
                auth: None,
            };
            claude_upsert(&spec, true)
                .map(|_| ())
                .map_err(|_| anyhow!("claude mcp add for the agentos-hub identity failed"))
        }
        other => {
            let path = agent_file(other).ok_or_else(|| anyhow!("no config file for {other}"))?;
            let mut doc = read_json(&path);
            let entry = doc
                .get_mut("mcpServers")
                .and_then(|m| m.get_mut("agentos-hub"))
                .filter(|e| e.is_object())
                .ok_or_else(|| anyhow!("{other} has no agentos-hub entry"))?;
            if !entry.get("env").is_some_and(Value::is_object) {
                entry["env"] = json!({});
            }
            entry["env"][key] = json!(token);
            write_json_preserving(&path, &doc)
        }
    }
}

/// Agents whose agentos-hub is a Studio-staged hub (under `root`) but not `want`: what an app
/// update leaves behind. Agents that point anywhere else (a dev build, a hand-written path) or do
/// not have the hub are left alone.
pub fn managed_outdated_agents(
    sources: &BTreeMap<String, BTreeMap<String, ServerDef>>,
    root: &Path,
    want: &Path,
) -> Vec<String> {
    let norm = |s: &str| s.replace('\\', "/").to_lowercase();
    let root = norm(&root.display().to_string());
    let want = norm(&want.display().to_string());
    AGENTS
        .iter()
        .filter(|a| {
            sources
                .get(**a)
                .and_then(|m| m.get("agentos-hub"))
                .and_then(|d| d.command.as_deref())
                .map(norm)
                .is_some_and(|c| c != want && c.starts_with(&format!("{root}/")))
        })
        .map(|a| a.to_string())
        .collect()
}

/// After an app update, move agents from the previous staged hub to this version's. Returns the
/// agents moved (empty when nothing was outdated). Running sessions keep their old binary, which
/// is never deleted, so this is safe while agents are open.
pub fn auto_update_hub(vault: &Path) -> Result<Vec<AgentOutcome>> {
    let root = hub_root();
    let want = hub_binary_for(&root, env!("CARGO_PKG_VERSION"));
    let sources = all_sources();
    let targets = managed_outdated_agents(&sources, &root, &want);
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    let exe = stage_hub_binary()?.display().to_string();
    let mut out = Vec::new();
    for agent in &targets {
        // Only the binary changes: keep each agent's own arguments (its vault, its flags).
        let mut spec = agentos_hub_spec(&exe, vault);
        if let Some(d) = sources.get(agent).and_then(|m| m.get("agentos-hub")) {
            if d.args.iter().any(|a| a == "--mcp-stdio") {
                spec.args = d.args.clone();
            }
            spec.env_vars = d.env.clone();
        }
        out.extend(add_mcp_server(&spec, std::slice::from_ref(agent))?);
    }
    Ok(out)
}

pub fn agentos_hub_spec(exe: &str, vault: &Path) -> ServerSpec {
    ServerSpec {
        name: "agentos-hub".into(),
        transport: "stdio".into(),
        command_or_url: exe.into(),
        args: vec![
            "--mcp-stdio".into(),
            "--vault".into(),
            vault.display().to_string(),
        ],
        env_vars: BTreeMap::new(),
        auth: None,
    }
}

pub fn trigger_mcp_oauth(name: &str) -> Result<String> {
    procs::spawn_console("hermes", &["mcp", "login", name])?;
    Ok(format!("Opened the sign-in window for {name}"))
}

// ---------------------------------------------------------------------------------------------
// Testing
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProbeResult {
    pub name: String,
    pub ok: bool,
    pub via: String,
    pub latency_ms: u128,
    pub server_name: String,
    pub server_version: String,
    pub protocol: String,
    pub tools: Vec<ToolSchema>,
    pub error: Option<String>,
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            procs::kill_tree(self.0.id());
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn resolve_program(command: &str) -> Result<PathBuf> {
    let p = PathBuf::from(command);
    if p.is_file() {
        return Ok(p);
    }
    procs::find(command).ok_or_else(|| anyhow!("'{command}' was not found on PATH"))
}

fn wait_for(
    rx: &mpsc::Receiver<String>,
    id: u64,
    deadline: Instant,
    stderr_tail: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> Result<Value> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("timed out waiting for the server{}", tail_note(stderr_tail));
        }
        match rx.recv_timeout(left) {
            Ok(line) => {
                let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                if v.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(e) = v.get("error") {
                    bail!(
                        "server error: {}",
                        e.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                    );
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("server exited before answering{}", tail_note(stderr_tail))
            }
        }
    }
}

fn tail_note(t: &std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> String {
    let lines = t.lock().map(|g| g.clone()).unwrap_or_default();
    if lines.is_empty() {
        String::new()
    } else {
        format!(": {}", lines.join(" | "))
    }
}

/// Speak MCP over stdio: initialize, notifications/initialized, then every tools/list page.
pub fn probe_stdio(name: &str, def: &ServerDef, limit: Duration) -> Result<ProbeResult> {
    let started = Instant::now();
    let program = resolve_program(def.command.as_deref().unwrap_or_default())?;
    let mut cmd = procs::command(&program);
    cmd.args(&def.args)
        .envs(&def.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut guard = ChildGuard(
        cmd.spawn()
            .with_context(|| format!("starting {}", program.display()))?,
    );
    let mut stdin = guard.0.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
    let stdout = guard.0.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let stderr = guard.0.stderr.take();
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let tail = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    if let Some(e) = stderr {
        let tail = tail.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(e).lines().map_while(Result::ok) {
                if let Ok(mut t) = tail.lock() {
                    t.push(procs::clean_line(&line));
                    let n = t.len();
                    if n > 4 {
                        t.drain(..n - 4);
                    }
                }
            }
        });
    }
    let deadline = started + limit;
    let mut send = |v: Value| -> Result<()> {
        writeln!(stdin, "{v}")?;
        stdin.flush()?;
        Ok(())
    };
    send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "agentos-studio", "version": env!("CARGO_PKG_VERSION")}
        }}),
    )?;
    let init = wait_for(&rx, 1, deadline, &tail)?;
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..20u64 {
        let id = 2 + page;
        let params = match &cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({}),
        };
        send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": params}))?;
        let res = wait_for(&rx, id, deadline, &tail)?;
        tools.extend(parse_tools(&res));
        cursor = res
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    let latency_ms = started.elapsed().as_millis();
    drop(guard);
    Ok(ProbeResult {
        name: name.into(),
        ok: true,
        via: "native".into(),
        latency_ms,
        server_name: init
            .pointer("/serverInfo/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        server_version: init
            .pointer("/serverInfo/version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        protocol: init
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        tools,
        error: None,
    })
}

pub fn parse_tools(res: &Value) -> Vec<ToolSchema> {
    res.get("tools")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|t| ToolSchema {
                    name: t
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .into(),
                    description: t
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    input_schema: t.get("inputSchema").cloned().unwrap_or(Value::Null),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `hermes mcp test <name>` output (used for HTTP and OAuth servers).
pub fn parse_hermes_test(name: &str, text: &str) -> ProbeResult {
    let lat = regex::Regex::new(r"Connected \((\d+)ms\)").expect("static regex");
    let latency_ms = lat
        .captures(text)
        .and_then(|c| c[1].parse::<u128>().ok())
        .unwrap_or(0);
    let mut tools = Vec::new();
    let mut in_tools = false;
    for line in text.lines() {
        if line.contains("Tools discovered") {
            in_tools = true;
            continue;
        }
        if in_tools {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            let mut parts = t.splitn(2, char::is_whitespace);
            let tool = parts.next().unwrap_or_default();
            if tool
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
            {
                tools.push(ToolSchema {
                    name: tool.into(),
                    description: parts.next().unwrap_or_default().trim().into(),
                    input_schema: Value::Null,
                });
            }
        }
    }
    let ok = text.contains("Connected");
    ProbeResult {
        name: name.into(),
        ok,
        via: "hermes".into(),
        latency_ms,
        server_name: name.into(),
        server_version: String::new(),
        protocol: String::new(),
        tools,
        error: (!ok).then(|| {
            text.lines()
                .rfind(|l| !l.trim().is_empty())
                .unwrap_or("connection failed")
                .trim()
                .to_string()
        }),
    }
}

pub fn test_mcp_server(name: &str) -> Result<ProbeResult> {
    let def = find_def(name)?;
    if def.transport == "stdio" {
        return Ok(match probe_stdio(name, &def, Duration::from_secs(60)) {
            Ok(r) => r,
            Err(e) => ProbeResult {
                name: name.into(),
                ok: false,
                via: "native".into(),
                latency_ms: 0,
                server_name: String::new(),
                server_version: String::new(),
                protocol: String::new(),
                tools: Vec::new(),
                error: Some(format!("{e:#}")),
            },
        });
    }
    let (_, out, err) = procs::run_with_timeout(
        "hermes",
        &["mcp", "test", name],
        None,
        None,
        Duration::from_secs(120),
    )?;
    let text = format!("{out}\n{err}")
        .lines()
        .map(procs::clean_line)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(parse_hermes_test(name, &text))
}

// ---------------------------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct CatalogParam {
    pub key: &'static str,
    pub label: &'static str,
    pub placeholder: &'static str,
    pub default: String,
    pub secret: bool,
    /// true: becomes an environment variable; false: substituted into args as `{key}`.
    pub env: bool,
    pub required: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct CatalogEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub transport: &'static str,
    pub command_or_url: String,
    pub args: Vec<String>,
    pub auth: Option<&'static str>,
    pub params: Vec<CatalogParam>,
    pub requires: &'static str,
}

fn param(
    key: &'static str,
    label: &'static str,
    placeholder: &'static str,
    default: String,
    secret: bool,
    env: bool,
) -> CatalogParam {
    CatalogParam {
        key,
        label,
        placeholder,
        default,
        secret,
        env,
        required: true,
    }
}

pub fn catalog(vault: &Path) -> Vec<CatalogEntry> {
    let home = crate::settings::home().display().to_string();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    vec![
        CatalogEntry {
            id: "agentos-hub",
            name: "AgentOS-Hub (Rust)",
            description:
                "The native Rust Agent Bus, typed shared memory, ranked recall, agent identity, the agent registry, handoffs and asks: 13 tools over your Obsidian vault.",
            transport: "stdio",
            command_or_url: exe,
            args: vec![
                "--mcp-stdio".into(),
                "--vault".into(),
                vault.display().to_string(),
            ],
            auth: None,
            params: vec![],
            requires: "Nothing: it is this app",
        },
        CatalogEntry {
            id: "notion",
            name: "Notion",
            description: "Search, read and edit your Notion workspace through Notion's hosted MCP.",
            transport: "http",
            command_or_url: "https://mcp.notion.com/mcp".into(),
            args: vec![],
            auth: Some("oauth"),
            params: vec![],
            requires: "A Notion account (sign-in window opens)",
        },
        CatalogEntry {
            id: "github",
            name: "GitHub",
            description: "Repositories, issues, pull requests and code search.",
            transport: "stdio",
            command_or_url: "npx".into(),
            args: s(&["-y", "@modelcontextprotocol/server-github"]),
            auth: None,
            params: vec![param(
                "GITHUB_PERSONAL_ACCESS_TOKEN",
                "GitHub token",
                "ghp_... (Use gh login fills this)",
                String::new(),
                true,
                true,
            )],
            requires: "Node.js and a GitHub token",
        },
        CatalogEntry {
            id: "filesystem",
            name: "Filesystem",
            description: "Read and write files inside one folder you choose.",
            transport: "stdio",
            command_or_url: "npx".into(),
            args: s(&["-y", "@modelcontextprotocol/server-filesystem", "{path}"]),
            auth: None,
            params: vec![param(
                "path",
                "Allowed folder",
                "C:\\Users\\you\\projects",
                home,
                false,
                false,
            )],
            requires: "Node.js",
        },
        CatalogEntry {
            id: "brave-search",
            name: "Brave Web Search",
            description: "Web and local search through the Brave Search API.",
            transport: "stdio",
            command_or_url: "npx".into(),
            args: s(&["-y", "@modelcontextprotocol/server-brave-search"]),
            auth: None,
            params: vec![param(
                "BRAVE_API_KEY",
                "Brave Search API key",
                "BSA...",
                String::new(),
                true,
                true,
            )],
            requires: "Node.js and a Brave Search API key",
        },
        CatalogEntry {
            id: "postgres",
            name: "Postgres",
            description: "Read-only SQL and schema inspection for a PostgreSQL database.",
            transport: "stdio",
            command_or_url: "npx".into(),
            args: s(&[
                "-y",
                "@modelcontextprotocol/server-postgres",
                "{connection}",
            ]),
            auth: None,
            params: vec![param(
                "connection",
                "Connection string",
                "postgresql://user:pass@localhost:5432/db",
                String::new(),
                true,
                false,
            )],
            requires: "Node.js and a reachable database",
        },
        CatalogEntry {
            id: "sqlite",
            name: "SQLite",
            description: "Query and change a local SQLite database file.",
            transport: "stdio",
            command_or_url: "uvx".into(),
            args: s(&["mcp-server-sqlite", "--db-path", "{db}"]),
            auth: None,
            params: vec![param(
                "db",
                "Database file",
                "C:\\data\\app.db",
                String::new(),
                false,
                false,
            )],
            requires: "uv (uvx)",
        },
        CatalogEntry {
            id: "playwright",
            name: "Playwright Browser",
            description: "Drive a real browser: navigate, click, fill forms and read pages.",
            transport: "stdio",
            command_or_url: "npx".into(),
            args: s(&["-y", "@playwright/mcp@latest"]),
            auth: None,
            params: vec![],
            requires: "Node.js",
        },
    ]
}

/// Turn a catalog entry plus user-supplied values into a concrete server spec.
pub fn spec_from_catalog(
    entry: &CatalogEntry,
    values: &BTreeMap<String, String>,
) -> Result<ServerSpec> {
    let mut env_vars = BTreeMap::new();
    let mut args = entry.args.clone();
    for p in &entry.params {
        let v = values
            .get(p.key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| p.default.clone());
        if v.is_empty() && p.required {
            bail!("{} needs '{}'", entry.name, p.label);
        }
        if p.env {
            env_vars.insert(p.key.to_string(), v);
        } else {
            let token = format!("{{{}}}", p.key);
            for a in args.iter_mut() {
                *a = a.replace(&token, &v);
            }
        }
    }
    Ok(ServerSpec {
        name: entry.id.to_string(),
        transport: entry.transport.to_string(),
        command_or_url: entry.command_or_url.clone(),
        args,
        env_vars,
        auth: entry.auth.map(String::from),
    })
}

pub fn gh_token() -> Result<String> {
    let out = procs::run("gh", &["auth", "token"])
        .ok_or_else(|| anyhow!("GitHub CLI is not installed"))?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || token.is_empty() {
        bail!("Run gh auth login first (Setup > GitHub CLI)");
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_studio_staged_outdated_hubs_are_moved() {
        let def = |c: &str| ServerDef {
            transport: "stdio".into(),
            command: Some(c.into()),
            args: vec!["--mcp-stdio".into()],
            ..Default::default()
        };
        let mut src: BTreeMap<String, BTreeMap<String, ServerDef>> = BTreeMap::new();
        let mut put = |a: &str, c: &str| {
            src.entry(a.into())
                .or_default()
                .insert("agentos-hub".into(), def(c));
        };
        put(
            "hermes",
            "C:\\Users\\u\\AppData\\Local\\AgentOS Hub\\0.9.2\\agentos-studio.exe",
        );
        put(
            "claude-code",
            "c:/users/u/appdata/local/agentos hub/0.9.3/agentos-studio.exe",
        );
        put(
            "qoder",
            "C:/dev/agentos-studio/target/release/agentos-studio.exe",
        );
        let root = Path::new("C:/Users/u/AppData/Local/AgentOS Hub");
        let want = hub_binary_for(root, "0.9.3");
        assert_eq!(
            managed_outdated_agents(&src, root, &want),
            vec!["hermes".to_string()]
        );
    }

    #[test]
    fn hub_binary_is_staged_per_version_and_reused() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("build.exe");
        fs::write(&src, b"hub v1").unwrap();
        let root = d.path().join("AgentOS Hub");
        let a = stage_hub_binary_at(&src, &root, "0.9.1").unwrap();
        assert_eq!(a, root.join("0.9.1").join("agentos-studio.exe"));
        assert_eq!(fs::read(&a).unwrap(), b"hub v1");
        assert!(!a.with_file_name("agentos-studio.exe.partial").exists());
        // Identical bytes: reused, not rewritten.
        let before = fs::metadata(&a).unwrap().modified().unwrap();
        assert_eq!(stage_hub_binary_at(&src, &root, "0.9.1").unwrap(), a);
        assert_eq!(fs::metadata(&a).unwrap().modified().unwrap(), before);
        // A new version gets its own folder and leaves the old one for running sessions.
        fs::write(&src, b"hub v2").unwrap();
        let b = stage_hub_binary_at(&src, &root, "0.9.2").unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::read(&a).unwrap(), b"hub v1");
        assert_eq!(fs::read(&b).unwrap(), b"hub v2");
        // Same version rebuilt with different bytes replaces an unlocked copy.
        assert_eq!(
            fs::read(stage_hub_binary_at(&src, &root, "0.9.1").unwrap()).unwrap(),
            b"hub v2"
        );
    }

    #[test]
    fn hermes_hub_update_changes_only_the_command() {
        let spec = agentos_hub_spec("C:/new/agentos-studio.exe", Path::new("C:/vault"));
        let existing = ServerDef {
            transport: "stdio".into(),
            command: Some("C:/old/agentos-studio.exe".into()),
            args: spec.args.clone(),
            enabled: true,
            ..Default::default()
        };
        assert!(only_command_changes(&existing, &spec));
        let slashes = ServerDef {
            args: vec!["--mcp-stdio".into(), "--vault".into(), "C:\\vault".into()],
            ..existing.clone()
        };
        assert!(
            only_command_changes(&slashes, &spec),
            "same path, other slash style"
        );
        let other_vault = ServerDef {
            args: vec!["--mcp-stdio".into(), "--vault".into(), "D:/other".into()],
            ..existing.clone()
        };
        assert!(!only_command_changes(&other_vault, &spec));
        let python = ServerDef {
            args: vec!["agent_bus_server.py".into()],
            ..existing
        };
        assert!(!only_command_changes(&python, &spec));
    }

    #[test]
    fn parses_hermes_yaml_servers() {
        let cfg: Yaml = serde_yaml::from_str(
            "mcp_servers:\n  notion:\n    url: https://mcp.notion.com/mcp\n    auth: oauth\n    enabled: true\n  agentos-hub:\n    command: C:/x/agentos-studio.exe\n    args:\n      - --mcp-stdio\n      - --vault\n      - C:/v\n    env:\n      AGENTOS_VAULT: C:/v\n  off:\n    command: npx\n    enabled: false\n",
        )
        .unwrap();
        let s = hermes_servers(&cfg);
        assert_eq!(s.len(), 3);
        assert_eq!(s["notion"].transport, "http");
        assert_eq!(s["notion"].auth.as_deref(), Some("oauth"));
        assert_eq!(
            s["agentos-hub"].args,
            vec!["--mcp-stdio", "--vault", "C:/v"]
        );
        assert_eq!(s["agentos-hub"].env["AGENTOS_VAULT"], "C:/v");
        assert!(!s["off"].enabled);
        assert!(hermes_servers(&serde_yaml::from_str("model: x\n").unwrap()).is_empty());
    }

    #[test]
    fn parses_json_servers_from_every_agent_shape() {
        let doc = json!({"mcpServers": {
            "hub": {"type": "stdio", "command": "C:\\x\\agentos-studio.exe", "args": ["--mcp-stdio"], "env": {"A": "1"}},
            "remote": {"serverUrl": "https://example.com/mcp"},
            "claude-http": {"type": "http", "url": "https://h/mcp"},
            "off": {"command": "npx", "disabled": true}
        }});
        let s = json_servers(&doc);
        assert_eq!(s["hub"].transport, "stdio");
        assert_eq!(s["remote"].url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(s["claude-http"].transport, "http");
        assert!(!s["off"].enabled);
        assert!(json_servers(&json!({})).is_empty());
    }

    #[test]
    fn merges_registry_with_agent_coverage_and_legacy_flag() {
        let hub = |cmd: &str, args: &[&str]| ServerDef {
            transport: "stdio".into(),
            command: Some(cmd.into()),
            args: args.iter().map(|s| s.to_string()).collect(),
            enabled: true,
            ..Default::default()
        };
        let mut sources = BTreeMap::new();
        sources.insert(
            "hermes".to_string(),
            BTreeMap::from([(
                "agentos-hub".to_string(),
                hub("C:/a/agentos-studio.exe", &["--mcp-stdio"]),
            )]),
        );
        sources.insert(
            "claude-code".to_string(),
            BTreeMap::from([(
                "agentos-hub".to_string(),
                hub("C:\\A\\agentos-studio.exe", &["--mcp-stdio"]),
            )]),
        );
        sources.insert(
            "qoder".to_string(),
            BTreeMap::from([(
                "agentos-hub".to_string(),
                hub("python.exe", &["C:/v/agent_bus_server.py"]),
            )]),
        );
        let reg = merge_registry(&sources);
        assert_eq!(reg.len(), 1);
        let r = &reg[0];
        assert!(r.is_agentos_hub);
        assert!(r.legacy_python, "Qoder still points at the Python server");
        let by: BTreeMap<_, _> = r
            .agents
            .iter()
            .map(|a| (a.agent.as_str(), a.matches_primary))
            .collect();
        assert!(by["hermes"]);
        assert!(
            by["claude-code"],
            "path comparison ignores slashes and case"
        );
        assert!(!by["qoder"]);
        assert!(!by.contains_key("antigravity"));
    }

    #[test]
    fn validates_specs_and_builds_agent_entries() {
        let mut spec = ServerSpec {
            name: "files".into(),
            transport: "stdio".into(),
            command_or_url: "npx".into(),
            args: vec!["-y".into(), "pkg".into()],
            env_vars: BTreeMap::from([("TOKEN".to_string(), "t".to_string())]),
            auth: None,
        };
        spec.validate().unwrap();
        assert_eq!(
            spec.json_entry("qoder"),
            json!({"command": "npx", "args": ["-y", "pkg"], "env": {"TOKEN": "t"}})
        );
        spec.name = "bad name".into();
        assert!(spec.validate().is_err());
        let http = ServerSpec {
            name: "n".into(),
            transport: "http".into(),
            command_or_url: "https://x/mcp".into(),
            ..Default::default()
        };
        http.validate().unwrap();
        assert_eq!(
            http.json_entry("antigravity"),
            json!({"serverUrl": "https://x/mcp"})
        );
        assert_eq!(http.json_entry("qoder"), json!({"url": "https://x/mcp"}));
        let bad = ServerSpec {
            transport: "http".into(),
            command_or_url: "npx".into(),
            name: "n".into(),
            ..Default::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn catalog_specs_substitute_params() {
        let v = PathBuf::from("C:/vault");
        let cat = catalog(&v);
        let ids: Vec<_> = cat.iter().map(|c| c.id).collect();
        for want in [
            "agentos-hub",
            "notion",
            "github",
            "filesystem",
            "brave-search",
            "postgres",
            "sqlite",
            "playwright",
        ] {
            assert!(ids.contains(&want), "{want}");
        }
        let fs_entry = cat.iter().find(|c| c.id == "filesystem").unwrap();
        let spec = spec_from_catalog(
            fs_entry,
            &BTreeMap::from([("path".to_string(), "D:/code".to_string())]),
        )
        .unwrap();
        assert_eq!(spec.args.last().unwrap(), "D:/code");
        let gh = cat.iter().find(|c| c.id == "github").unwrap();
        assert!(spec_from_catalog(gh, &BTreeMap::new()).is_err());
        let gh_spec = spec_from_catalog(
            gh,
            &BTreeMap::from([("GITHUB_PERSONAL_ACCESS_TOKEN".to_string(), "x".to_string())]),
        )
        .unwrap();
        assert_eq!(gh_spec.env_vars["GITHUB_PERSONAL_ACCESS_TOKEN"], "x");
        let hub = cat.iter().find(|c| c.id == "agentos-hub").unwrap();
        assert_eq!(
            spec_from_catalog(hub, &BTreeMap::new()).unwrap().args[2],
            "C:/vault"
        );
    }

    #[test]
    fn parses_hermes_mcp_test_output() {
        let text = "Testing 'agentos-hub'...\n  Transport: stdio\n  Auth: none\n  \u{2713} Connected (2686ms)\n  \u{2713} Tools discovered: 2\n\n    delegate_task                        Create a new task on the Agent Bus\n    fetch_pending_tasks                  List open tasks\n";
        let r = parse_hermes_test("agentos-hub", text);
        assert!(r.ok);
        assert_eq!(r.latency_ms, 2686);
        assert_eq!(
            r.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["delegate_task", "fetch_pending_tasks"]
        );
        let bad = parse_hermes_test(
            "x",
            "Testing 'x'...\n  \u{2717} Connection failed: refused\n",
        );
        assert!(!bad.ok);
        assert!(bad.error.unwrap().contains("refused"));
    }

    #[test]
    fn writes_json_preserving_crlf_with_backup() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(&p, "{\r\n  \"keep\": 1\r\n}\r\n").unwrap();
        write_json_preserving(
            &p,
            &json!({"keep": 1, "mcpServers": {"x": {"command": "y"}}}),
        )
        .unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.contains("\r\n") && !text.replace("\r\n", "").contains('\n'));
        assert!(d.path().join("settings.json.bak-agentos-studio").exists());
    }
}
