//! Hermes computer use, made visible: what Hermes did on the desktop and the switch for it.
//!
//! Hermes owns the capability (the `computer_use` toolset, driven by cua-driver). Studio reads its
//! state and history and changes it only through Hermes commands: `hermes tools enable/disable`,
//! `hermes tools post-setup cua_driver`, `cua-driver update --apply`. The activity feed comes from
//! the `computer_use` tool results in `state.db`, read only; screenshots are served only from
//! Hermes' own image cache.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::procs;

#[derive(Serialize, Debug, Clone, Default)]
pub struct Status {
    /// `None` when `hermes tools list` could not be read.
    pub toolset_enabled: Option<bool>,
    pub driver_path: Option<String>,
    pub driver_version: Option<String>,
    pub update_available: Option<bool>,
    pub latest_version: Option<String>,
    /// `cua-driver status`, e.g. "Cua Driver daemon is not running" (Hermes starts it on demand).
    pub daemon: Option<String>,
}

/// Whether `computer_use` is enabled for the CLI, from `hermes tools list`. Pure.
pub fn parse_toolset(list: &str) -> Option<bool> {
    let mut in_cli = false;
    for line in list.lines() {
        let t = line.trim();
        if t.ends_with(':') && !line.starts_with(' ') {
            in_cli = t.starts_with("Built-in toolsets (cli)");
            continue;
        }
        if in_cli && t.split_whitespace().nth(2) == Some("computer_use") {
            return Some(t.contains("enabled") && !t.contains("disabled"));
        }
    }
    None
}

/// The newest cua-driver Hermes installed (`<hermes home>/tools/cua-driver-<version>-*`), then PATH.
pub fn driver() -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "cua-driver.exe"
    } else {
        "cua-driver"
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(procs::hermes_home().join("tools"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("cua-driver-"))
        })
        .map(|p| p.join(exe))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found.pop().or_else(|| procs::find("cua-driver"))
}

fn run_driver(exe: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(procs::CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    Some(format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

pub fn status() -> Status {
    let mut s = Status::default();
    if let Some(out) = procs::run("hermes", &["tools", "list"]) {
        s.toolset_enabled = parse_toolset(&String::from_utf8_lossy(&out.stdout));
    }
    let Some(exe) = driver() else { return s };
    s.driver_path = Some(exe.display().to_string());
    s.driver_version = run_driver(&exe, &["--version"])
        .and_then(|t| t.split_whitespace().nth(1).map(String::from));
    if let Some(v) = run_driver(&exe, &["check-update", "--json"]).and_then(|t| {
        let (a, b) = (t.find('{')?, t.rfind('}')?);
        serde_json::from_str::<Value>(&t[a..=b]).ok()
    }) {
        s.update_available = v.get("update_available").and_then(Value::as_bool);
        s.latest_version = v
            .get("latest_version")
            .and_then(Value::as_str)
            .map(String::from);
    }
    s.daemon = run_driver(&exe, &["status"]).and_then(|t| {
        t.lines()
            .map(str::trim)
            .find(|l| l.to_lowercase().contains("daemon"))
            .map(String::from)
    });
    s
}

/// Turn the toolset on or off for Hermes.
pub fn set_enabled(on: bool) -> Result<String> {
    let out = procs::run(
        "hermes",
        &[
            "tools",
            if on { "enable" } else { "disable" },
            "computer_use",
        ],
    )
    .context("hermes is not installed")?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(text)
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Action {
    pub session_id: String,
    pub at: f64,
    /// `capture`, `click`, `wait`, `list_windows`, ... as far as the result says.
    pub action: String,
    pub summary: String,
    pub ok: Option<bool>,
    pub screenshot: Option<String>,
}

/// One `computer_use` tool result, as stored by Hermes. Pure.
pub fn parse_result(session_id: &str, at: f64, content: &str) -> Action {
    let shot = content
        .find("screenshot saved to ")
        .map(|i| &content[i + "screenshot saved to ".len()..])
        .and_then(|rest| {
            let end = rest.find([')', '\n', '"']).unwrap_or(rest.len());
            let p = rest[..end].trim().replace("\\\\", "\\");
            (!p.is_empty()).then_some(p)
        });
    let (action, summary, ok) = match serde_json::from_str::<Value>(content) {
        Ok(v) => {
            let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let action = if !s("action").is_empty() {
                s("action")
            } else if v.get("windows").is_some() {
                "list_windows".into()
            } else if !s("mode").is_empty() {
                "capture".into()
            } else {
                "result".into()
            };
            let summary = [s("message"), s("summary")]
                .into_iter()
                .find(|x| !x.is_empty())
                .unwrap_or_else(|| {
                    v.get("windows")
                        .and_then(Value::as_array)
                        .map(|w| format!("{} windows", w.len()))
                        .unwrap_or_default()
                });
            (action, summary, v.get("ok").and_then(Value::as_bool))
        }
        Err(_) => {
            let first = content.lines().next().unwrap_or("").trim().to_string();
            let action = first
                .split_whitespace()
                .next()
                .unwrap_or("result")
                .to_string();
            (action, first, None)
        }
    };
    Action {
        session_id: session_id.to_string(),
        at,
        action,
        summary: summary.chars().take(240).collect(),
        ok,
        screenshot: shot,
    }
}

pub fn activity(limit: usize) -> Result<Vec<Action>> {
    let db = crate::hermes_engine::state_db();
    let c = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut st = c.prepare(
        "SELECT session_id, timestamp, content FROM messages
         WHERE tool_name = 'computer_use' AND role = 'tool' AND content IS NOT NULL
         ORDER BY timestamp DESC LIMIT ?1",
    )?;
    let rows = st.query_map([limit.min(200) as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    Ok(rows
        .flatten()
        .map(|(s, at, content)| parse_result(&s, at, &content))
        .collect())
}

/// Whether `path` is a computer-use screenshot inside Hermes' image cache. Pure given `cache`.
pub fn allowed_screenshot(path: &Path, cache: &Path) -> bool {
    let (Ok(p), Ok(c)) = (path.canonicalize(), cache.canonicalize()) else {
        return false;
    };
    let name_ok = p
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("computer_use_"));
    let ext_ok = p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp"));
    p.starts_with(&c) && name_ok && ext_ok
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// A screenshot as a data URL, only from Hermes' image cache and at most 6 MB.
pub fn screenshot(path: &str) -> Result<String> {
    let p = PathBuf::from(path);
    let cache = procs::hermes_home().join("cache").join("images");
    if !allowed_screenshot(&p, &cache) {
        bail!("Only computer-use screenshots in Hermes' image cache can be shown");
    }
    let bytes = std::fs::read(&p)?;
    if bytes.len() > 6 * 1024 * 1024 {
        bail!("Screenshot is larger than 6 MB");
    }
    let mime = match p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    };
    Ok(format!("data:{mime};base64,{}", b64(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_cli_toolset_line() {
        let list = "Built-in toolsets (cli):\n  ✗ disabled  yuanbao  🤖 Yuanbao\n  ✓ enabled  computer_use  🖱️  Computer Use\n\nPlugin toolsets (cli):\n  ✗ disabled  computer_use  x\n";
        assert_eq!(parse_toolset(list), Some(true));
        assert_eq!(
            parse_toolset("Built-in toolsets (cli):\n  ✗ disabled  computer_use  x\n"),
            Some(false)
        );
        assert_eq!(parse_toolset("nothing"), None);
    }

    #[test]
    fn parses_hermes_computer_use_results() {
        let wait = parse_result(
            "s",
            1.0,
            r#"{"ok": true, "action": "wait", "message": "waited 20.00s"}"#,
        );
        assert_eq!(
            (wait.action.as_str(), wait.summary.as_str(), wait.ok),
            ("wait", "waited 20.00s", Some(true))
        );
        let win = parse_result(
            "s",
            1.0,
            r#"{"windows": [{"app_name": "a"}, {"app_name": "b"}]}"#,
        );
        assert_eq!(
            (win.action.as_str(), win.summary.as_str()),
            ("list_windows", "2 windows")
        );
        let cap = parse_result(
            "s",
            1.0,
            "capture mode=vision 1455x883\n0 interactable element(s):\n  (shareable screenshot saved to C:\\Users\\me\\AppData\\Local\\hermes\\cache\\images\\computer_use_c99a.png)",
        );
        assert_eq!(cap.action, "capture");
        assert_eq!(
            cap.screenshot.as_deref(),
            Some("C:\\Users\\me\\AppData\\Local\\hermes\\cache\\images\\computer_use_c99a.png")
        );
    }

    #[test]
    fn screenshots_only_from_the_hermes_image_cache() {
        let d = std::env::temp_dir().join(format!("cu-test-{}", std::process::id()));
        let cache = d.join("cache").join("images");
        std::fs::create_dir_all(&cache).unwrap();
        let good = cache.join("computer_use_1.png");
        let other = cache.join("avatar.png");
        let outside = d.join("computer_use_2.png");
        for f in [&good, &other, &outside] {
            std::fs::write(f, b"x").unwrap();
        }
        assert!(allowed_screenshot(&good, &cache));
        assert!(!allowed_screenshot(&other, &cache));
        assert!(!allowed_screenshot(&outside, &cache));
        assert!(!allowed_screenshot(
            &cache.join("..").join("..").join("computer_use_2.png"),
            &cache
        ));
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(b64(b"Man"), "TWFu");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"M"), "TQ==");
    }
}
