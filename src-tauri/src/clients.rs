//! Apps that call the hub but never run missions: Claude Desktop, Cursor, VS Code, Windsurf and
//! the Codex CLI. Studio adds `agentos-hub` to each one's MCP config when it is installed, moves
//! it to the new hub on every update like the four agents, and gives it no identity token, so
//! these apps read the bus and memory but never write as an agent (`hub_identity`).
//!
//! A config Studio cannot parse is never overwritten: the user may keep comments in it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::settings::home;

pub const SERVER: &str = "agentos-hub";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `{"mcpServers": {name: {command, args}}}` (Claude Desktop, Cursor, Windsurf).
    McpServers,
    /// `{"servers": {name: {type: "stdio", command, args}}}` (VS Code).
    VsCode,
    /// `[mcp_servers.<name>]` in TOML (Codex CLI).
    CodexToml,
}

#[derive(Debug, Clone, Serialize)]
pub struct Client {
    pub id: String,
    pub label: String,
    /// The app is installed on this machine.
    pub present: bool,
    pub config: String,
    /// The hub command registered in the app's config, if any.
    pub registered: Option<String>,
    /// Registered and pointing at this build's hub.
    pub current: bool,
}

struct Spec {
    id: &'static str,
    label: &'static str,
    format: Format,
}

const SPECS: [Spec; 5] = [
    Spec {
        id: "claude-desktop",
        label: "Claude Desktop",
        format: Format::McpServers,
    },
    Spec {
        id: "cursor",
        label: "Cursor",
        format: Format::McpServers,
    },
    Spec {
        id: "vscode",
        label: "VS Code",
        format: Format::VsCode,
    },
    Spec {
        id: "windsurf",
        label: "Windsurf",
        format: Format::McpServers,
    },
    Spec {
        id: "codex",
        label: "Codex CLI",
        format: Format::CodexToml,
    },
];

/// Per-OS application data folder (`%APPDATA%`, `~/Library/Application Support`, `~/.config`).
fn app_data() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| home().join(".config"))
}

/// Where an app keeps its MCP config, and whether the app is installed.
fn locate(id: &str) -> (PathBuf, bool) {
    let h = home();
    match id {
        "claude-desktop" => {
            // The Microsoft Store build reads its config from the package's virtualised folder.
            if cfg!(windows) {
                if let Some(local) = dirs::data_local_dir() {
                    let pkgs = local.join("Packages");
                    if let Ok(rd) = std::fs::read_dir(&pkgs) {
                        for e in rd.flatten() {
                            let name = e.file_name().to_string_lossy().to_string();
                            if name.starts_with("Claude_") {
                                return (
                                    e.path()
                                        .join("LocalCache")
                                        .join("Roaming")
                                        .join("Claude")
                                        .join("claude_desktop_config.json"),
                                    true,
                                );
                            }
                        }
                    }
                    let classic = local.join("AnthropicClaude").is_dir();
                    let dir = app_data().join("Claude");
                    return (
                        dir.join("claude_desktop_config.json"),
                        classic || dir.is_dir(),
                    );
                }
            }
            let dir = app_data().join("Claude");
            (dir.join("claude_desktop_config.json"), dir.is_dir())
        }
        "cursor" => (
            h.join(".cursor").join("mcp.json"),
            h.join(".cursor").is_dir(),
        ),
        "vscode" => {
            let user = app_data().join("Code").join("User");
            (user.join("mcp.json"), user.is_dir())
        }
        "windsurf" => {
            let dir = h.join(".codeium").join("windsurf");
            (dir.join("mcp_config.json"), dir.is_dir())
        }
        _ => (
            h.join(".codex").join("config.toml"),
            h.join(".codex").is_dir(),
        ),
    }
}

fn spec(id: &str) -> Result<&'static Spec> {
    SPECS
        .iter()
        .find(|s| s.id == id)
        .with_context(|| format!("unknown app '{id}'"))
}

/// The hub entry for one config format. Pure.
pub fn entry(format: Format, exe: &str, vault: &Path) -> Value {
    let args = json!(["--mcp-stdio", "--vault", vault.display().to_string()]);
    match format {
        Format::VsCode => json!({"type": "stdio", "command": exe, "args": args}),
        _ => json!({"command": exe, "args": args}),
    }
}

fn servers_key(format: Format) -> &'static str {
    if format == Format::VsCode {
        "servers"
    } else {
        "mcpServers"
    }
}

/// Add or replace the hub in a JSON config, keeping everything else. Pure.
pub fn upsert_json(text: &str, format: Format, exe: &str, vault: &Path) -> Result<String> {
    let mut v: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(text).context(
            "the config is not plain JSON (it may hold comments), so Studio leaves it alone; add the hub by hand",
        )?
    };
    let obj = v
        .as_object_mut()
        .context("the config is not a JSON object")?;
    let servers = obj
        .entry(servers_key(format))
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = servers
        .as_object_mut()
        .context("the servers entry is not an object")?;
    servers.insert(SERVER.into(), entry(format, exe, vault));
    Ok(serde_json::to_string_pretty(&v)? + "\n")
}

/// Remove the hub from a JSON config. Pure.
pub fn remove_json(text: &str, format: Format) -> Result<String> {
    let mut v: Value = serde_json::from_str(text).context("the config is not plain JSON")?;
    if let Some(s) = v
        .get_mut(servers_key(format))
        .and_then(Value::as_object_mut)
    {
        s.remove(SERVER);
    }
    Ok(serde_json::to_string_pretty(&v)? + "\n")
}

/// The registered hub command in a JSON config. Pure.
pub fn registered_json(text: &str, format: Format) -> Option<String> {
    let v: Value = serde_json::from_str(text).ok()?;
    v.get(servers_key(format))?
        .get(SERVER)?
        .get("command")?
        .as_str()
        .map(String::from)
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

const TOML_HEADER: &str = "[mcp_servers.agentos-hub]";

/// Split a TOML file around the hub table: (before, table, after). Pure.
fn toml_split(text: &str) -> (String, Option<String>, String) {
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines
        .iter()
        .position(|l| l.trim() == TOML_HEADER || l.trim() == "[mcp_servers.\"agentos-hub\"]")
    else {
        return (text.to_string(), None, String::new());
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map(|i| start + 1 + i)
        .unwrap_or(lines.len());
    let join = |ls: &[&str]| {
        if ls.is_empty() {
            String::new()
        } else {
            ls.join("\n") + "\n"
        }
    };
    (
        join(&lines[..start]),
        Some(join(&lines[start..end])),
        join(&lines[end..]),
    )
}

/// Add or replace the hub table in a Codex `config.toml`, keeping every other line. Pure.
pub fn upsert_toml(text: &str, exe: &str, vault: &Path) -> String {
    let table = format!(
        "{TOML_HEADER}\ncommand = {}\nargs = [\"--mcp-stdio\", \"--vault\", {}]\n",
        toml_str(exe),
        toml_str(&vault.display().to_string())
    );
    let (before, _, after) = toml_split(text);
    let mut out = before;
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.push_str(&table);
    if !after.is_empty() {
        out.push('\n');
        out.push_str(&after);
    }
    out
}

/// Remove the hub table from a Codex `config.toml`. Pure.
pub fn remove_toml(text: &str) -> String {
    let (before, _, after) = toml_split(text);
    format!(
        "{}{}",
        before.trim_end_matches('\n').to_string() + "\n",
        after
    )
    .trim_start_matches('\n')
    .to_string()
}

/// The registered hub command in a Codex `config.toml`. Pure.
pub fn registered_toml(text: &str) -> Option<String> {
    let (_, table, _) = toml_split(text);
    let table = table?;
    let line = table
        .lines()
        .find(|l| l.trim_start().starts_with("command"))?;
    let raw = line.split_once('=')?.1.trim();
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.replace("\\\\", "\\").replace("\\\"", "\""))
}

fn registered_in(format: Format, text: &str) -> Option<String> {
    match format {
        Format::CodexToml => registered_toml(text),
        _ => registered_json(text, format),
    }
}

/// Every supported app, with whether it is installed and registered.
pub fn list(hub_exe: &str) -> Vec<Client> {
    SPECS
        .iter()
        .map(|s| {
            let (path, present) = locate(s.id);
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let registered = registered_in(s.format, &text);
            Client {
                id: s.id.into(),
                label: s.label.into(),
                present,
                config: path.display().to_string(),
                current: registered.as_deref().is_some_and(|r| same_path(r, hub_exe)),
                registered,
            }
        })
        .collect()
}

fn same_path(a: &str, b: &str) -> bool {
    a.replace('/', "\\")
        .eq_ignore_ascii_case(&b.replace('/', "\\"))
}

/// Add the hub to one app's config. Refuses an app that is not installed.
pub fn connect(id: &str, hub_exe: &str, vault: &Path) -> Result<()> {
    let s = spec(id)?;
    let (path, present) = locate(id);
    if !present {
        bail!("{} is not installed on this machine", s.label);
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let out = match s.format {
        Format::CodexToml => upsert_toml(&text, hub_exe, vault),
        f => upsert_json(&text, f, hub_exe, vault)?,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if !text.is_empty() {
        let _ = std::fs::write(path.with_extension("agentos-backup"), &text);
    }
    std::fs::write(&path, out).with_context(|| format!("writing {}", path.display()))
}

/// Remove the hub from one app's config.
pub fn disconnect(id: &str) -> Result<()> {
    let s = spec(id)?;
    let (path, _) = locate(id);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let out = match s.format {
        Format::CodexToml => remove_toml(&text),
        f => remove_json(&text, f)?,
    };
    std::fs::write(&path, out).with_context(|| format!("writing {}", path.display()))
}

/// Move every registered app that points at an older staged hub to `hub_exe`; apps the user
/// pointed somewhere else are left alone. Returns the apps moved.
pub fn follow_update(hub_exe: &str, vault: &Path, hub_root: &Path) -> Vec<String> {
    let root = hub_root.display().to_string();
    list(hub_exe)
        .into_iter()
        .filter(|c| c.present && !c.current)
        .filter(|c| {
            c.registered.as_deref().is_some_and(|r| {
                r.replace('/', "\\")
                    .to_lowercase()
                    .starts_with(&root.replace('/', "\\").to_lowercase())
            })
        })
        .filter(|c| connect(&c.id, hub_exe, vault).is_ok())
        .map(|c| c.label)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Users\u\AppData\Local\AgentOS Hub\1.1.0\agentos-studio.exe";

    #[test]
    fn json_configs_keep_every_other_server_and_setting() {
        let v = Path::new(r"C:\vault");
        let before = r#"{"theme": "dark", "mcpServers": {"other": {"command": "x"}}}"#;
        let out = upsert_json(before, Format::McpServers, EXE, v).unwrap();
        let p: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(p["theme"], "dark");
        assert_eq!(p["mcpServers"]["other"]["command"], "x");
        assert_eq!(p["mcpServers"][SERVER]["args"][0], "--mcp-stdio");
        assert_eq!(
            registered_json(&out, Format::McpServers).as_deref(),
            Some(EXE)
        );
        let gone = remove_json(&out, Format::McpServers).unwrap();
        assert!(registered_json(&gone, Format::McpServers).is_none());
        assert!(gone.contains("other"));
        let vs = upsert_json("", Format::VsCode, EXE, v).unwrap();
        let p: Value = serde_json::from_str(&vs).unwrap();
        assert_eq!(p["servers"][SERVER]["type"], "stdio");
    }

    #[test]
    fn a_config_with_comments_is_never_overwritten() {
        let jsonc = "{\n  // my servers\n  \"servers\": {}\n}";
        assert!(upsert_json(jsonc, Format::VsCode, EXE, Path::new("v")).is_err());
    }

    #[test]
    fn codex_toml_replaces_only_the_hub_table() {
        let v = Path::new(r"C:\my vault");
        let before = "model = \"o4\"\n\n[mcp_servers.other]\ncommand = \"x\"\n";
        let once = upsert_toml(before, EXE, v);
        assert!(once.contains("model = \"o4\""));
        assert!(once.contains("[mcp_servers.other]"));
        assert_eq!(registered_toml(&once).as_deref(), Some(EXE));
        let newer = EXE.replace("1.1.0", "1.2.0");
        let twice = upsert_toml(&once, &newer, v);
        assert_eq!(twice.matches(TOML_HEADER).count(), 1, "{twice}");
        assert_eq!(registered_toml(&twice).as_deref(), Some(newer.as_str()));
        assert!(twice.contains("[mcp_servers.other]"));
        let gone = remove_toml(&twice);
        assert!(registered_toml(&gone).is_none());
        assert!(gone.contains("[mcp_servers.other]") && gone.contains("model = \"o4\""));
        // A table in the middle keeps what follows it.
        let mid = format!("{TOML_HEADER}\ncommand = \"old\"\n\n[profiles.a]\nk = 1\n");
        let out = upsert_toml(&mid, EXE, v);
        assert!(out.contains("[profiles.a]\nk = 1"), "{out}");
        assert_eq!(registered_toml(&out).as_deref(), Some(EXE));
    }

    /// Live: list the apps on this machine, then connect and disconnect VS Code and restore its
    /// config byte for byte. `cargo test --lib -- --ignored clients_live --nocapture`
    #[test]
    #[ignore]
    fn clients_live() {
        for c in list(EXE) {
            println!(
                "{} present={} registered={:?} at {}",
                c.label, c.present, c.registered, c.config
            );
        }
        let (path, present) = locate("vscode");
        if !present {
            println!("VS Code not installed; skipping the round trip");
            return;
        }
        let before = std::fs::read(&path).ok();
        connect("vscode", EXE, Path::new(r"C:\vault")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(registered_json(&text, Format::VsCode).as_deref(), Some(EXE));
        assert!(list(EXE).iter().any(|c| c.id == "vscode" && c.current));
        disconnect("vscode").unwrap();
        assert!(
            registered_json(&std::fs::read_to_string(&path).unwrap(), Format::VsCode).is_none()
        );
        match before {
            Some(b) => std::fs::write(&path, b).unwrap(),
            None => std::fs::remove_file(&path).unwrap(),
        }
        let _ = std::fs::remove_file(path.with_extension("agentos-backup"));
    }

    #[test]
    fn paths_compare_without_case_or_slash_differences() {
        assert!(same_path(&EXE.to_lowercase(), &EXE.replace('\\', "/")));
    }
}
