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
        v.extend(python_script_dirs());
    } else {
        v.push(h.join(".hermes").join("bin"));
        v.push(PathBuf::from("/opt/homebrew/bin"));
        v.push(PathBuf::from("/usr/local/bin"));
        v.push(h.join(".npm-global").join("bin"));
    }
    v
}

/// `Scripts` folders of per-user and machine Python installs (pip-installed CLIs live there).
fn python_script_dirs() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(appdata) = dirs::data_dir() {
        roots.push(appdata.join("Python"));
    }
    if let Some(local) = dirs::data_local_dir() {
        roots.push(local.join("Programs").join("Python"));
    }
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("Scripts"))
            .filter(|p| p.is_dir())
            .collect();
        found.sort();
        found.reverse();
        out.extend(found);
    }
    out
}

/// Make this process's own PATH the augmented search path, so every later spawn
/// (including ones from libraries) finds hermes, claude, npm, cargo and Python CLIs.
pub fn heal_process_path() -> usize {
    let before = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).count())
        .unwrap_or(0);
    let healed = search_path();
    let after = std::env::split_paths(&healed).count();
    std::env::set_var("PATH", healed);
    after.saturating_sub(before)
}

/// Launch a tool in its own visible console window without waiting (OAuth and login flows).
pub fn spawn_console(tool: &str, args: &[&str]) -> anyhow::Result<u32> {
    let exe = find(tool).ok_or_else(|| anyhow::anyhow!("{tool} is not installed"))?;
    let mut cmd = Command::new(&exe);
    cmd.args(args).env("PATH", search_path());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }
    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("starting {tool}: {e}"))?;
    Ok(child.id())
}

/// Remove ANSI escape sequences and keep only the last carriage-return segment of a line.
pub fn clean_line(raw: &str) -> String {
    static ANSI: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = ANSI.get_or_init(|| {
        regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*(\x07|\x1b\\)|\x1b[@-Z\\-_]")
            .expect("static regex")
    });
    let stripped = re.replace_all(raw, "");
    let last = stripped
        .trim_end_matches('\r')
        .rsplit('\r')
        .next()
        .unwrap_or("");
    last.to_string()
}

/// Run a tool with a wall-clock limit; the process tree is killed when the limit passes.
pub fn run_with_timeout(
    tool: &str,
    args: &[&str],
    stdin_text: Option<&str>,
    cwd: Option<&Path>,
    limit: std::time::Duration,
) -> anyhow::Result<(Option<i32>, String, String)> {
    run_with_timeout_env(tool, args, stdin_text, cwd, limit, &[])
}

/// `run_with_timeout` with extra environment variables for the child only (the planner points
/// Hermes at a private kanban database this way).
pub fn run_with_timeout_env(
    tool: &str,
    args: &[&str],
    stdin_text: Option<&str>,
    cwd: Option<&Path>,
    limit: std::time::Duration,
    env: &[(&str, &str)],
) -> anyhow::Result<(Option<i32>, String, String)> {
    let exe = find(tool).ok_or_else(|| anyhow::anyhow!("{tool} is not installed"))?;
    let mut cmd = command(&exe);
    cmd.args(args)
        .env("PYTHONIOENCODING", "utf-8")
        .env("NO_COLOR", "1")
        .envs(env.iter().copied())
        .stdin(if stdin_text.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    wait_with_timeout(cmd, tool, stdin_text, limit)
}

/// Run a shell command line (a done check the owner typed) with a time limit. On Windows the line
/// reaches `cmd.exe` verbatim (`/D /S /C "<line>"`, cmd's own quote rule): passed as an ordinary
/// argument, Rust escapes its inner quotes as `\"`, which cmd does not understand, so a command
/// such as `node -e "..."` broke.
pub fn run_shell_with_timeout(
    line: &str,
    cwd: Option<&Path>,
    limit: std::time::Duration,
) -> anyhow::Result<(Option<i32>, String, String)> {
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = command(std::path::Path::new("cmd"));
        c.raw_arg(format!("/D /S /C \"{line}\""));
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = command(std::path::Path::new("sh"));
        c.args(["-c", line]);
        c
    };
    cmd.env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    wait_with_timeout(cmd, "the command", None, limit)
}

fn wait_with_timeout(
    mut cmd: std::process::Command,
    tool: &str,
    stdin_text: Option<&str>,
    limit: std::time::Duration,
) -> anyhow::Result<(Option<i32>, String, String)> {
    use std::io::{Read, Write};
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("starting {tool}: {e}"))?;
    if let (Some(text), Some(mut stdin)) = (stdin_text, child.stdin.take()) {
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let out_t = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_end(&mut s);
        }
        String::from_utf8_lossy(&s).into_owned()
    });
    let err_t = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_end(&mut s);
        }
        String::from_utf8_lossy(&s).into_owned()
    });
    let deadline = std::time::Instant::now() + limit;
    let code = loop {
        if let Some(status) = child.try_wait()? {
            break status.code();
        }
        if std::time::Instant::now() >= deadline {
            kill_tree(child.id());
            let _ = child.wait();
            anyhow::bail!("{tool} did not finish within {}s", limit.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    Ok((code, stdout, stderr))
}

/// The persisted user PATH (Windows registry), which can be newer than this process's PATH.
#[cfg(windows)]
pub fn user_path_entries() -> Vec<String> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;
    let user: String = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Environment")
        .and_then(|k| k.get_value("Path"))
        .unwrap_or_default();
    // Installers (winget, Git, Node.js) write the machine PATH, which a running process never
    // sees; reading it here lets Setup find a tool it just installed without a restart.
    let machine: String = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment")
        .and_then(|k| k.get_value("Path"))
        .unwrap_or_default();
    let raw = format!("{user};{machine}");
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
