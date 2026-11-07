//! Streaming chat engine. Each message runs one `hermes chat --format stream-json` turn
//! (or `claude -p --output-format stream-json`) inside the chosen working directory,
//! continuing the session when an id is given. Output is parsed into structured events
//! (`token`, `thinking`, `tool_start`, `tool_result`, `status`, `session`, `error`,
//! `complete`) and emitted on `agentos://chat-event`.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::procs;

pub const EV_CHAT: &str = "agentos://chat-event";
/// Hermes only accepts gateway-registered sources; `gui` keeps Studio chats in the human session list.
pub const STUDIO_SOURCE: &str = "gui";

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct ChatEvent {
    pub run_id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<bool>,
}

impl ChatEvent {
    fn text(kind: &str, text: impl Into<String>) -> ChatEvent {
        ChatEvent {
            kind: kind.into(),
            text: Some(text.into()),
            ..Default::default()
        }
    }
}

/// `mcp__agentos_hub__fetch_pending_tasks` -> (Some("agentos_hub"), "fetch_pending_tasks").
pub fn split_tool_name(full: &str) -> (Option<String>, String) {
    if let Some(rest) = full.strip_prefix("mcp__") {
        if let Some((server, tool)) = rest.split_once("__") {
            return (Some(server.to_string()), tool.to_string());
        }
    }
    (None, full.to_string())
}

/// Hermes can route MCP calls through its `tool_call` wrapper; show the real target instead.
fn attribute(full_name: &str, input: Option<&Value>) -> (Option<String>, String) {
    if full_name == "tool_call" {
        let calls = input
            .and_then(|i| i.get("calls"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let [only] = calls.as_slice() {
            if let Some(inner) = only.get("name").and_then(Value::as_str) {
                return split_tool_name(inner);
            }
        } else if calls.len() > 1 {
            return (None, format!("tool_call ({} calls)", calls.len()));
        }
    }
    split_tool_name(full_name)
}

fn tool_start(id: &str, full_name: &str, input: Option<Value>) -> ChatEvent {
    let (server, short) = attribute(full_name, input.as_ref());
    ChatEvent {
        kind: "tool_start".into(),
        tool_id: Some(id.into()),
        tool_name: Some(short),
        server,
        input,
        ..Default::default()
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| {
                p.get("text")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .unwrap_or_else(|| p.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// One line of `hermes chat --format stream-json`.
pub fn parse_hermes_line(line: &str) -> Vec<ChatEvent> {
    let t = line.trim();
    if t.is_empty() {
        return vec![];
    }
    let Ok(v) = serde_json::from_str::<Value>(t) else {
        for prefix in ["session_id:", "Session:"] {
            if let Some(id) = t.strip_prefix(prefix) {
                return vec![ChatEvent {
                    kind: "session".into(),
                    session_id: Some(id.trim().into()),
                    ..Default::default()
                }];
            }
        }
        return vec![ChatEvent::text("status", procs::clean_line(t))];
    };
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match v.get("type").and_then(Value::as_str).unwrap_or_default() {
        "system" => {
            let model = s("model");
            let mut e = ChatEvent::text(
                "status",
                if model.is_empty() {
                    "Hermes started".to_string()
                } else {
                    format!("Hermes started on {model}")
                },
            );
            if !s("session_id").is_empty() {
                e.session_id = Some(s("session_id"));
            }
            vec![e]
        }
        "text" | "assistant" | "delta" => vec![ChatEvent::text("token", s("text"))],
        "reasoning" | "thinking" => vec![ChatEvent::text("thinking", s("text"))],
        "tool_use" | "tool_call" => vec![tool_start(
            &s("tool_call_id"),
            &s("name"),
            v.get("input").cloned(),
        )],
        "tool_result" => {
            let (server, short) = split_tool_name(&s("name"));
            vec![ChatEvent {
                kind: "tool_result".into(),
                tool_id: Some(s("tool_call_id")),
                tool_name: Some(short),
                server,
                output: Some(v.get("output").map(value_text).unwrap_or_default()),
                is_error: v.get("is_error").and_then(Value::as_bool),
                duration_ms: v.get("duration_ms").and_then(Value::as_u64),
                ..Default::default()
            }]
        }
        "error" => vec![ChatEvent::text(
            "error",
            v.get("message")
                .or_else(|| v.get("error"))
                .map(value_text)
                .unwrap_or_else(|| t.to_string()),
        )],
        "result" => {
            let code = v.get("exit_code").and_then(Value::as_i64).map(|c| c as i32);
            let mut out = Vec::new();
            if code.is_some_and(|c| c != 0) {
                out.push(ChatEvent::text(
                    "error",
                    v.get("error")
                        .map(value_text)
                        .filter(|e| !e.is_empty())
                        .unwrap_or_else(|| {
                            format!("Hermes exited with code {}", code.unwrap_or(-1))
                        }),
                ));
            }
            out.push(ChatEvent {
                kind: "complete".into(),
                text: v.get("text").map(value_text),
                session_id: Some(s("session_id")).filter(|x| !x.is_empty()),
                tokens: v.get("tokens").cloned(),
                duration_ms: v.get("duration_ms").and_then(Value::as_u64),
                cost_usd: v.get("cost_usd").and_then(Value::as_f64),
                exit_code: code,
                ..Default::default()
            });
            out
        }
        _ => vec![],
    }
}

#[derive(Default)]
pub struct ClaudeParser {
    streamed_text: bool,
}

impl ClaudeParser {
    /// One line of `claude -p --output-format stream-json --verbose --include-partial-messages`.
    pub fn parse(&mut self, line: &str) -> Vec<ChatEvent> {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            let t = line.trim();
            return if t.is_empty() {
                vec![]
            } else {
                vec![ChatEvent::text("status", procs::clean_line(t))]
            };
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "system" => {
                let model = v.get("model").and_then(Value::as_str).unwrap_or_default();
                let mut e = ChatEvent::text("status", format!("Claude Code started on {model}"));
                e.session_id = v
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(String::from);
                vec![e]
            }
            "stream_event" => {
                let ev = v.get("event").cloned().unwrap_or(Value::Null);
                if ev.get("type").and_then(Value::as_str) != Some("content_block_delta") {
                    return vec![];
                }
                let delta = ev.get("delta").cloned().unwrap_or(Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        self.streamed_text = true;
                        vec![ChatEvent::text(
                            "token",
                            delta
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        )]
                    }
                    Some("thinking_delta") => vec![ChatEvent::text(
                        "thinking",
                        delta
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )],
                    _ => vec![],
                }
            }
            "assistant" => {
                let mut out = Vec::new();
                let streamed = std::mem::take(&mut self.streamed_text);
                for block in v
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") if !streamed => out.push(ChatEvent::text(
                            "token",
                            block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        )),
                        Some("tool_use") => out.push(tool_start(
                            block.get("id").and_then(Value::as_str).unwrap_or_default(),
                            block.get("name").and_then(Value::as_str).unwrap_or("tool"),
                            block.get("input").cloned(),
                        )),
                        _ => {}
                    }
                }
                out
            }
            "user" => v
                .pointer("/message/content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                .map(|b| ChatEvent {
                    kind: "tool_result".into(),
                    tool_id: b
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .map(String::from),
                    output: Some(b.get("content").map(value_text).unwrap_or_default()),
                    is_error: b.get("is_error").and_then(Value::as_bool),
                    ..Default::default()
                })
                .collect(),
            "result" => {
                let mut out = Vec::new();
                let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                if is_error {
                    out.push(ChatEvent::text(
                        "error",
                        v.get("result")
                            .map(value_text)
                            .unwrap_or_else(|| "Claude Code reported an error".into()),
                    ));
                }
                out.push(ChatEvent {
                    kind: "complete".into(),
                    text: v.get("result").map(value_text),
                    session_id: v
                        .get("session_id")
                        .and_then(Value::as_str)
                        .map(String::from),
                    tokens: v.get("usage").cloned(),
                    duration_ms: v.get("duration_ms").and_then(Value::as_u64),
                    cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
                    exit_code: Some(if is_error { 1 } else { 0 }),
                    ..Default::default()
                });
                out
            }
            _ => vec![],
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct ChatRequest {
    pub session_id: Option<String>,
    pub prompt: String,
    pub working_dir: String,
    pub agent_backend: String,
    pub model: Option<String>,
    /// Skills preloaded for this turn (`hermes chat -s a,b`).
    #[serde(default)]
    pub skills: Vec<String>,
    /// 1-based user turn in this chat; the ledger attaches rows to it.
    #[serde(default)]
    pub turn: Option<u32>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ChatStarted {
    pub run_id: String,
    pub backend: String,
    pub working_dir: String,
    pub pid: u32,
}

struct ActiveChat {
    run_id: String,
    pid: u32,
    stopped: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct ChatState {
    active: Arc<Mutex<Option<ActiveChat>>>,
}

pub fn normalize_backend(b: &str) -> Result<&'static str> {
    match b.trim().to_lowercase().as_str() {
        "" | "auto" | "hermes" => Ok("hermes"),
        "claude" | "claude-code" => Ok("claude-code"),
        other => bail!("Unknown chat backend '{other}' (use hermes or claude-code)"),
    }
}

/// Build the argument list for one chat turn.
pub fn build_args(backend: &str, req: &ChatRequest, query_file: &Path, cwd: &Path) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let session = req
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let model = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if backend == "hermes" {
        a.extend(
            [
                "chat",
                "--format",
                "stream-json",
                "--max-turns",
                "90",
                "--source",
                STUDIO_SOURCE,
            ]
            .map(String::from),
        );
        a.push("--query-file".into());
        a.push(query_file.display().to_string());
        a.push("--in".into());
        a.push(cwd.display().to_string());
        // A resumed session keeps the working directory it was created in (Hermes gateway rule).
        if let Some(sid) = session {
            a.extend(["--resume".into(), sid.into()]);
        }
        if let Some(m) = model {
            a.extend(["-m".into(), m.into()]);
        }
        let skills: Vec<String> = req
            .skills
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !skills.is_empty() {
            a.extend(["-s".into(), skills.join(",")]);
        }
    } else {
        a.extend(
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "mcp__agentos-hub",
            ]
            .map(String::from),
        );
        if let Some(sid) = session {
            a.extend(["--resume".into(), sid.into()]);
        }
        if let Some(m) = model {
            a.extend(["--model".into(), m.into()]);
        }
    }
    a
}

fn emit(app: &AppHandle, run_id: &str, mut e: ChatEvent) {
    e.run_id = run_id.to_string();
    if e.kind.starts_with("tool") {
        if let Some(name) = e.tool_name.as_deref() {
            crate::memory_ledger::note_event(run_id, name, e.input.as_ref(), e.output.as_deref());
        }
    }
    if matches!(e.kind.as_str(), "done" | "end" | "finished" | "exit") {
        crate::memory_ledger::end_run(run_id);
    }
    let _ = app.emit(EV_CHAT, e);
}

pub fn send_chat_message(
    app: AppHandle,
    state: &ChatState,
    req: ChatRequest,
) -> Result<ChatStarted> {
    let backend = normalize_backend(&req.agent_backend)?;
    if req.prompt.trim().is_empty() {
        bail!("Type a message first");
    }
    {
        let guard = state
            .active
            .lock()
            .map_err(|_| anyhow!("chat lock poisoned"))?;
        if guard.is_some() {
            bail!("A reply is still streaming. Stop it first.");
        }
    }
    let cwd = PathBuf::from(req.working_dir.trim());
    let cwd = if cwd.is_dir() {
        cwd
    } else {
        crate::settings::home()
    };
    let run_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let ledger_session = req
        .session_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("run-{run_id}"));
    crate::memory_ledger::begin_run(&run_id, &ledger_session, req.turn.unwrap_or(1));
    let dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("agentos-studio")
        .join("chat");
    fs::create_dir_all(&dir)?;
    let query_file = dir.join(format!("{run_id}.md"));
    fs::write(&query_file, &req.prompt)?;
    let args = build_args(backend, &req, &query_file, &cwd);
    let tool = if backend == "hermes" {
        "hermes"
    } else {
        "claude"
    };
    let exe = procs::find(tool)
        .ok_or_else(|| anyhow!("{tool} is not installed. Open Setup to install it."))?;
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&args)
        .current_dir(&cwd)
        .env("PATH", procs::search_path())
        .env("PYTHONIOENCODING", "utf-8")
        .env("NO_COLOR", "1")
        .stdin(if backend == "claude-code" {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(false);
    #[cfg(windows)]
    cmd.creation_flags(procs::CREATE_NO_WINDOW);
    let mut child = cmd.spawn().with_context(|| format!("starting {tool}"))?;
    let pid = child.id().unwrap_or(0);
    let stopped = Arc::new(AtomicBool::new(false));
    *state
        .active
        .lock()
        .map_err(|_| anyhow!("chat lock poisoned"))? = Some(ActiveChat {
        run_id: run_id.clone(),
        pid,
        stopped: stopped.clone(),
    });
    if backend == "claude-code" {
        if let Some(mut stdin) = child.stdin.take() {
            let prompt = req.prompt.clone();
            tauri::async_runtime::spawn(async move {
                let _ = stdin.write_all(prompt.as_bytes()).await;
                let _ = stdin.shutdown().await;
            });
        }
    }
    emit(
        &app,
        &run_id,
        ChatEvent::text(
            "status",
            format!(
                "{} is working in {}",
                if backend == "hermes" {
                    "Hermes"
                } else {
                    "Claude Code"
                },
                cwd.display()
            ),
        ),
    );

    let completed = Arc::new(AtomicBool::new(false));
    let session_seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let tool_clock: Arc<Mutex<HashMap<String, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let out_task = child.stdout.take().map(|out| {
        let (app, rid, completed, session_seen, tool_clock) = (
            app.clone(),
            run_id.clone(),
            completed.clone(),
            session_seen.clone(),
            tool_clock.clone(),
        );
        tauri::async_runtime::spawn(async move {
            let mut claude = ClaudeParser::default();
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let events = if backend == "hermes" {
                    parse_hermes_line(&line)
                } else {
                    claude.parse(&line)
                };
                for mut e in events {
                    match e.kind.as_str() {
                        "tool_start" => {
                            if let (Some(id), Ok(mut c)) = (e.tool_id.clone(), tool_clock.lock()) {
                                c.insert(id, Instant::now());
                            }
                        }
                        "tool_result" if e.duration_ms.is_none() => {
                            if let (Some(id), Ok(c)) = (e.tool_id.as_ref(), tool_clock.lock()) {
                                e.duration_ms = c.get(id).map(|t| t.elapsed().as_millis() as u64);
                            }
                        }
                        "complete" => completed.store(true, Ordering::SeqCst),
                        _ => {}
                    }
                    if let Some(sid) = e.session_id.clone() {
                        if let Ok(mut s) = session_seen.lock() {
                            *s = Some(sid);
                        }
                    }
                    emit(&app, &rid, e);
                }
            }
        })
    });
    let err_task = child.stderr.take().map(|err| {
        let (app, rid, session_seen, stderr_tail) = (
            app.clone(),
            run_id.clone(),
            session_seen.clone(),
            stderr_tail.clone(),
        );
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(raw)) = lines.next_line().await {
                let line = procs::clean_line(&raw);
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                if let Some(id) = t
                    .strip_prefix("session_id:")
                    .or_else(|| t.strip_prefix("Session:"))
                {
                    if let Ok(mut s) = session_seen.lock() {
                        *s = Some(id.trim().to_string());
                    }
                    emit(
                        &app,
                        &rid,
                        ChatEvent {
                            kind: "session".into(),
                            session_id: Some(id.trim().into()),
                            ..Default::default()
                        },
                    );
                    continue;
                }
                if let Ok(mut tail) = stderr_tail.lock() {
                    tail.push(t.to_string());
                    let n = tail.len();
                    if n > 12 {
                        tail.drain(..n - 12);
                    }
                }
                emit(&app, &rid, ChatEvent::text("stderr", t));
            }
        })
    });

    let active = state.active.clone();
    let rid = run_id.clone();
    tauri::async_runtime::spawn(async move {
        let status = child.wait().await;
        if let Some(t) = out_task {
            let _ = t.await;
        }
        if let Some(t) = err_task {
            let _ = t.await;
        }
        let _ = fs::remove_file(&query_file);
        let code = status.ok().and_then(|s| s.code());
        let was_stopped = stopped.load(Ordering::SeqCst);
        if let Ok(mut a) = active.lock() {
            if a.as_ref().is_some_and(|c| c.run_id == rid) {
                *a = None;
            }
        }
        if was_stopped {
            emit(
                &app,
                &rid,
                ChatEvent {
                    kind: "complete".into(),
                    stopped: Some(true),
                    exit_code: code,
                    session_id: session_seen.lock().ok().and_then(|s| s.clone()),
                    ..Default::default()
                },
            );
            return;
        }
        if !completed.load(Ordering::SeqCst) {
            if code != Some(0) {
                let tail = stderr_tail.lock().map(|t| t.join("\n")).unwrap_or_default();
                emit(
                    &app,
                    &rid,
                    ChatEvent::text(
                        "error",
                        if tail.is_empty() {
                            format!("The agent exited with code {code:?}")
                        } else {
                            tail
                        },
                    ),
                );
            }
            emit(
                &app,
                &rid,
                ChatEvent {
                    kind: "complete".into(),
                    exit_code: code,
                    session_id: session_seen.lock().ok().and_then(|s| s.clone()),
                    ..Default::default()
                },
            );
        }
    });

    Ok(ChatStarted {
        run_id,
        backend: backend.into(),
        working_dir: cwd.display().to_string(),
        pid,
    })
}

pub fn stop_active_chat(state: &ChatState) -> Result<String> {
    let guard = state
        .active
        .lock()
        .map_err(|_| anyhow!("chat lock poisoned"))?;
    let Some(active) = guard.as_ref() else {
        bail!("Nothing is streaming");
    };
    active.stopped.store(true, Ordering::SeqCst);
    procs::kill_tree(active.pid);
    Ok(active.run_id.clone())
}

pub fn active_run(state: &ChatState) -> Option<String> {
    state
        .active
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|a| a.run_id.clone()))
}

/// Resolve `rel` inside `root`, refusing anything that escapes it.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.trim().replace('\\', "/");
    if rel.is_empty() {
        bail!("Give a file path");
    }
    let candidate = Path::new(&rel);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let mut clean = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::ParentDir => {
                if !clean.pop() {
                    bail!("Path escapes the working directory");
                }
            }
            Component::CurDir => {}
            other => clean.push(other.as_os_str()),
        }
    }
    let root_clean: PathBuf = root
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    let norm = |p: &Path| p.display().to_string().replace('\\', "/").to_lowercase();
    if !norm(&clean).starts_with(&format!("{}/", norm(&root_clean).trim_end_matches('/'))) {
        bail!("{} is outside {}", clean.display(), root.display());
    }
    Ok(clean)
}

#[derive(Serialize, Clone, Debug)]
pub struct ApplyResult {
    pub path: String,
    pub created: bool,
    pub bytes: usize,
}

/// Write a code block from the chat into a file under the working directory.
pub fn apply_code_to_file(working_dir: &str, rel_path: &str, content: &str) -> Result<ApplyResult> {
    let root = PathBuf::from(working_dir.trim());
    if !root.is_dir() {
        bail!("Working directory not found: {}", root.display());
    }
    let path = safe_join(&root, rel_path)?;
    let created = !path.exists();
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let mut text = content.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if let Ok(existing) = fs::read_to_string(&path) {
        if existing.contains("\r\n") {
            text = text.replace("\r\n", "\n").replace('\n', "\r\n");
        }
    }
    fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
    Ok(ApplyResult {
        path: path.display().to_string(),
        created,
        bytes: text.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_mcp_tool_names() {
        assert_eq!(
            split_tool_name("mcp__agentos_hub__fetch_pending_tasks"),
            (Some("agentos_hub".into()), "fetch_pending_tasks".into())
        );
        assert_eq!(split_tool_name("terminal"), (None, "terminal".into()));
        assert_eq!(split_tool_name("mcp__broken"), (None, "mcp__broken".into()));
    }

    #[test]
    fn attributes_wrapped_mcp_calls() {
        let e = parse_hermes_line(
            r#"{"type":"tool_use","name":"tool_call","tool_call_id":"t1","input":{"calls":[{"name":"mcp__notion__notion_search","arguments":{"query":"x"}}]}}"#,
        );
        assert_eq!(e[0].server.as_deref(), Some("notion"));
        assert_eq!(e[0].tool_name.as_deref(), Some("notion_search"));
        let many = parse_hermes_line(
            r#"{"type":"tool_use","name":"tool_call","tool_call_id":"t2","input":{"calls":[{"name":"a"},{"name":"b"}]}}"#,
        );
        assert_eq!(many[0].tool_name.as_deref(), Some("tool_call (2 calls)"));
        let plain = parse_hermes_line(
            r#"{"type":"tool_use","name":"tool_describe","tool_call_id":"t3","input":{}}"#,
        );
        assert_eq!(
            (plain[0].server.clone(), plain[0].tool_name.as_deref()),
            (None, Some("tool_describe"))
        );
    }

    #[test]
    fn parses_real_hermes_stream() {
        let lines = [
            r#"{"type": "system", "subtype": "init", "model": "", "session_id": "", "timestamp": 1}"#,
            r#"{"type": "tool_use", "name": "mcp__agentos_hub__fetch_pending_tasks", "tool_call_id": "toolu_01", "input": {"agent_name": "all"}, "timestamp": 2}"#,
            r#"{"type": "tool_result", "name": "mcp__agentos_hub__fetch_pending_tasks", "tool_call_id": "toolu_01", "output": "{\"result\": \"[]\"}", "duration_ms": 11, "is_error": false, "timestamp": 3}"#,
            r#"{"type": "text", "text": "\n\nOK 0", "timestamp": 4}"#,
            r#"{"type": "result", "session_id": "local-834", "exit_code": 0, "text": "OK 0", "tokens": {"input": 48826, "output": 107}, "duration_ms": 8613, "timestamp": 5}"#,
        ];
        let ev: Vec<ChatEvent> = lines.iter().flat_map(|l| parse_hermes_line(l)).collect();
        let kinds: Vec<&str> = ev.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["status", "tool_start", "tool_result", "token", "complete"]
        );
        assert_eq!(ev[1].server.as_deref(), Some("agentos_hub"));
        assert_eq!(ev[1].tool_name.as_deref(), Some("fetch_pending_tasks"));
        assert_eq!(ev[1].input.as_ref().unwrap()["agent_name"], "all");
        assert_eq!(ev[2].duration_ms, Some(11));
        assert_eq!(ev[2].is_error, Some(false));
        assert_eq!(ev[3].text.as_deref(), Some("\n\nOK 0"));
        assert_eq!(ev[4].session_id.as_deref(), Some("local-834"));
        assert_eq!(ev[4].tokens.as_ref().unwrap()["output"], 107);
        assert_eq!(
            parse_hermes_line("session_id: local-9")[0]
                .session_id
                .as_deref(),
            Some("local-9")
        );
        assert!(parse_hermes_line("   ").is_empty());
        let failed =
            parse_hermes_line(r#"{"type":"result","exit_code":2,"error":"model not found"}"#);
        assert_eq!(failed[0].kind, "error");
        assert_eq!(failed[0].text.as_deref(), Some("model not found"));
        assert_eq!(failed[1].exit_code, Some(2));
    }

    #[test]
    fn parses_claude_stream_without_duplicate_text() {
        let mut p = ClaudeParser::default();
        let lines = [
            r#"{"type":"system","subtype":"init","session_id":"c-1","model":"claude-sonnet-4-6"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"},{"type":"tool_use","id":"tu_1","name":"mcp__agentos-hub__search_shared_memory","input":{"query":"fleet"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_1","content":[{"type":"text","text":"3 hits"}],"is_error":false}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Hello Done.","session_id":"c-1","duration_ms":900,"total_cost_usd":0.01}"#,
        ];
        let ev: Vec<ChatEvent> = lines.iter().flat_map(|l| p.parse(l)).collect();
        let kinds: Vec<&str> = ev.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec![
                "status",
                "token",
                "token",
                "tool_start",
                "tool_result",
                "token",
                "complete"
            ]
        );
        assert_eq!(ev[0].session_id.as_deref(), Some("c-1"));
        assert_eq!(ev[3].server.as_deref(), Some("agentos-hub"));
        assert_eq!(ev[4].output.as_deref(), Some("3 hits"));
        assert_eq!(
            ev[5].text.as_deref(),
            Some("Done."),
            "non-streamed text is kept"
        );
        assert_eq!(ev[6].cost_usd, Some(0.01));
    }

    #[test]
    fn builds_resume_args_for_both_backends() {
        let req = ChatRequest {
            turn: None,
            session_id: Some("local-1".into()),
            prompt: "hi".into(),
            working_dir: "C:/repo".into(),
            agent_backend: "hermes".into(),
            model: Some("claude-opus-5-5[1m]".into()),
            skills: vec!["agentos-studio-dev".into(), "obsidian".into()],
        };
        let h = build_args("hermes", &req, Path::new("C:/q.md"), Path::new("C:/repo"));
        let joined = h.join(" ");
        assert!(joined.starts_with("chat --format stream-json"));
        assert!(joined.contains("--resume local-1"));
        assert!(
            !joined.contains("--no-restore-cwd"),
            "the gateway rejects it"
        );
        assert!(joined.contains("--in C:/repo"));
        assert!(joined.contains("-m claude-opus-5-5[1m]"));
        assert!(
            joined.contains("-s agentos-studio-dev,obsidian"),
            "{joined}"
        );
        let c = build_args(
            "claude-code",
            &ChatRequest {
                model: None,
                session_id: None,
                ..req
            },
            Path::new("q"),
            Path::new("C:/repo"),
        );
        assert!(c.contains(&"--include-partial-messages".to_string()));
        assert!(!c.contains(&"--resume".to_string()));
        assert_eq!(normalize_backend("Claude").unwrap(), "claude-code");
        assert!(normalize_backend("qoder").is_err());
    }

    #[test]
    fn safe_join_blocks_escapes_and_apply_writes() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        assert!(safe_join(root, "../outside.txt").is_err());
        assert!(safe_join(root, "a/../../x").is_err());
        assert!(safe_join(root, "").is_err());
        assert!(safe_join(root, "C:/Windows/system.ini").is_err() || !cfg!(windows));
        let ok = safe_join(root, "src/./lib.rs").unwrap();
        assert!(ok.ends_with("src/lib.rs") || ok.ends_with("src\\lib.rs"));
        let r =
            apply_code_to_file(&root.display().to_string(), "src/new.rs", "fn main() {}").unwrap();
        assert!(r.created);
        assert_eq!(
            fs::read_to_string(root.join("src/new.rs")).unwrap(),
            "fn main() {}\n"
        );
        fs::write(root.join("crlf.txt"), "a\r\nb\r\n").unwrap();
        let r2 = apply_code_to_file(&root.display().to_string(), "crlf.txt", "x\ny").unwrap();
        assert!(!r2.created);
        assert_eq!(
            fs::read_to_string(root.join("crlf.txt")).unwrap(),
            "x\r\ny\r\n"
        );
    }
}
