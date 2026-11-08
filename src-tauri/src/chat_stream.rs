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
use std::time::{Duration, Instant};

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
    /// Provider for `model` (`hermes chat --provider`), set from a folder default.
    #[serde(default)]
    pub provider: Option<String>,
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
    /// Set for an ACP turn: Stop sends `session/cancel` instead of ending a process.
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

type PermissionReply = tokio::sync::oneshot::Sender<Option<String>>;

#[derive(Default, Clone)]
pub struct ChatState {
    active: Arc<Mutex<Option<ActiveChat>>>,
    /// Approval questions waiting for the user, by request id.
    pending: Arc<Mutex<HashMap<String, PermissionReply>>>,
}

pub fn normalize_backend(b: &str) -> Result<&'static str> {
    match b.trim().to_lowercase().as_str() {
        "" | "auto" | "hermes" => Ok("hermes"),
        "claude" | "claude-code" => Ok("claude-code"),
        other => bail!("Unknown chat backend '{other}' (use hermes or claude-code)"),
    }
}

fn launch_skills_key(session_id: &str) -> String {
    format!("chat.launch_skills.{session_id}")
}

/// Skills a Hermes session was created with (`-s`), as Studio recorded them. `None` for a session
/// Studio did not create, which is treated as having every requested skill already.
pub fn launch_skills(session_id: &str) -> Option<Vec<String>> {
    let c = crate::store::open().ok()?;
    let raw = crate::store::kv_get(&c, &launch_skills_key(session_id)).ok()??;
    serde_json::from_str(&raw).ok()
}

fn save_launch_skills(session_id: &str, skills: &[String]) {
    if let (Ok(c), Ok(v)) = (crate::store::open(), serde_json::to_string(skills)) {
        let _ = crate::store::kv_set(&c, &launch_skills_key(session_id), &v);
    }
}

/// A resumed Hermes turn cannot add `-s` (Hermes refuses changed creation flags), so a skill
/// picked after the chat started is invoked the way Hermes documents: `/<skill> <message>`.
/// One skill per turn; the rest follow on the next turns. Returns the prompt to send and the
/// skill set the session will have loaded afterwards. Pure, so it is unit tested.
pub fn hermes_resume_prompt(
    prompt: &str,
    requested: &[String],
    launched: Option<&[String]>,
) -> (String, Vec<String>) {
    let Some(launched) = launched else {
        return (prompt.to_string(), requested.to_vec());
    };
    let mut loaded = launched.to_vec();
    match requested
        .iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty() && !launched.iter().any(|l| l == s))
    {
        Some(new) if !prompt.trim_start().starts_with('/') => {
            loaded.push(new.to_string());
            (format!("/{new} {prompt}"), loaded)
        }
        _ => (prompt.to_string(), loaded),
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
        if let Some(sid) = session {
            // A resumed session keeps the policy it was created with: Hermes refuses a resume
            // whose -m, -s or --checkpoints differ ("creation overrides are unsupported on
            // resume"). Model and skill changes go through slash commands instead
            // (`hermes_resume_prompt`). It also keeps its original working directory.
            a.extend(["--resume".into(), sid.into()]);
        } else {
            // New Hermes chats snapshot files before edits, so a turn can be undone with
            // /rollback in the Hermes TUI.
            a.push("--checkpoints".into());
            if let Some(m) = model {
                a.extend(["-m".into(), m.into()]);
                if let Some(p) = req
                    .provider
                    .as_deref()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                {
                    a.extend(["--provider".into(), p.into()]);
                }
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
    // A new Hermes chat with no explicit model takes the folder's default (Providers > Folder
    // defaults). Resumed chats keep the model they were created with.
    let mut req = req;
    if backend == "hermes"
        && req
            .session_id
            .as_deref()
            .map_or(true, |s| s.trim().is_empty())
        && req.model.as_deref().map_or(true, |m| m.trim().is_empty())
    {
        let defaults = crate::model_layer::folder_models();
        if let Some(d) = crate::model_layer::folder_default(&defaults, &cwd.display().to_string()) {
            req.model = Some(d.model.clone());
            req.provider = Some(d.provider.clone());
        }
    }
    let resumed = req.session_id.clone().filter(|s| !s.trim().is_empty());
    let prompt = match (&resumed, backend) {
        (Some(sid), "hermes") => {
            let launched = launch_skills(sid);
            let (p, loaded) = hermes_resume_prompt(&req.prompt, &req.skills, launched.as_deref());
            if launched.is_some() {
                save_launch_skills(sid, &loaded);
            }
            p
        }
        _ => req.prompt.clone(),
    };
    fs::write(&query_file, &prompt)?;
    // Hermes turns go over ACP (approvals asked, real cancel, reasoning) unless the user chose
    // `chat -q` or the prompt is a slash command, which ACP would hand to the model as text.
    if backend == "hermes" && crate::acp_host::enabled() && !prompt.trim_start().starts_with('/') {
        let stopped = Arc::new(AtomicBool::new(false));
        *state
            .active
            .lock()
            .map_err(|_| anyhow!("chat lock poisoned"))? = Some(ActiveChat {
            run_id: run_id.clone(),
            pid: 0,
            stopped: stopped.clone(),
            cancel: None,
        });
        emit(
            &app,
            &run_id,
            ChatEvent::text("status", format!("Hermes is working in {}", cwd.display())),
        );
        let (app2, state2, rid) = (app.clone(), state.clone(), run_id.clone());
        let cwd2 = cwd.clone();
        tauri::async_runtime::spawn(async move {
            acp_turn(app2, state2, rid, req, prompt, cwd2, query_file, stopped).await;
        });
        return Ok(ChatStarted {
            run_id,
            backend: backend.into(),
            working_dir: cwd.display().to_string(),
            pid: 0,
        });
    }
    spawn_cli(
        &app,
        state,
        &req,
        backend,
        &cwd,
        &run_id,
        query_file,
        resumed.is_none(),
    )
}

/// One Hermes turn over ACP. Falls back to `hermes chat -q` when the session cannot be created
/// or loaded, before anything was sent, so a broken ACP path never loses a message.
#[allow(clippy::too_many_arguments)]
async fn acp_turn(
    app: AppHandle,
    state: ChatState,
    run_id: String,
    mut req: ChatRequest,
    prompt: String,
    cwd: PathBuf,
    query_file: PathBuf,
    stopped: Arc<AtomicBool>,
) {
    // The whole turn as the user waits for it: session creation, load, and the reply.
    let started = Instant::now();
    let took = || Some(started.elapsed().as_millis() as u64);
    let fallback = |app: &AppHandle, state: &ChatState, req: &ChatRequest, why: String| {
        emit(
            app,
            &run_id,
            ChatEvent::text(
                "status",
                format!("ACP unavailable ({why}); using hermes chat -q"),
            ),
        );
        let new = req
            .session_id
            .as_deref()
            .map_or(true, |s| s.trim().is_empty());
        if let Err(e) = spawn_cli(
            app,
            state,
            req,
            "hermes",
            &cwd,
            &run_id,
            query_file.clone(),
            new,
        ) {
            emit(app, &run_id, ChatEvent::text("error", e.to_string()));
            finish(app, state, &run_id, None, false);
        }
    };
    let sid = match req.session_id.clone().filter(|s| !s.trim().is_empty()) {
        Some(s) => s,
        None => {
            let (c, m, p, sk) = (
                cwd.clone(),
                req.model.clone(),
                req.provider.clone(),
                req.skills.clone(),
            );
            match tauri::async_runtime::spawn_blocking(move || {
                crate::acp_host::create_session(&c, m.as_deref(), p.as_deref(), &sk)
            })
            .await
            {
                Ok(Ok(s)) => {
                    save_launch_skills(&s, &req.skills);
                    crate::memory_ledger::rename_run(&run_id, &s);
                    emit(
                        &app,
                        &run_id,
                        ChatEvent {
                            kind: "session".into(),
                            session_id: Some(s.clone()),
                            ..Default::default()
                        },
                    );
                    // From here on this is a resume: the session already has its policy.
                    req.session_id = Some(s.clone());
                    s
                }
                Ok(Err(e)) => return fallback(&app, &state, &req, e.to_string()),
                Err(e) => return fallback(&app, &state, &req, e.to_string()),
            }
        }
    };
    if stopped.load(Ordering::SeqCst) {
        let _ = fs::remove_file(&query_file);
        return finish_timed(&app, &state, &run_id, took(), Some(sid), true);
    }
    let mut turn = match crate::acp_host::start_turn(&sid, &cwd, &prompt).await {
        Ok(t) => t,
        Err(e) => return fallback(&app, &state, &req, e.to_string()),
    };
    let _ = fs::remove_file(&query_file);
    if let Ok(mut a) = state.active.lock() {
        if let Some(c) = a.as_mut().filter(|c| c.run_id == run_id) {
            c.cancel = Some(turn.cancel);
        }
    }
    // A Stop pressed while the session was being created or loaded applies now.
    if stopped.load(Ordering::SeqCst) {
        if let Some(c) = state
            .active
            .lock()
            .ok()
            .and_then(|mut a| a.as_mut().and_then(|c| c.cancel.take()))
        {
            let _ = c.send(());
        }
    }
    let mut clock: HashMap<String, Instant> = HashMap::new();
    let handle = |e: crate::acp::AcpEvent, clock: &mut HashMap<String, Instant>| {
        for ev in acp_chat_events(e, clock) {
            emit(&app, &run_id, ev);
        }
    };
    let outcome = loop {
        tokio::select! {
            Some(e) = turn.events.recv() => handle(e, &mut clock),
            Some(p) = turn.permissions.recv() => {
                let deny = timeout_choice(&p.options);
                let (rid, title) = (p.request_id.clone(), p.title.clone());
                if let Ok(mut m) = state.pending.lock() {
                    m.insert(p.request_id.clone(), p.reply);
                }
                let (app2, pending, run) = (app.clone(), state.pending.clone(), run_id.clone());
                let secs = approval_timeout_secs();
                tauri::async_runtime::spawn(async move {
                    if expire_after(&pending, &rid, Duration::from_secs(secs), deny).await {
                        let msg = format!(
                            "Denied after timeout: nobody answered within {} ({title})",
                            human_secs(secs)
                        );
                        emit(&app2, &run, ChatEvent { kind: "permission_closed".into(), tool_id: Some(rid), ..Default::default() });
                        emit(&app2, &run, ChatEvent::text("error", msg.clone()));
                        use tauri_plugin_notification::NotificationExt;
                        let _ = app2
                            .notification()
                            .builder()
                            .title("Hermes approval denied after timeout")
                            .body(msg)
                            .show();
                    }
                });
            }
            r = &mut turn.done => break r,
        }
    };
    while let Ok(e) = turn.events.try_recv() {
        handle(e, &mut clock);
    }
    if let Ok(mut m) = state.pending.lock() {
        m.clear();
    }
    match outcome {
        Ok(Ok(stop)) => {
            let was_stopped = stop == "cancelled" || stopped.load(Ordering::SeqCst);
            if stop == "refusal" {
                emit(
                    &app,
                    &run_id,
                    ChatEvent::text("error", "Hermes refused this turn"),
                );
            }
            finish_timed(&app, &state, &run_id, took(), Some(sid), was_stopped);
        }
        Ok(Err(e)) => {
            if !stopped.load(Ordering::SeqCst) {
                emit(
                    &app,
                    &run_id,
                    ChatEvent::text("error", format!("Hermes: {e}")),
                );
            }
            finish_timed(
                &app,
                &state,
                &run_id,
                took(),
                Some(sid),
                stopped.load(Ordering::SeqCst),
            );
        }
        Err(_) => {
            if !stopped.load(Ordering::SeqCst) {
                emit(
                    &app,
                    &run_id,
                    ChatEvent::text("error", "The Hermes ACP connection closed during the turn"),
                );
            }
            finish_timed(
                &app,
                &state,
                &run_id,
                took(),
                Some(sid),
                stopped.load(Ordering::SeqCst),
            );
        }
    }
}

fn finish(app: &AppHandle, state: &ChatState, run_id: &str, sid: Option<String>, stopped: bool) {
    finish_timed(app, state, run_id, None, sid, stopped)
}

fn finish_timed(
    app: &AppHandle,
    state: &ChatState,
    run_id: &str,
    duration_ms: Option<u64>,
    sid: Option<String>,
    stopped: bool,
) {
    if let Ok(mut a) = state.active.lock() {
        if a.as_ref().is_some_and(|c| c.run_id == run_id) {
            *a = None;
        }
    }
    emit(app, run_id, complete_event(sid, stopped, duration_ms));
}

/// The `complete` event for an ACP turn. Pure.
pub fn complete_event(sid: Option<String>, stopped: bool, duration_ms: Option<u64>) -> ChatEvent {
    ChatEvent {
        kind: "complete".into(),
        stopped: stopped.then_some(true),
        session_id: sid,
        duration_ms,
        ..Default::default()
    }
}

/// ACP updates as the chat events the UI already renders. Built-in tools arrive as a title like
/// `terminal: rm -rf x` with no structured input, so the title's detail becomes the card input.
pub fn acp_chat_events(
    e: crate::acp::AcpEvent,
    clock: &mut HashMap<String, Instant>,
) -> Vec<ChatEvent> {
    use crate::acp::AcpEvent as A;
    match e {
        A::Text { text } => vec![ChatEvent::text("token", text)],
        A::Thought { text } => vec![ChatEvent::text("thinking", text)],
        A::ToolCall {
            id, title, status, ..
        } => {
            clock.insert(id.clone(), Instant::now());
            let (name, detail) = match title.split_once(": ") {
                Some((n, d)) if !n.contains(' ') => (n.to_string(), Some(d.to_string())),
                _ => (title.clone(), None),
            };
            let mut out = vec![tool_start(
                &id,
                &name,
                detail.map(|d| serde_json::json!({ "detail": d })),
            )];
            if status == "completed" || status == "failed" {
                out.push(ChatEvent {
                    kind: "tool_result".into(),
                    tool_id: Some(id),
                    is_error: Some(status == "failed"),
                    output: Some(String::new()),
                    ..Default::default()
                });
            }
            out
        }
        A::ToolUpdate { id, status, output } if status == "completed" || status == "failed" => {
            vec![ChatEvent {
                kind: "tool_result".into(),
                duration_ms: clock.get(&id).map(|t| t.elapsed().as_millis() as u64),
                tool_id: Some(id),
                is_error: Some(status == "failed"),
                output: Some(output),
                ..Default::default()
            }]
        }
        A::Plan { entries } if !entries.is_empty() => vec![ChatEvent::text(
            "status",
            format!("Plan: {}", entries.join(" / ")),
        )],
        A::Permission {
            request_id,
            title,
            options,
        } => vec![ChatEvent {
            kind: "permission".into(),
            tool_id: Some(request_id),
            text: Some(title),
            input: serde_json::to_value(options).ok(),
            ..Default::default()
        }],
        _ => vec![],
    }
}

/// `studio.db` kv: seconds an approval may wait before Studio denies it. Default 10 minutes.
pub const KV_APPROVAL_TIMEOUT: &str = "chat.approval_timeout_secs";
pub const DEFAULT_APPROVAL_TIMEOUT: u64 = 600;

/// Clamp to 10 s .. 24 h; anything unreadable is the default. Pure.
pub fn parse_timeout(v: Option<&str>) -> u64 {
    v.and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_APPROVAL_TIMEOUT)
        .clamp(10, 86_400)
}

pub fn approval_timeout_secs() -> u64 {
    let v = crate::store::open()
        .ok()
        .and_then(|c| crate::store::kv_get(&c, KV_APPROVAL_TIMEOUT).ok().flatten());
    parse_timeout(v.as_deref())
}

pub fn set_approval_timeout_secs(secs: u64) -> Result<u64> {
    let v = parse_timeout(Some(&secs.to_string()));
    let c = crate::store::open()?;
    crate::store::kv_set(&c, KV_APPROVAL_TIMEOUT, &v.to_string())?;
    Ok(v)
}

/// "10 min", "45 s". Pure.
pub fn human_secs(s: u64) -> String {
    if s >= 60 && s % 60 == 0 {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

/// The option sent when an approval times out: deny this once, never "deny always", which would
/// change Hermes' policy on the owner's behalf. `None` (Cancelled) also means deny. Pure.
pub fn timeout_choice(options: &[crate::acp::PermissionChoice]) -> Option<String> {
    options
        .iter()
        .find(|o| o.kind == "reject_once")
        .map(|o| o.id.clone())
}

/// After `wait`, deny the request if it is still unanswered. True when it expired.
pub async fn expire_after(
    pending: &Arc<Mutex<HashMap<String, PermissionReply>>>,
    request_id: &str,
    wait: Duration,
    deny: Option<String>,
) -> bool {
    tokio::time::sleep(wait).await;
    let reply = pending.lock().ok().and_then(|mut m| m.remove(request_id));
    match reply {
        Some(r) => {
            let _ = r.send(deny);
            true
        }
        None => false,
    }
}

/// Answer a Hermes approval question: an option id from the `permission` event, or `None` to
/// cancel it (Hermes treats that as a denial).
pub fn answer_permission(
    state: &ChatState,
    request_id: &str,
    option_id: Option<String>,
) -> Result<()> {
    let reply = state
        .pending
        .lock()
        .map_err(|_| anyhow!("chat lock poisoned"))?
        .remove(request_id)
        .ok_or_else(|| anyhow!("That approval is no longer waiting"))?;
    let _ = reply.send(option_id);
    Ok(())
}

/// Start `hermes chat -q` or `claude -p` for one turn.
#[allow(clippy::too_many_arguments)]
fn spawn_cli(
    app: &AppHandle,
    state: &ChatState,
    req: &ChatRequest,
    backend: &'static str,
    cwd: &Path,
    run_id: &str,
    query_file: PathBuf,
    new_session: bool,
) -> Result<ChatStarted> {
    let (app, cwd, run_id) = (app.clone(), cwd.to_path_buf(), run_id.to_string());
    let args = build_args(backend, req, &query_file, &cwd);
    let new_hermes_skills = (new_session && backend == "hermes").then(|| req.skills.clone());
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
        cancel: None,
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
                            if s.is_none() {
                                if let Some(skills) = &new_hermes_skills {
                                    save_launch_skills(&sid, skills);
                                }
                            }
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
    let mut guard = state
        .active
        .lock()
        .map_err(|_| anyhow!("chat lock poisoned"))?;
    let Some(active) = guard.as_mut() else {
        bail!("Nothing is streaming");
    };
    active.stopped.store(true, Ordering::SeqCst);
    // Deny anything waiting for an answer, so the turn can end.
    if let Ok(mut m) = state.pending.lock() {
        m.clear();
    }
    let run_id = active.run_id.clone();
    if let Some(cancel) = active.cancel.take() {
        let _ = cancel.send(());
        // Hermes stops at the next safe point; if it does not within 20 s, drop the connection.
        let (state, rid) = (state.clone(), run_id.clone());
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            let still = state
                .active
                .lock()
                .map(|a| a.as_ref().is_some_and(|c| c.run_id == rid))
                .unwrap_or(false);
            if still {
                crate::acp_host::reset();
            }
        });
    } else if active.pid != 0 {
        procs::kill_tree(active.pid);
    }
    Ok(run_id)
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
    fn acp_updates_become_the_chat_events_the_ui_renders() {
        use crate::acp::{AcpEvent, PermissionChoice};
        let mut clock = HashMap::new();
        let start = acp_chat_events(
            AcpEvent::ToolCall {
                id: "t1".into(),
                title: "terminal: rm -rf x".into(),
                tool_kind: "execute".into(),
                status: "pending".into(),
            },
            &mut clock,
        );
        assert_eq!(start.len(), 1);
        assert_eq!(start[0].kind, "tool_start");
        assert_eq!(start[0].tool_name.as_deref(), Some("terminal"));
        assert_eq!(start[0].input.as_ref().unwrap()["detail"], "rm -rf x");
        let mcp = acp_chat_events(
            AcpEvent::ToolCall {
                id: "t2".into(),
                title: "mcp__agentos_hub__recall".into(),
                tool_kind: "other".into(),
                status: "pending".into(),
            },
            &mut clock,
        );
        assert_eq!(
            (mcp[0].server.as_deref(), mcp[0].tool_name.as_deref()),
            (Some("agentos_hub"), Some("recall"))
        );
        let done = acp_chat_events(
            AcpEvent::ToolUpdate {
                id: "t1".into(),
                status: "failed".into(),
                output: "denied".into(),
            },
            &mut clock,
        );
        assert_eq!(
            (done[0].kind.as_str(), done[0].is_error),
            ("tool_result", Some(true))
        );
        assert!(done[0].duration_ms.is_some());
        assert!(acp_chat_events(
            AcpEvent::ToolUpdate {
                id: "t1".into(),
                status: "in_progress".into(),
                output: String::new()
            },
            &mut clock
        )
        .is_empty());
        assert_eq!(
            acp_chat_events(AcpEvent::Thought { text: "hm".into() }, &mut clock)[0].kind,
            "thinking"
        );
        let p = acp_chat_events(
            AcpEvent::Permission {
                request_id: "r1".into(),
                title: "recursive delete".into(),
                options: vec![PermissionChoice {
                    id: "deny".into(),
                    name: "Deny".into(),
                    kind: "reject_once".into(),
                }],
            },
            &mut clock,
        );
        assert_eq!(
            (p[0].kind.as_str(), p[0].tool_id.as_deref()),
            ("permission", Some("r1"))
        );
        assert_eq!(p[0].input.as_ref().unwrap()[0]["kind"], "reject_once");
    }

    #[test]
    fn an_acp_turn_reports_its_duration() {
        let e = complete_event(Some("s".into()), false, Some(4200));
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(
            (v["kind"].as_str(), v["duration_ms"].as_u64()),
            (Some("complete"), Some(4200))
        );
        assert!(v.get("stopped").is_none());
        assert_eq!(
            serde_json::to_value(complete_event(None, true, None)).unwrap()["stopped"],
            true
        );
    }

    #[test]
    fn approval_timeout_setting_and_choice() {
        use crate::acp::PermissionChoice;
        assert_eq!(parse_timeout(None), 600);
        assert_eq!(parse_timeout(Some("x")), 600);
        assert_eq!(parse_timeout(Some("3")), 10);
        assert_eq!(parse_timeout(Some("900")), 900);
        assert_eq!(
            (human_secs(600).as_str(), human_secs(45).as_str()),
            ("10 min", "45 s")
        );
        let opt = |id: &str, kind: &str| PermissionChoice {
            id: id.into(),
            name: id.into(),
            kind: kind.into(),
        };
        let all = [
            opt("a", "allow_once"),
            opt("da", "reject_always"),
            opt("d", "reject_once"),
        ];
        assert_eq!(timeout_choice(&all).as_deref(), Some("d"));
        assert_eq!(
            timeout_choice(&all[..2]),
            None,
            "never deny always on the owner's behalf"
        );
    }

    #[test]
    fn an_unanswered_approval_is_denied_after_the_timeout() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let state = ChatState::default();
            let (tx, rx) = tokio::sync::oneshot::channel();
            state.pending.lock().unwrap().insert("r1".into(), tx);
            assert!(
                expire_after(
                    &state.pending,
                    "r1",
                    Duration::from_millis(20),
                    Some("deny".into())
                )
                .await
            );
            assert_eq!(rx.await.unwrap().as_deref(), Some("deny"));
            // Answered in time: the timer does nothing.
            let (tx2, rx2) = tokio::sync::oneshot::channel();
            state.pending.lock().unwrap().insert("r2".into(), tx2);
            answer_permission(&state, "r2", Some("allow".into())).unwrap();
            assert!(
                !expire_after(
                    &state.pending,
                    "r2",
                    Duration::from_millis(5),
                    Some("deny".into())
                )
                .await
            );
            assert_eq!(rx2.await.unwrap().as_deref(), Some("allow"));
        });
    }

    #[test]
    fn answering_an_approval_reaches_the_turn_once() {
        let state = ChatState::default();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        state.pending.lock().unwrap().insert("r1".into(), tx);
        answer_permission(&state, "r1", Some("allow".into())).unwrap();
        assert_eq!(rx.try_recv().unwrap(), Some("allow".to_string()));
        assert!(
            answer_permission(&state, "r1", None).is_err(),
            "a second answer is refused"
        );
    }

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
            provider: None,
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
        for refused in ["-m ", "-s ", "--checkpoints"] {
            assert!(
                !joined.contains(refused),
                "Hermes refuses changed creation flags on resume: {joined}"
            );
        }
        let new = build_args(
            "hermes",
            &ChatRequest {
                session_id: None,
                ..req.clone()
            },
            Path::new("C:/q.md"),
            Path::new("C:/repo"),
        )
        .join(" ");
        assert!(new.contains("--checkpoints"));
        assert!(new.contains("-m claude-opus-5-5[1m]"));
        assert!(new.contains("-s agentos-studio-dev,obsidian"), "{new}");
        let c = build_args(
            "claude-code",
            &ChatRequest {
                model: None,
                provider: None,
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
    fn a_skill_added_mid_chat_is_invoked_not_passed_as_a_flag() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let launched = s(&["obsidian"]);
        let (p, loaded) =
            hermes_resume_prompt("fix it", &s(&["obsidian", "github"]), Some(&launched));
        assert_eq!(p, "/github fix it");
        assert_eq!(loaded, s(&["obsidian", "github"]));
        let (p, _) = hermes_resume_prompt("fix it", &s(&["obsidian"]), Some(&launched));
        assert_eq!(p, "fix it", "nothing new, nothing added");
        let (p, _) = hermes_resume_prompt("/model x", &s(&["github"]), Some(&launched));
        assert_eq!(p, "/model x", "a slash command is never prefixed");
        let (p, loaded) = hermes_resume_prompt("hi", &s(&["github"]), None);
        assert_eq!(
            (p.as_str(), loaded),
            ("hi", s(&["github"])),
            "unknown sessions are left alone"
        );
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
