//! Real terminals inside Studio: a pseudo terminal (ConPTY on Windows, openpty on macOS
//! and Linux) running a shell, interactive Hermes, or Claude Code. Output is streamed to
//! the UI as UTF-8 safe chunks on `agentos://pty-data`; keystrokes come back through
//! [`write`]. Interactive programs (Hermes REPL and TUI, `claude`, wizards) work exactly
//! as they do in a terminal window, including live reasoning and typed approvals.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::procs;

pub const EV_PTY_DATA: &str = "agentos://pty-data";
pub const EV_PTY_EXIT: &str = "agentos://pty-exit";

/// Bytes of output kept per terminal so a remounted view can replay its screen.
const SCROLLBACK_BYTES: usize = 512 * 1024;

#[derive(Deserialize, Debug, Clone)]
pub struct SpawnRequest {
    /// `shell`, `hermes`, `hermes-tui`, `hermes-resume`, `claude` or `claude-resume`.
    pub profile: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

fn default_cols() -> u16 {
    120
}
fn default_rows() -> u16 {
    30
}

#[derive(Serialize, Clone, Debug)]
pub struct PtyInfo {
    pub id: String,
    pub profile: String,
    pub title: String,
    pub cwd: String,
    pub pid: Option<u32>,
    pub started_at: i64,
}

#[derive(Serialize, Clone)]
struct DataEvent<'a> {
    id: &'a str,
    data: String,
    /// Chunk counter; a snapshot with `seq >= this` already contains this data.
    seq: u64,
}

#[derive(Default)]
struct Scrollback {
    bytes: Vec<u8>,
    seq: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub data: String,
    pub seq: u64,
}

#[derive(Serialize, Clone)]
struct ExitEvent {
    id: String,
    code: Option<u32>,
}

struct Session {
    info: PtyInfo,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    scrollback: Arc<Mutex<Scrollback>>,
}

#[derive(Default, Clone)]
pub struct PtyState {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// Program, arguments and tab title for a profile. Only these programs can be started.
pub fn resolve_profile(
    profile: &str,
    session_id: Option<&str>,
) -> Result<(String, Vec<String>, String)> {
    let sid = || -> Result<String> {
        let s = session_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("This terminal needs a session id"))?;
        if !valid_session_id(s) {
            bail!("Invalid session id");
        }
        Ok(s.to_string())
    };
    Ok(match profile {
        "shell" => {
            if cfg!(windows) {
                let shell = if procs::find("pwsh").is_some() {
                    "pwsh"
                } else {
                    "powershell"
                };
                (shell.into(), vec!["-NoLogo".into()], "PowerShell".into())
            } else {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
                (shell, vec!["-l".into()], "Shell".into())
            }
        }
        "hermes" => ("hermes".into(), vec![], "Hermes".into()),
        "hermes-tui" => ("hermes".into(), vec!["--tui".into()], "Hermes TUI".into()),
        "hermes-resume" => {
            let s = sid()?;
            let short: String = s.chars().take(8).collect();
            (
                "hermes".into(),
                vec!["--resume".into(), s],
                format!("Hermes {short}"),
            )
        }
        "claude" => ("claude".into(), vec![], "Claude Code".into()),
        "claude-resume" => {
            let s = sid()?;
            let short: String = s.chars().take(8).collect();
            (
                "claude".into(),
                vec!["--resume".into(), s],
                format!("Claude {short}"),
            )
        }
        other => bail!("Unknown terminal profile {other}"),
    })
}

/// Split `buf` into the longest valid UTF-8 prefix (returned) and an incomplete trailing
/// sequence (left in `buf`). Invalid bytes in the middle become U+FFFD.
pub fn drain_utf8(buf: &mut Vec<u8>) -> String {
    let mut out = String::new();
    let mut start = 0;
    loop {
        match std::str::from_utf8(&buf[start..]) {
            Ok(s) => {
                out.push_str(s);
                buf.clear();
                return out;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                out.push_str(std::str::from_utf8(&buf[start..start + valid]).unwrap_or_default());
                match e.error_len() {
                    Some(bad) => {
                        out.push('\u{FFFD}');
                        start += valid + bad;
                    }
                    None => {
                        let rest = buf[start + valid..].to_vec();
                        *buf = rest;
                        return out;
                    }
                }
            }
        }
    }
}

fn push_scrollback(sb: &Mutex<Scrollback>, bytes: &[u8]) -> u64 {
    match sb.lock() {
        Ok(mut b) => {
            b.bytes.extend_from_slice(bytes);
            if b.bytes.len() > SCROLLBACK_BYTES {
                let cut = b.bytes.len() - SCROLLBACK_BYTES;
                b.bytes.drain(..cut);
            }
            b.seq += 1;
            b.seq
        }
        Err(_) => 0,
    }
}

fn command_for(program: &str, args: &[String], cwd: &PathBuf) -> Result<CommandBuilder> {
    let exe =
        procs::find(program).ok_or_else(|| anyhow!("{program} is not installed or not on PATH"))?;
    let ext = exe
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut cmd = if cfg!(windows) && (ext == "cmd" || ext == "bat") {
        let mut c = CommandBuilder::new("cmd.exe");
        c.arg("/c");
        c.arg(&exe);
        c
    } else {
        CommandBuilder::new(&exe)
    };
    for a in args {
        cmd.arg(a);
    }
    cmd.cwd(cwd);
    cmd.env("PATH", procs::search_path());
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("PYTHONIOENCODING", "utf-8");
    cmd.env_remove("NO_COLOR");
    cmd.env_remove("FORCE_COLOR");
    Ok(cmd)
}

/// Receives `(event name, payload)` for terminal output and exit.
pub type Sink = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

pub fn spawn(app: AppHandle, state: &PtyState, req: SpawnRequest) -> Result<PtyInfo> {
    let sink: Sink = Arc::new(move |ev, payload| {
        let _ = app.emit(ev, payload);
    });
    spawn_with(sink, state, req)
}

/// Spawn with any event sink (the GUI uses Tauri events; tests collect in memory).
pub fn spawn_with(sink: Sink, state: &PtyState, req: SpawnRequest) -> Result<PtyInfo> {
    let (program, args, title) = resolve_profile(&req.profile, req.session_id.as_deref())?;
    let cwd = req
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(crate::settings::home);
    let size = PtySize {
        rows: req.rows.clamp(5, 500),
        cols: req.cols.clamp(20, 1000),
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = native_pty_system()
        .openpty(size)
        .map_err(|e| anyhow!("opening a pseudo terminal: {e}"))?;
    let cmd = command_for(&program, &args, &cwd)?;
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow!("starting {program}: {e}"))?;
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| anyhow!("terminal reader: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| anyhow!("terminal writer: {e}"))?;
    let id = crate::store::new_id();
    let info = PtyInfo {
        id: id.clone(),
        profile: req.profile.clone(),
        title,
        cwd: cwd.display().to_string(),
        pid: child.process_id(),
        started_at: crate::store::now(),
    };
    let scrollback = Arc::new(Mutex::new(Scrollback::default()));
    state
        .sessions
        .lock()
        .map_err(|_| anyhow!("terminal lock poisoned"))?
        .insert(
            id.clone(),
            Session {
                info: info.clone(),
                master: pair.master,
                writer,
                child,
                scrollback: scrollback.clone(),
            },
        );

    // Reader: bytes -> UTF-8 chunks -> UI.
    {
        let (sink, id, sb) = (sink.clone(), id.clone(), scrollback);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 16 * 1024];
            let mut carry: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let seq = push_scrollback(&sb, &chunk[..n]);
                        carry.extend_from_slice(&chunk[..n]);
                        let text = drain_utf8(&mut carry);
                        if !text.is_empty() {
                            let ev = DataEvent {
                                id: &id,
                                data: text,
                                seq,
                            };
                            sink(EV_PTY_DATA, serde_json::to_value(ev).unwrap_or_default());
                        }
                    }
                }
            }
        });
    }

    // Exit watcher: ConPTY keeps the output pipe open after the child exits, so poll the
    // child and drop the session (closing the pseudo console) when it is gone.
    {
        let (sink, id, sessions) = (sink, id, state.sessions.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(250));
            let mut map = match sessions.lock() {
                Ok(m) => m,
                Err(_) => return,
            };
            let Some(s) = map.get_mut(&id) else {
                drop(map);
                let ev = ExitEvent { id, code: None };
                sink(EV_PTY_EXIT, serde_json::to_value(ev).unwrap_or_default());
                return;
            };
            if let Ok(Some(status)) = s.child.try_wait() {
                map.remove(&id);
                drop(map);
                let ev = ExitEvent {
                    id,
                    code: Some(status.exit_code()),
                };
                sink(EV_PTY_EXIT, serde_json::to_value(ev).unwrap_or_default());
                return;
            }
        });
    }
    Ok(info)
}

pub fn write(state: &PtyState, id: &str, data: &str) -> Result<()> {
    let mut map = state
        .sessions
        .lock()
        .map_err(|_| anyhow!("terminal lock poisoned"))?;
    let s = map
        .get_mut(id)
        .ok_or_else(|| anyhow!("Terminal {id} has exited"))?;
    s.writer
        .write_all(data.as_bytes())
        .context("writing to the terminal")?;
    s.writer.flush().ok();
    Ok(())
}

pub fn resize(state: &PtyState, id: &str, cols: u16, rows: u16) -> Result<()> {
    let map = state
        .sessions
        .lock()
        .map_err(|_| anyhow!("terminal lock poisoned"))?;
    let s = map
        .get(id)
        .ok_or_else(|| anyhow!("Terminal {id} has exited"))?;
    s.master
        .resize(PtySize {
            rows: rows.clamp(5, 500),
            cols: cols.clamp(20, 1000),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| anyhow!("resizing the terminal: {e}"))
}

pub fn kill(state: &PtyState, id: &str) -> Result<()> {
    let mut map = state
        .sessions
        .lock()
        .map_err(|_| anyhow!("terminal lock poisoned"))?;
    let mut s = map
        .remove(id)
        .ok_or_else(|| anyhow!("Terminal {id} has exited"))?;
    if let Some(pid) = s.child.process_id() {
        procs::kill_tree(pid);
    }
    let _ = s.child.kill();
    Ok(())
}

pub fn kill_all(state: &PtyState) {
    let ids: Vec<String> = state
        .sessions
        .lock()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    for id in ids {
        let _ = kill(state, &id);
    }
}

pub fn list(state: &PtyState) -> Vec<PtyInfo> {
    let mut v: Vec<PtyInfo> = state
        .sessions
        .lock()
        .map(|m| m.values().map(|s| s.info.clone()).collect())
        .unwrap_or_default();
    v.sort_by_key(|i| i.started_at);
    v
}

/// The retained output of a terminal, for replay after the view remounts.
pub fn snapshot(state: &PtyState, id: &str) -> Result<Snapshot> {
    let sb = {
        let map = state
            .sessions
            .lock()
            .map_err(|_| anyhow!("terminal lock poisoned"))?;
        map.get(id)
            .ok_or_else(|| anyhow!("Terminal {id} has exited"))?
            .scrollback
            .clone()
    };
    let (bytes, seq) = sb
        .lock()
        .map(|b| (b.bytes.clone(), b.seq))
        .unwrap_or_default();
    let mut bytes = bytes;
    // Drop a partial UTF-8 sequence at the end; the next live chunk carries it.
    let data = drain_utf8(&mut bytes);
    Ok(Snapshot { data, seq })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_drain_keeps_split_sequences() {
        let s = "h\u{e9}llo \u{2713}".as_bytes().to_vec();
        let (a, b) = s.split_at(2); // splits the two-byte e-acute
        let mut buf = a.to_vec();
        assert_eq!(drain_utf8(&mut buf), "h");
        assert_eq!(buf.len(), 1);
        buf.extend_from_slice(b);
        assert_eq!(drain_utf8(&mut buf), "\u{e9}llo \u{2713}");
        assert!(buf.is_empty());
    }

    #[test]
    fn utf8_drain_replaces_invalid_bytes() {
        let mut buf = vec![b'a', 0xff, b'b'];
        assert_eq!(drain_utf8(&mut buf), "a\u{FFFD}b");
    }

    #[test]
    fn profiles_are_a_closed_set() {
        assert!(resolve_profile("rm -rf /", None).is_err());
        assert!(resolve_profile("hermes-resume", None).is_err());
        assert!(resolve_profile("hermes-resume", Some("abc; del *")).is_err());
        let (p, a, _) = resolve_profile("hermes-resume", Some("20261106_120000_ab12cd")).unwrap();
        assert_eq!(p, "hermes");
        assert_eq!(a, vec!["--resume", "20261106_120000_ab12cd"]);
        let (p, a, _) = resolve_profile("claude-resume", Some("6f1c-uuid")).unwrap();
        assert_eq!(p, "claude");
        assert_eq!(a[0], "--resume");
        assert_eq!(
            resolve_profile("hermes-tui", None).unwrap().1,
            vec!["--tui"]
        );
    }
}
