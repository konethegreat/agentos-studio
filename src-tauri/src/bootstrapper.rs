//! 1-click bootstrapper: detects every piece of the stack and installs, repairs,
//! authenticates or registers it, streaming progress to the UI.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::procs;
use crate::vault::Vault;

pub const EV_SETUP: &str = "agentos://setup-progress";
pub const PLUGIN: &str = "claude-subscription-directsdk";
/// Antigravity is reached through a community Hermes plugin; it is not in the curated catalog.
pub const ANTIGRAVITY_PLUGIN_URL: &str = "https://github.com/neerazz/hermes-antigravity-oauth";

#[derive(Debug, Clone, Serialize)]
pub struct Component {
    pub id: String,
    pub label: String,
    pub pillar: u8,
    pub ok: bool,
    pub detail: String,
    pub action: String,
    /// Not needed to use the app (Notion, an IDE the user does not have). An optional item that
    /// is not set up never counts against "Healthy".
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pillar {
    pub number: u8,
    pub title: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StackStatus {
    pub components: Vec<Component>,
    pub pillars: Vec<Pillar>,
    pub connected: usize,
    pub total: usize,
    pub vault: String,
    pub studio_binary: String,
    pub checked_at: String,
}

pub const PILLARS: [(u8, &str); 7] = [
    (0, "Foundations"),
    (1, "Claude Subscription Bridge"),
    (2, "Hermes Agent Engine"),
    (3, "Obsidian Shared Brain"),
    (4, "Notion Cloud Sync"),
    (5, "AgentOS Rust MCP Bus"),
    (6, "IDE Fleet Connectors"),
];

fn comp(
    id: &str,
    label: &str,
    pillar: u8,
    ok: bool,
    detail: impl Into<String>,
    action: &str,
) -> Component {
    Component {
        id: id.into(),
        label: label.into(),
        pillar,
        ok,
        detail: detail.into(),
        action: action.into(),
        optional: false,
    }
}

/// Tools the native hub must list in its self-test; kept in step with `mcp_stdio::tool_definitions`.
pub const HUB_TOOLS: usize = 13;

fn optional(mut c: Component) -> Component {
    c.optional = true;
    c
}

/// An agent that is not installed is optional, so a first run with only Hermes never shows a red
/// pillar for Claude, Qoder or Antigravity. The component stays on screen with its install action.
pub fn optional_when_absent(mut c: Component, present: bool) -> Component {
    if !present {
        c.optional = true;
        c.detail = format!("{} (optional)", c.detail);
    }
    c
}

/// Claude Code is installed when its CLI resolves or when a config from another install exists.
fn claude_present() -> bool {
    procs::find("claude").is_some() || procs::claude_json().exists()
}

/// An IDE agent counts as present when its config folder exists; Studio only wires agents the
/// user actually has.
pub fn agent_present(agent: &str) -> bool {
    let h = crate::settings::home();
    match agent {
        "qoder" => h.join(".qoder").is_dir(),
        "antigravity" => {
            h.join(".gemini").join("antigravity").is_dir() || procs::find("antigravity").is_some()
        }
        _ => true,
    }
}

fn tool_comp(id: &str, label: &str, pillar: u8, tool: &str, action: &str) -> Component {
    match procs::version(tool) {
        Some(v) => comp(id, label, pillar, true, v, "Reinstall"),
        None => comp(
            id,
            label,
            pillar,
            false,
            format!("{tool} not found on PATH"),
            action,
        ),
    }
}

fn read_yaml(path: &Path) -> serde_yaml::Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_yaml::from_str(&t).ok())
        .unwrap_or(serde_yaml::Value::Null)
}

fn read_json(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

fn studio_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("agentos-studio"))
}

/// Describe where an MCP registration points (this binary, the Python server, or elsewhere).
fn describe_target(command: &str, args: &[String]) -> String {
    let exe = studio_exe();
    let same =
        Path::new(command) == exe || fs::canonicalize(command).ok() == fs::canonicalize(&exe).ok();
    if same {
        "native Rust bus (this AgentOS Studio binary)".into()
    } else if args.iter().any(|a| a.ends_with("agent_bus_server.py")) {
        "Python agent_bus_server.py".into()
    } else if args.iter().any(|a| a == "--mcp-stdio") {
        format!("Rust bus at {command}")
    } else {
        command.to_string()
    }
}

fn json_registration(v: &Value) -> Option<String> {
    let e = v.get("mcpServers")?.get("agentos-hub")?;
    if e.get("disabled").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let cmd = e.get("command")?.as_str()?.to_string();
    let args: Vec<String> = e
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(describe_target(&cmd, &args))
}

fn path_missing() -> Vec<PathBuf> {
    let persisted: Vec<PathBuf> = if cfg!(windows) {
        procs::user_path_entries()
            .into_iter()
            .map(PathBuf::from)
            .collect()
    } else {
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default()
    };
    let machine: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let h = crate::settings::home();
    let mut wanted = vec![h.join(".local").join("bin"), h.join(".cargo").join("bin")];
    if cfg!(windows) {
        if let Some(a) = dirs::data_dir() {
            wanted.push(a.join("npm"));
        }
    }
    wanted
        .into_iter()
        .filter(|d| d.is_dir())
        .filter(|d| !persisted.iter().chain(machine.iter()).any(|p| p == d))
        .collect()
}

/// Send `initialize` + `tools/list` to this binary in `--mcp-stdio` mode and count tools.
pub fn bus_selftest(vault: &Path) -> Result<usize> {
    let mut cmd = Command::new(studio_exe());
    cmd.args(["--mcp-stdio", "--vault"])
        .arg(vault)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(procs::CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().context("starting --mcp-stdio")?;
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| anyhow!("no stdin"))?;
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"agentos-studio-selftest","version":"1"}}}}}}"#
        )?;
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )?;
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#)?;
    }
    drop(child.stdin.take());
    let out = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let list: Value = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["id"] == 2)
        .ok_or_else(|| anyhow!("no tools/list reply"))?;
    Ok(list["result"]["tools"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0))
}

pub fn check_full_stack_status(vault: &Vault) -> StackStatus {
    let hermes_cfg = read_yaml(&procs::hermes_config());
    let servers = hermes_cfg
        .get("mcp_servers")
        .cloned()
        .unwrap_or(serde_yaml::Value::Null);
    let vault_path = vault.root.clone();

    let handles: Vec<std::thread::JoinHandle<Vec<Component>>> = vec![
        std::thread::spawn(|| {
            vec![
                tool_comp("git", "Git", 0, "git", "Install"),
                tool_comp("node", "Node.js + npm", 0, "node", "Install"),
            ]
        }),
        std::thread::spawn(|| {
            let mut c = tool_comp("gh", "GitHub CLI", 0, "gh", "Install");
            if c.ok {
                let authed = procs::run("gh", &["auth", "status"])
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if !authed {
                    c.ok = false;
                    c.detail = format!("{} - not signed in", c.detail);
                    c.action = "Sign in".into();
                }
            }
            vec![c]
        }),
        std::thread::spawn(|| {
            let missing = path_missing();
            vec![if missing.is_empty() {
                comp(
                    "path",
                    "User PATH",
                    0,
                    true,
                    "~/.local/bin, npm and Cargo bins are on PATH",
                    "Re-check",
                )
            } else {
                comp(
                    "path",
                    "User PATH",
                    0,
                    false,
                    format!(
                        "Missing: {}",
                        missing
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                    "Fix PATH",
                )
            }]
        }),
        std::thread::spawn(|| {
            let cli = tool_comp("claude-cli", "Claude Code CLI", 1, "claude", "Install");
            let auth = if cli.ok {
                let out = procs::run("claude", &["auth", "status"]);
                let v: Value = out
                    .as_ref()
                    .and_then(|o| serde_json::from_slice(&o.stdout).ok())
                    .unwrap_or(Value::Null);
                if v["loggedIn"].as_bool() == Some(true) {
                    comp(
                        "claude-auth",
                        "Claude subscription login",
                        1,
                        true,
                        format!(
                            "Signed in via {} (expires {})",
                            v["authMethod"].as_str().unwrap_or("claude.ai"),
                            v["loginExpiresAt"].as_str().unwrap_or("n/a")
                        ),
                        "Sign in again",
                    )
                } else {
                    comp(
                        "claude-auth",
                        "Claude subscription login",
                        1,
                        false,
                        "Not signed in",
                        "Sign in",
                    )
                }
            } else {
                comp(
                    "claude-auth",
                    "Claude subscription login",
                    1,
                    false,
                    "Install Claude Code first",
                    "Sign in",
                )
            };
            let present = cli.ok;
            vec![
                optional_when_absent(cli, present),
                optional_when_absent(auth, present),
            ]
        }),
        std::thread::spawn(|| {
            let hermes = tool_comp("hermes", "Hermes Agent", 2, "hermes", "Install");
            let plugin = if hermes.ok {
                let ok = procs::run("hermes", &["plugins", "show", PLUGIN])
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                comp(
                    "hermes-plugin",
                    "Claude subscription plugin for Hermes",
                    1,
                    ok,
                    if ok {
                        format!("{PLUGIN} installed")
                    } else {
                        format!("{PLUGIN} not installed")
                    },
                    if ok { "Reinstall" } else { "Install plugin" },
                )
            } else {
                comp(
                    "hermes-plugin",
                    "Claude subscription plugin for Hermes",
                    1,
                    false,
                    "Install Hermes first",
                    "Install plugin",
                )
            };
            // The Claude bridge matters only when Claude Code is present.
            vec![hermes, optional_when_absent(plugin, claude_present())]
        }),
        std::thread::spawn(move || {
            vec![match bus_selftest(&vault_path) {
                Ok(n) if n == HUB_TOOLS => comp(
                    "bus-selftest",
                    "Native MCP server self-test",
                    5,
                    true,
                    format!("initialize + tools/list returned all {HUB_TOOLS} tools"),
                    "Run again",
                ),
                Ok(n) => comp(
                    "bus-selftest",
                    "Native MCP server self-test",
                    5,
                    false,
                    format!("expected {HUB_TOOLS} tools, got {n}"),
                    "Run again",
                ),
                Err(e) => comp(
                    "bus-selftest",
                    "Native MCP server self-test",
                    5,
                    false,
                    format!("{e:#}"),
                    "Run again",
                ),
            }]
        }),
    ];
    let mut components: Vec<Component> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap_or_default())
        .collect();

    let model = hermes_cfg
        .get("model")
        .and_then(|m| m.get("default"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let provider = hermes_cfg
        .get("model")
        .and_then(|m| m.get("provider"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    components.push(match model {
        Some(m) => comp(
            "hermes-model",
            "Hermes model configuration",
            2,
            true,
            format!(
                "{m}{}",
                provider.map(|p| format!(" via {p}")).unwrap_or_default()
            ),
            "Change model",
        ),
        None => comp(
            "hermes-model",
            "Hermes model configuration",
            2,
            false,
            "No default model in config.yaml",
            "Configure",
        ),
    });

    let (vok, missing) = vault.structure_ok();
    components.push(comp(
        "vault",
        "Obsidian vault",
        3,
        vok,
        if vok {
            vault.root.display().to_string()
        } else {
            format!("{} - missing {}", vault.root.display(), missing.join(", "))
        },
        if vok { "Repair" } else { "Initialize" },
    ));

    let notion_cfg = servers.get("notion").is_some();
    let token = procs::hermes_home()
        .join("mcp-tokens")
        .join("notion.json")
        .exists();
    components.push(optional(comp(
        "notion",
        "Notion MCP (via Hermes, optional)",
        4,
        notion_cfg && token,
        match (notion_cfg, token) {
            (true, true) => "Configured and authorized".to_string(),
            (true, false) => "Configured, not authorized".to_string(),
            _ => "Not configured".to_string(),
        },
        if notion_cfg { "Authorize" } else { "Connect" },
    )));

    let hermes_reg = servers.get("agentos-hub").and_then(|e| {
        let enabled = e.get("enabled").and_then(|b| b.as_bool()).unwrap_or(true);
        let cmd = e.get("command")?.as_str()?.to_string();
        let args: Vec<String> = e
            .get("args")
            .and_then(|a| a.as_sequence())
            .map(|s| {
                s.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        enabled.then(|| describe_target(&cmd, &args))
    });
    components.push(comp(
        "mcp-hermes",
        "Bus registered in Hermes",
        5,
        hermes_reg.is_some(),
        hermes_reg.unwrap_or_else(|| "not registered".into()),
        "Wire Config",
    ));

    let claude_reg = json_registration(&read_json(&procs::claude_json()));
    components.push(optional_when_absent(
        comp(
            "mcp-claude",
            "Claude Code",
            6,
            claude_reg.is_some(),
            claude_reg.unwrap_or_else(|| {
                if claude_present() {
                    "not registered".into()
                } else {
                    "Claude Code is not installed".into()
                }
            }),
            "Wire Config",
        ),
        claude_present(),
    ));
    for (id, label, agent, path) in [
        ("mcp-qoder", "Qoder", "qoder", procs::qoder_settings()),
        (
            "mcp-antigravity",
            "Antigravity",
            "antigravity",
            procs::gemini_mcp_config(),
        ),
    ] {
        let reg = json_registration(&read_json(&path));
        let present = agent_present(agent);
        let c = comp(
            id,
            label,
            6,
            reg.is_some(),
            reg.unwrap_or_else(|| {
                if present {
                    "not registered".into()
                } else {
                    format!("{label} is not installed (optional)")
                }
            }),
            "Wire Config",
        );
        components.push(if present { c } else { optional(c) });
    }

    let order = [
        "git",
        "gh",
        "node",
        "path",
        "claude-cli",
        "claude-auth",
        "hermes-plugin",
        "hermes",
        "hermes-model",
        "vault",
        "notion",
        "bus-selftest",
        "mcp-hermes",
        "mcp-claude",
        "mcp-qoder",
        "mcp-antigravity",
    ];
    components.sort_by_key(|c| order.iter().position(|o| *o == c.id).unwrap_or(99));

    let pillars: Vec<Pillar> = PILLARS
        .iter()
        .map(|(n, t)| Pillar {
            number: *n,
            title: t.to_string(),
            ok: components
                .iter()
                .filter(|c| c.pillar == *n)
                .all(|c| c.ok || c.optional),
        })
        .collect();
    let connected = pillars.iter().filter(|p| p.number > 0 && p.ok).count();
    StackStatus {
        components,
        pillars,
        connected,
        total: 6,
        vault: vault.root.display().to_string(),
        studio_binary: crate::mcp_manager::hub_binary_for(
            &crate::mcp_manager::hub_root(),
            env!("CARGO_PKG_VERSION"),
        )
        .display()
        .to_string(),
        checked_at: crate::vault::now(),
    }
}

// ---------------------------------------------------------------------------
// Repair actions
// ---------------------------------------------------------------------------

struct Job {
    app: AppHandle,
    id: String,
    component: String,
}

impl Job {
    fn emit(&self, level: &str, line: &str) {
        let _ = self.app.emit(EV_SETUP, json!({"job": self.id, "component": self.component, "level": level, "line": line, "done": false}));
    }
    fn finish(&self, ok: bool, line: &str) {
        let _ = self.app.emit(EV_SETUP, json!({"job": self.id, "component": self.component, "level": if ok { "ok" } else { "error" }, "line": line, "done": true, "ok": ok}));
    }
}

/// Run a tool, streaming each output line to the UI. Returns success.
fn stream(job: &Job, tool: &str, args: &[&str], stdin_text: Option<&str>) -> Result<bool> {
    let exe = procs::find(tool).ok_or_else(|| anyhow!("{tool} is not installed"))?;
    job.emit("cmd", &format!("$ {tool} {}", args.join(" ")));
    let mut child = procs::command(&exe)
        .args(args)
        .stdin(if stdin_text.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {tool}"))?;
    if let (Some(text), Some(mut stdin)) = (stdin_text, child.stdin.take()) {
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    let out = child.stdout.take();
    let err = child.stderr.take();
    std::thread::scope(|s| {
        if let Some(o) = out {
            s.spawn(|| {
                BufReader::new(o)
                    .lines()
                    .map_while(Result::ok)
                    .for_each(|l| job.emit("out", &l))
            });
        }
        if let Some(e) = err {
            s.spawn(|| {
                BufReader::new(e)
                    .lines()
                    .map_while(Result::ok)
                    .for_each(|l| job.emit("err", &l))
            });
        }
    });
    Ok(child.wait()?.success())
}

/// Run an interactive flow (OAuth, installers) in its own visible console window and wait for it.
fn interactive(job: &Job, program: &Path, args: &[&str]) -> Result<bool> {
    job.emit(
        "cmd",
        &format!(
            "$ {} {} (opens in its own window)",
            program.display(),
            args.join(" ")
        ),
    );
    let mut cmd = Command::new(program);
    cmd.args(args).env("PATH", procs::search_path());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }
    let status = cmd
        .status()
        .with_context(|| format!("starting {}", program.display()))?;
    Ok(status.success())
}

fn interactive_tool(job: &Job, tool: &str, args: &[&str]) -> Result<bool> {
    let exe = procs::find(tool).ok_or_else(|| anyhow!("{tool} is not installed"))?;
    interactive(job, &exe, args)
}

fn winget(job: &Job, id: &str) -> Result<bool> {
    stream(
        job,
        "winget",
        &[
            "install",
            "--id",
            id,
            "-e",
            "--silent",
            "--accept-package-agreements",
            "--accept-source-agreements",
        ],
        None,
    )
}

fn os_install(job: &Job, winget_id: &str, brew: &str) -> Result<bool> {
    if cfg!(windows) {
        if procs::find("winget").is_none() {
            bail!(
                "winget is not available. Install \"App Installer\" from the Microsoft Store, or install {winget_id} yourself, then press Re-check"
            );
        }
        return winget(job, winget_id);
    }
    if procs::find("brew").is_some() {
        return stream(job, "brew", &["install", brew], None);
    }
    bail!("Install {brew} with your system package manager, then press Re-check")
}

fn fix_path(job: &Job) -> Result<bool> {
    let missing = path_missing();
    if missing.is_empty() {
        job.emit("out", "PATH already contains every tool directory");
        return Ok(true);
    }
    if cfg!(windows) {
        let list = missing
            .iter()
            .map(|p| format!("'{}'", p.display().to_string().replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let script = format!(
            "$add=@({list}); $cur=[Environment]::GetEnvironmentVariable('Path','User'); $parts=@($cur -split ';' | Where-Object {{ $_ }}); foreach($d in $add){{ if($parts -notcontains $d){{ $parts += $d }} }}; [Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User'); Write-Output ('User PATH now has ' + $parts.Count + ' entries')"
        );
        return stream(
            job,
            "powershell",
            &[
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ],
            None,
        );
    }
    let h = crate::settings::home();
    let line = format!(
        "\n# >>> agentos-studio path >>>\nexport PATH=\"{}:$PATH\"\n# <<< agentos-studio path <<<\n",
        missing.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(":")
    );
    for rc in [".profile", ".zshrc", ".bashrc"] {
        let p = h.join(rc);
        if rc == ".profile" || p.exists() {
            let text = fs::read_to_string(&p).unwrap_or_default();
            if !text.contains("agentos-studio path") {
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&p)?
                    .write_all(line.as_bytes())?;
                job.emit("out", &format!("Updated {}", p.display()));
            }
        }
    }
    Ok(true)
}

/// Register agentos-hub for one agent, pointing at this version's staged hub binary
/// (`%LOCALAPPDATA%/AgentOS Hub/<version>/`), never at the app's own install folder: a registered
/// exe is held open by every agent session, which would block installs and updates.
fn register_hub(job: &Job, agent: &str, vault: &Path) -> Result<bool> {
    let exe = crate::mcp_manager::stage_hub_binary()?;
    job.emit("out", &format!("Hub binary: {}", exe.display()));
    let spec = crate::mcp_manager::agentos_hub_spec(&exe.display().to_string(), vault);
    let mut ok = true;
    for o in crate::mcp_manager::add_mcp_server(&spec, &[agent.to_string()])? {
        job.emit(
            if o.ok { "out" } else { "err" },
            &format!("{}: {}", o.agent, o.detail),
        );
        ok &= o.ok;
    }
    Ok(ok)
}

fn run_component(job: &Job, component: &str, vault: &Vault) -> Result<bool> {
    let v = vault.root.clone();
    match component {
        "git" => os_install(job, "Git.Git", "git"),
        "node" => os_install(job, "OpenJS.NodeJS.LTS", "node"),
        "gh" => {
            if procs::find("gh").is_none() && !os_install(job, "GitHub.cli", "gh")? {
                return Ok(false);
            }
            interactive_tool(
                job,
                "gh",
                &["auth", "login", "--web", "--git-protocol", "https"],
            )
        }
        "path" => fix_path(job),
        "claude-cli" => {
            if cfg!(windows)
                && procs::find("winget").is_some()
                && winget(job, "Anthropic.ClaudeCode")?
            {
                return Ok(true);
            }
            job.emit("out", "Falling back to npm");
            stream(
                job,
                "npm",
                &["install", "-g", "@anthropic-ai/claude-code"],
                None,
            )
        }
        "claude-auth" => interactive_tool(job, "claude", &["auth", "login"]),
        "hermes" => {
            if cfg!(windows) {
                let ps =
                    procs::find("powershell").ok_or_else(|| anyhow!("PowerShell not found"))?;
                interactive(
                    job,
                    &ps,
                    &[
                        "-NoProfile",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-Command",
                        "iex (irm https://hermes-agent.nousresearch.com/install.ps1)",
                    ],
                )
            } else {
                stream(
                    job,
                    "bash",
                    &[
                        "-c",
                        "curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash",
                    ],
                    None,
                )
            }
        }
        "hermes-plugin" => stream(
            job,
            "hermes",
            &["plugins", "install", PLUGIN],
            Some("y\ny\n"),
        ),
        "hermes-model" => interactive_tool(job, "hermes", &["model"]),
        "antigravity-plugin" => {
            job.emit(
                "out",
                &format!("Installing the community plugin {ANTIGRAVITY_PLUGIN_URL} (not published by Nous Research)"),
            );
            if !stream(
                job,
                "hermes",
                &["plugins", "install", ANTIGRAVITY_PLUGIN_URL],
                Some("y\ny\n"),
            )? {
                return Ok(false);
            }
            job.emit(
                "out",
                "Choose Antigravity in the window that opens and sign in",
            );
            interactive_tool(job, "hermes", &["model"])
        }
        "vault" => {
            let created = vault.scaffold()?;
            if created.is_empty() {
                job.emit("out", &format!("Vault at {} already complete", v.display()));
            }
            for c in created {
                job.emit("out", &format!("created {c}"));
            }
            Ok(true)
        }
        "notion" => {
            let configured = read_yaml(&procs::hermes_config())
                .get("mcp_servers")
                .and_then(|s| s.get("notion"))
                .is_some();
            if !configured && !stream(job, "hermes", &["mcp", "install", "notion"], Some("y\n"))? {
                return Ok(false);
            }
            interactive_tool(
                job,
                "hermes",
                &["mcp", "login", "notion", "--flow", "browser"],
            )
        }
        "bus-selftest" => {
            let n = bus_selftest(&v)?;
            job.emit("out", &format!("tools/list returned {n} tools"));
            Ok(n == HUB_TOOLS)
        }
        "mcp-hermes" => register_hub(job, "hermes", &v),
        "mcp-claude" => register_hub(job, "claude-code", &v),
        "mcp-qoder" => register_hub(job, "qoder", &v),
        "mcp-antigravity" => register_hub(job, "antigravity", &v),
        other => bail!("Unknown component '{other}'"),
    }
}

/// The one-click plan for this machine (docs/ONE-CLICK-SETUP.md).
pub fn setup_plan(vault: &Vault) -> crate::setup_flow::Plan {
    let status = check_full_stack_status(vault);
    crate::setup_flow::plan(
        &status.components,
        crate::setup_flow::Os::current(),
        procs::find("winget").is_some(),
    )
}

/// Run the system pass: every missing package in one elevated step. Returns the components that
/// reported success.
fn system_pass(job: &Job, plan: &crate::setup_flow::Plan) -> Result<Vec<String>> {
    use crate::setup_flow::{Os, SYSTEM_PKGS};
    let pkgs: Vec<&crate::setup_flow::SystemPkg> = SYSTEM_PKGS
        .iter()
        .filter(|p| plan.system.iter().any(|s| s == p.component))
        .collect();
    if pkgs.is_empty() {
        return Ok(Vec::new());
    }
    match Os::current() {
        Os::Windows => {
            let dir = crate::store::studio_dir().join("setup");
            std::fs::create_dir_all(&dir)?;
            let log = dir.join(format!("elevated-{}.log", job.id));
            let result = dir.join(format!("elevated-{}.json", job.id));
            let script = dir.join(format!("elevated-{}.ps1", job.id));
            let ids: Vec<&str> = pkgs.iter().map(|p| p.winget).collect();
            std::fs::write(
                &script,
                crate::setup_flow::windows_script(
                    &ids,
                    plan.install_winget,
                    &log.display().to_string(),
                    &result.display().to_string(),
                ),
            )?;
            job.emit(
                "cmd",
                &format!(
                    "Installing {} with one admin prompt (Windows asks once)",
                    ids.join(", ")
                ),
            );
            let ps = procs::find("powershell").ok_or_else(|| anyhow!("PowerShell not found"))?;
            let inner = format!(
                "-NoProfile -ExecutionPolicy Bypass -File \"{}\"",
                script.display()
            );
            let launch = format!(
                "try {{ Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList '{}' ; exit 0 }} catch {{ exit 5 }}",
                inner.replace('\'', "''")
            );
            let mut child = procs::command(&ps)
                .args(["-NoProfile", "-Command", &launch])
                .stdin(Stdio::null())
                .spawn()
                .context("starting the elevated install")?;
            // Stream the elevated script's log while it runs.
            let mut seen = 0usize;
            let status = loop {
                if let Ok(text) = std::fs::read_to_string(&log) {
                    for l in text.lines().skip(seen) {
                        job.emit("out", l.trim_start_matches('\u{feff}'));
                    }
                    seen = text.lines().count();
                }
                if let Some(st) = child.try_wait()? {
                    break st;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            };
            if let Ok(text) = std::fs::read_to_string(&log) {
                for l in text.lines().skip(seen) {
                    job.emit("out", l.trim_start_matches('\u{feff}'));
                }
            }
            let _ = std::fs::remove_file(&script);
            if !status.success() {
                bail!("the admin prompt was declined, so nothing was installed");
            }
            let codes: Value = std::fs::read_to_string(&result)
                .ok()
                .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
                .unwrap_or(Value::Null);
            let _ = std::fs::remove_file(&result);
            Ok(pkgs
                .iter()
                .filter(|p| {
                    codes
                        .get(p.winget)
                        .and_then(Value::as_i64)
                        .is_some_and(crate::setup_flow::winget_ok)
                })
                .map(|p| p.component.to_string())
                .collect())
        }
        Os::MacOs => {
            if procs::find("brew").is_none() {
                job.emit("out", "Homebrew is missing: its installer opens in Terminal and asks for your password");
                let cmd = "/bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"";
                let script = format!(
                    "tell application \"Terminal\" to do script \"{}\"",
                    cmd.replace('\\', "\\\\").replace('"', "\\\"")
                );
                let _ = Command::new("osascript").args(["-e", &script]).status();
                bail!(
                    "finish the Homebrew install in Terminal, then press Set up everything again"
                );
            }
            let names: Vec<&str> = pkgs.iter().map(|p| p.brew).collect();
            let mut args = vec!["install"];
            args.extend(names.iter().copied());
            let ok = stream(job, "brew", &args, None)?;
            Ok(if ok {
                pkgs.iter().map(|p| p.component.to_string()).collect()
            } else {
                Vec::new()
            })
        }
        Os::Linux => {
            let dnf = procs::find("dnf").is_some();
            let mgr = if dnf { "dnf" } else { "apt-get" };
            let names = crate::setup_flow::linux_packages(&plan.system, dnf);
            if procs::find("pkexec").is_none() {
                bail!(
                    "install {} with sudo {mgr} install, then press Re-check",
                    names.join(" ")
                );
            }
            let mut args = vec![mgr, "install", "-y"];
            args.extend(names.iter().map(String::as_str));
            let ok = stream(job, "pkexec", &args, None)?;
            Ok(if ok {
                pkgs.iter().map(|p| p.component.to_string()).collect()
            } else {
                Vec::new()
            })
        }
    }
}

/// "Set up everything": the system pass with one admin prompt, then the user steps, then the
/// sign-ins, re-reading the machine between passes so a fresh install is found at once.
fn set_up_everything(job: &Job, vault: &Vault) -> bool {
    let plan = setup_plan(vault);
    if plan.is_empty() {
        job.emit("out", "Every required component is already set up");
        return true;
    }
    job.emit(
        "out",
        &format!(
            "Plan: install {}; then {}; then sign in: {}",
            if plan.system.is_empty() {
                "nothing".into()
            } else {
                plan.system.join(", ")
            },
            if plan.user.is_empty() {
                "no other steps".into()
            } else {
                plan.user.join(", ")
            },
            if plan.sign_ins.is_empty() {
                "none".into()
            } else {
                plan.sign_ins.join(", ")
            },
        ),
    );
    let mut all_ok = true;
    let installed = match system_pass(job, &plan) {
        Ok(v) => v,
        Err(e) => {
            job.emit("err", &format!("{e:#}"));
            Vec::new()
        }
    };
    // Anything the system pass did not install gets its one-by-one install (Claude Code falls
    // back to npm there), so a failed package never stops the rest.
    let retry: Vec<String> = plan
        .system
        .iter()
        .filter(|s| !installed.contains(s))
        .cloned()
        .collect();
    let after = setup_plan(vault);
    let steps: Vec<String> = retry
        .into_iter()
        .filter(|s| s != "gh")
        .chain(after.user.iter().cloned())
        .chain(after.sign_ins.iter().cloned())
        .collect();
    for t in steps {
        let sub = Job {
            app: job.app.clone(),
            id: job.id.clone(),
            component: t.clone(),
        };
        sub.emit("step", &format!("== {t} =="));
        let ok = run_component(&sub, &t, vault).unwrap_or_else(|e| {
            sub.emit("err", &format!("{e:#}"));
            false
        });
        sub.emit(
            if ok { "ok" } else { "err" },
            &format!("{t}: {}", if ok { "done" } else { "needs attention" }),
        );
        all_ok &= ok;
    }
    all_ok && setup_plan(vault).system.is_empty()
}

/// Start an install/repair job in the background; progress arrives as `agentos://setup-progress`.
pub fn install_or_repair_component(app: AppHandle, vault: Vault, component: String) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let job_id = id.clone();
    std::thread::spawn(move || {
        let job = Job {
            app,
            id: job_id,
            component: component.clone(),
        };
        if component == "everything" {
            let ok = set_up_everything(&job, &vault);
            job.finish(
                ok,
                if ok {
                    "Set up finished"
                } else {
                    "Set up finished with items that need attention"
                },
            );
            return;
        }
        let targets: Vec<String> = if component == "all" {
            let status = check_full_stack_status(&vault);
            let broken: Vec<String> = status
                .components
                .iter()
                // Optional items (Notion, IDEs that are not installed) are set up one by one, on
                // purpose; "Auto-Setup and Repair Everything" never opens a login or writes a config for them.
                .filter(|c| !c.ok && !c.optional)
                .map(|c| c.id.clone())
                .collect();
            if broken.is_empty() {
                job.emit("out", "Every component is already healthy");
            } else {
                job.emit("out", &format!("Repairing: {}", broken.join(", ")));
            }
            broken
        } else {
            vec![component.clone()]
        };
        let mut all_ok = true;
        for t in &targets {
            let sub = Job {
                app: job.app.clone(),
                id: job.id.clone(),
                component: t.clone(),
            };
            sub.emit("step", &format!("== {t} =="));
            let ok = match run_component(&sub, t, &vault) {
                Ok(ok) => ok,
                Err(e) => {
                    sub.emit("err", &format!("{e:#}"));
                    false
                }
            };
            sub.emit(
                if ok { "ok" } else { "err" },
                &format!("{t}: {}", if ok { "done" } else { "needs attention" }),
            );
            all_ok &= ok;
        }
        job.finish(
            all_ok,
            if all_ok {
                "Finished"
            } else {
                "Finished with items that need attention"
            },
        );
    });
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_agent_is_optional_and_never_reddens_a_pillar() {
        let absent = optional_when_absent(
            comp(
                "claude-cli",
                "Claude Code CLI",
                1,
                false,
                "claude not found on PATH",
                "Install",
            ),
            false,
        );
        assert!(absent.optional);
        assert!(absent.detail.ends_with("(optional)"));
        let present = optional_when_absent(
            comp(
                "claude-cli",
                "Claude Code CLI",
                1,
                true,
                "2.0.0",
                "Reinstall",
            ),
            true,
        );
        assert!(!present.optional);
        assert_eq!(present.detail, "2.0.0");
        let pillars: Vec<Pillar> = vec![Pillar {
            number: 1,
            title: "Claude Subscription Bridge".into(),
            ok: [absent].iter().all(|c| c.ok || c.optional),
        }];
        assert!(pillars[0].ok);
    }

    #[test]
    fn json_registration_detects_targets() {
        let v = json!({"mcpServers": {"agentos-hub": {"command": "python.exe", "args": ["C:/x/agent_bus_server.py"]}}});
        assert_eq!(
            json_registration(&v).as_deref(),
            Some("Python agent_bus_server.py")
        );
        let d = json!({"mcpServers": {"agentos-hub": {"command": "x", "disabled": true}}});
        assert!(json_registration(&d).is_none());
        assert!(json_registration(&json!({})).is_none());
    }
}
