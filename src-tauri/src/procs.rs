//! Process helpers: tool discovery on an augmented PATH, hidden-window spawning,
//! and well-known config locations for Hermes, Claude Code, Qoder and Antigravity.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::settings::home;

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Directories CLI installers commonly use that GUI processes often miss.
pub fn extra_bin_dirs() -> Vec<PathBuf> {
    let h = home();
    let mut v = vec![h.join(".local").join("bin"), h.join(".cargo").join("bin")];
    if cfg!(windows) {
        if let Some(appdata) = dirs::data_dir() {
            v.push(appdata.join("npm"));
        }
        if let Some(local) = dirs::data_local_dir() {
            v.push(local.join("hermes").join("bin"));
        }
        v.push(PathBuf::from(r"C:\Program Files\nodejs"));
        v.push(PathBuf::from(r"C:\Program Files\Git\cmd"));
        v.push(PathBuf::from(r"C:\Program Files\GitHub CLI"));
    } else {
        v.push(h.join(".hermes").join("bin"));
        v.push(PathBuf::from("/opt/homebrew/bin"));
        v.push(PathBuf::from("/usr/local/bin"));
        v.push(h.join(".npm-global").join("bin"));
    }
    v
}

/// The persisted user PATH (Windows registry), which can be newer than this process's PATH.
#[cfg(windows)]
pub fn user_path_entries() -> Vec<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let raw: String = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Environment")
        .and_then(|k| k.get_value("Path"))
        .unwrap_or_default();
    raw.split(';')
        .filter(|s| !s.trim().is_empty())
        .map(expand_env)
        .collect()
}

#[cfg(not(windows))]
pub fn user_path_entries() -> Vec<String> {
    Vec::new()
}

#[cfg(windows)]
fn expand_env(s: &str) -> String {
    let re = regex::Regex::new(r"%([^%]+)%").expect("static regex");
    re.replace_all(s, |c: &regex::Captures| {
        std::env::var(&c[1]).unwrap_or_else(|_| c[0].to_string())
    })
    .into_owned()
}

/// PATH used for every child process: process PATH, then persisted user PATH, then known bin dirs.
pub fn search_path() -> OsString {
    let mut parts: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for e in user_path_entries() {
        let p = PathBuf::from(e);
        if !parts.contains(&p) {
            parts.push(p);
        }
    }
    for d in extra_bin_dirs() {
        if d.is_dir() && !parts.contains(&d) {
            parts.push(d);
        }
    }
    std::env::join_paths(parts).unwrap_or_default()
}

pub fn find(tool: &str) -> Option<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| home());
    which::which_in(tool, Some(search_path()), cwd).ok()
}

/// Build a Command for a discovered tool with the augmented PATH and no console window.
pub fn command(program: &Path) -> Command {
    let mut c = Command::new(program);
    c.env("PATH", search_path());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Run a tool to completion and capture its output.
pub fn run(tool: &str, args: &[&str]) -> Option<Output> {
    let exe = find(tool)?;
    command(&exe).args(args).stdin(Stdio::null()).output().ok()
}

/// First line of `<tool> --version`, or None when the tool is missing or broken.
pub fn version(tool: &str) -> Option<String> {
    let out = run(tool, &["--version"])?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("installed")
        .trim()
        .to_string();
    Some(first)
}

pub fn hermes_home() -> PathBuf {
    if let Ok(h) = std::env::var("HERMES_HOME") {
        if !h.trim().is_empty() {
            return PathBuf::from(h);
        }
    }
    if cfg!(windows) {
        if let Some(local) = dirs::data_local_dir() {
            let p = local.join("hermes");
            if p.join("config.yaml").exists() || !home().join(".hermes").exists() {
                return p;
            }
        }
    }
    home().join(".hermes")
}

pub fn hermes_config() -> PathBuf {
    hermes_home().join("config.yaml")
}
pub fn claude_json() -> PathBuf {
    home().join(".claude.json")
}
pub fn qoder_settings() -> PathBuf {
    home().join(".qoder").join("settings.json")
}
pub fn gemini_mcp_config() -> PathBuf {
    home()
        .join(".gemini")
        .join("config")
        .join("mcp_config.json")
}

/// Kill a process and its children.
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        if let Some(tk) = find("taskkill") {
            let _ = command(&tk)
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .output();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .output();
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output();
    }
}
