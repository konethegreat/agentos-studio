//! Real-time vault watcher, quick mission dispatch, and headless worker processes.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use notify::{RecursiveMode, Watcher};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::procs;
use crate::vault::{DelegateArgs, State, Vault};

pub const EV_VAULT: &str = "agentos://vault-changed";
pub const EV_WORKER: &str = "agentos://worker-stream";

#[derive(Debug, Clone, Serialize)]
pub struct WorkerInfo {
    pub task_id: String,
    pub agent: String,
    pub pid: u32,
    pub started_at: String,
    pub log_path: String,
}

#[derive(Default)]
pub struct BusState {
    pub watcher: Mutex<Option<notify::RecommendedWatcher>>,
    pub workers: Arc<Mutex<HashMap<String, WorkerInfo>>>,
}

/// Watch the bus and memory folders and emit a debounced `agentos://vault-changed`.
pub fn start_watcher(app: AppHandle, state: &BusState, vault_root: &Path) -> Result<()> {
    let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            let interesting: Vec<PathBuf> = ev
                .paths
                .into_iter()
                .filter(|p| {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    !name.starts_with(".tmp_") && !name.starts_with(".agentos")
                })
                .collect();
            if !interesting.is_empty() {
                let _ = tx.send(interesting);
            }
        }
    })?;
    let bus = vault_root.join("02_Agent_Bus");
    let mem = vault_root.join("03_Memory");
    fs::create_dir_all(&bus)?;
    fs::create_dir_all(&mem)?;
    watcher.watch(&bus, RecursiveMode::Recursive)?;
    watcher.watch(&mem, RecursiveMode::Recursive)?;
    let projects = vault_root.join("01_Projects");
    fs::create_dir_all(&projects)?;
    watcher.watch(&projects, RecursiveMode::Recursive)?;
    let root = vault_root.to_path_buf();
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut paths = first;
            while let Ok(more) = rx.recv_timeout(Duration::from_millis(120)) {
                paths.extend(more);
            }
            paths.sort();
            paths.dedup();
            let rel: Vec<String> = paths
                .iter()
                .map(|p| {
                    p.strip_prefix(&root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect();
            let memory = rel
                .iter()
                .any(|r| r.starts_with("03_Memory") || r.starts_with("01_Projects"));
            // Keep the FTS index in step with the files. Cheap: unchanged files are skipped by mtime.
            let v = crate::vault::Vault::new(&root);
            let _ = crate::memory_index::refresh(&v);
            if app
                .emit(EV_VAULT, json!({"paths": rel, "memory": memory}))
                .is_err()
            {
                break;
            }
        }
    });
    *state
        .watcher
        .lock()
        .map_err(|_| anyhow!("watcher lock poisoned"))? = Some(watcher);
    Ok(())
}

fn emit_line(app: &AppHandle, task_id: &str, stream: &str, line: &str) {
    let _ = app.emit(
        EV_WORKER,
        json!({"task_id": task_id, "stream": stream, "line": line, "at": crate::vault::now()}),
    );
}

/// Render one Claude Code stream-json event as readable lines.
pub fn render_claude_event(line: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![line.to_string()];
    };
    let short = |s: &str, n: usize| -> String {
        let one = s.replace('\n', " ");
        if one.chars().count() > n {
            format!("{}...", one.chars().take(n).collect::<String>())
        } else {
            one
        }
    };
    match v["type"].as_str().unwrap_or_default() {
        "system" => vec![format!(
            "[session] {} model={}",
            v["subtype"].as_str().unwrap_or("event"),
            v["model"].as_str().unwrap_or("?")
        )],
        "assistant" => v["message"]["content"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|c| match c["type"].as_str() {
                        Some("text") => c["text"].as_str().map(str::to_string),
                        Some("tool_use") => Some(format!(
                            "> {} {}",
                            c["name"].as_str().unwrap_or("tool"),
                            short(&c["input"].to_string(), 160)
                        )),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "user" => v["message"]["content"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|c| c["type"] == "tool_result")
                    .map(|c| {
                        let text = c["content"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| c["content"].to_string());
                        format!("< {}", short(&text, 200))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "result" => vec![format!(
            "[result] {} (turns: {}, cost: ${:.4})",
            v["result"]
                .as_str()
                .unwrap_or(v["subtype"].as_str().unwrap_or("done")),
            v["num_turns"].as_u64().unwrap_or(0),
            v["total_cost_usd"].as_f64().unwrap_or(0.0)
        )],
        _ => vec![],
    }
}

pub fn worker_prompt(task_id: &str, agent: &str, body: &str) -> String {
    format!(
        "You are {agent}, working AgentOS task {task_id}. It is already claimed for you on the agentos-hub Agent Bus.\n\n\
         --- TASK SPEC ---\n{body}\n--- END TASK SPEC ---\n\n\
         Follow the AgentOS constitution (CLAUDE.md / AGENTS.md / SYSTEM_CONTEXT). Work autonomously.\n\
         If the spec names a Worktree you are already in it, on its own Branch: commit there, push with `git push -u origin HEAD`, \
         and open the pull request with `gh pr create`. Never merge and never push to the default branch.\n\
         When finished, call the agentos-hub MCP tool complete_task with task_id \"{task_id}\", agent_name \"{agent}\" \
         and a concrete summary_of_work (what changed, files, PR URL, verification). If you cannot finish, explain why in your final message."
    )
}

fn line_value(body: &str, key: &str) -> Option<String> {
    let re = Regex::new(&format!(r"(?m)^{key}:\s*`?([^`\r\n]+?)`?\s*$")).expect("static regex");
    re.captures(body).map(|c| c[1].trim().to_string())
}

/// Extract `Repository: <path>` from a task body, if the directory exists.
pub fn repo_from_body(body: &str) -> Option<PathBuf> {
    line_value(body, "Repository")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

/// The folder a worker runs in: the mission's `Worktree:` when it exists, else `Repository:`.
pub fn workdir_from_body(body: &str) -> Option<PathBuf> {
    line_value(body, "Worktree")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| repo_from_body(body))
}

/// A model pinned on the mission (`Model: <id>`), if any.
pub fn model_from_body(body: &str) -> Option<String> {
    line_value(body, "Model").filter(|m| !m.is_empty())
}

/// `claude -p` arguments for a headless mission worker. Edits are accepted inside the working
/// folder; shell commands only as the allowlist names them. Never skips permissions. Pure.
pub fn claude_args(allowed: &[String], model: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "acceptEdits",
    ]
    .map(String::from)
    .to_vec();
    if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
        a.extend(["--model".into(), m.trim().into()]);
    }
    a.push("--allowedTools".into());
    a.extend(allowed.iter().cloned());
    a
}

/// The app handle and worker table, so the scheduler thread can start workers like Run now does.
type Host = (AppHandle, Arc<Mutex<HashMap<String, WorkerInfo>>>);
static HOST: std::sync::OnceLock<Host> = std::sync::OnceLock::new();

/// The worker starter, registered by `set_host`. Callers outside this module (the scheduler,
/// `missions::start`) go through it, so code under unit test never links the Tauri runtime: a
/// test binary that does fails to load on Windows (STATUS_ENTRYPOINT_NOT_FOUND).
type Launcher = fn(&Vault, &str, &str) -> Result<WorkerInfo>;
static LAUNCHER: std::sync::OnceLock<Launcher> = std::sync::OnceLock::new();

pub fn set_host(app: AppHandle, workers: Arc<Mutex<HashMap<String, WorkerInfo>>>) {
    let _ = HOST.set((app, workers));
    let _ = LAUNCHER.set(launch);
}

/// Start a headless worker through the registered launcher. Without the app (tests, the stdio
/// hub) there is none, and this says so instead of starting anything.
pub fn start_worker(vault: &Vault, task_id: &str, agent: &str) -> Result<WorkerInfo> {
    let f = LAUNCHER
        .get()
        .ok_or_else(|| anyhow!("no app host: workers start only inside AgentOS Studio"))?;
    f(vault, task_id, agent)
}

/// Agents Studio can run without an IDE: Hermes, Claude Code (`claude -p`) and Antigravity
/// (`agy`, headless print mode). Qoder runs inside its IDE here; its CLI (`qodercli`) is not part of
/// the IDE install. Pure.
pub fn is_headless(agent: &str) -> bool {
    matches!(agent, "hermes" | "claude-code" | "antigravity")
}

/// The executable a headless agent runs as. Pure.
pub fn headless_bin(agent: &str) -> Option<&'static str> {
    match agent {
        "hermes" => Some("hermes"),
        "claude-code" => Some("claude"),
        "antigravity" => Some("agy"),
        _ => None,
    }
}

/// A headless agent whose CLI is installed on this machine.
pub fn headless_available(agent: &str) -> bool {
    headless_bin(agent).is_some_and(|b| procs::find(b).is_some())
}

/// The first available agent in preference order that is not `exclude`: Claude Code (flat
/// subscription, strongest coder here), then Antigravity (flat), then Hermes. Pure over `available`.
pub fn best_headless(exclude: &str, available: impl Fn(&str) -> bool) -> Option<&'static str> {
    ["claude-code", "antigravity", "hermes"]
        .into_iter()
        .find(|a| *a != exclude && available(a))
}

/// `agy` arguments for a headless mission worker: one stream-json turn read from stdin, the
/// allowlist as session-only settings (a file), never skipping permissions. Pure.
pub fn agy_args(settings_file: &Path, model: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = [
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--mode",
        "accept-edits",
    ]
    .map(String::from)
    .to_vec();
    a.extend(["--settings".into(), settings_file.display().to_string()]);
    if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
        a.extend(["--model".into(), m.trim().into()]);
    }
    a
}

/// The session settings file content for an Antigravity worker. Pure.
pub fn agy_settings(allow: &[String]) -> String {
    json!({"permissions": {"allow": allow}}).to_string()
}

/// One stdin line for `agy --input-format stream-json`. Pure.
pub fn agy_input(prompt: &str) -> String {
    format!(
        "{}\n",
        json!({"event": "user", "message": {"content": prompt}})
    )
}

/// Render one Antigravity stream-json event as readable lines.
pub fn render_agy_event(line: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![line.to_string()];
    };
    match v["event"].as_str().unwrap_or_default() {
        "init" => vec![format!(
            "[session] antigravity permission_mode={}",
            v["init"]["permission_mode"].as_str().unwrap_or("?")
        )],
        "step_update" => {
            let s = &v["step_update"];
            if s["state"] != "DONE" {
                return vec![];
            }
            if let Some(name) = s["tool_info"]["name"].as_str() {
                let p = s["tool_info"]["parameters"].to_string();
                let p: String = p.chars().take(160).collect();
                vec![format!("> {name} {p}")]
            } else {
                s["text_delta"]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                    .map(|t| vec![t.trim().to_string()])
                    .unwrap_or_default()
            }
        }
        "result" => vec![format!(
            "[result] {} (turns: {}){}",
            v["result"]["status"].as_str().unwrap_or("?"),
            v["result"]["num_turns"].as_u64().unwrap_or(0),
            v["result"]["denied_actions"]
                .as_array()
                .filter(|d| !d.is_empty())
                .map(|d| format!(" denied: {}", Value::Array(d.clone())))
                .unwrap_or_default()
        )],
        _ => vec![],
    }
}

/// Kill the task's worker if one runs in this app session (stuck retry or block).
pub fn stop_task_worker(task_id: &str) {
    if let Some((_, workers)) = HOST.get() {
        let _ = stop_worker(workers, task_id);
    }
}

/// Whether a worker for this task is alive in this app session.
pub fn worker_running(task_id: &str) -> bool {
    HOST.get()
        .and_then(|(_, w)| w.lock().ok().map(|w| w.contains_key(task_id)))
        .unwrap_or(false)
}

/// Claim a task for a headless agent (if still pending) and start its worker, from any thread.
/// Fails when the app host is not set (unit tests, the stdio hub).
pub fn launch(vault: &Vault, task_id: &str, agent: &str) -> Result<WorkerInfo> {
    let (app, workers) = HOST
        .get()
        .cloned()
        .ok_or_else(|| anyhow!("no app host: workers start only inside AgentOS Studio"))?;
    if let Ok((t, _)) = vault.get_task(task_id) {
        if t.state == State::Pending {
            vault.claim_task(task_id, agent)?;
        }
    }
    // tokio::process needs a runtime context; the scheduler is a plain thread.
    let _rt = tauri::async_runtime::handle().inner().enter();
    spawn_worker(app, workers, vault.clone(), task_id, agent)
}

/// Spawn a headless Hermes or Claude Code worker for a claimed task.
pub fn spawn_worker(
    app: AppHandle,
    workers: Arc<Mutex<HashMap<String, WorkerInfo>>>,
    vault: Vault,
    task_id: &str,
    agent: &str,
) -> Result<WorkerInfo> {
    if workers
        .lock()
        .map_err(|_| anyhow!("worker lock poisoned"))?
        .contains_key(task_id)
    {
        bail!("{task_id} already has a running worker");
    }
    let (task, _) = vault.get_task(task_id)?;
    // Agent work never falls back to the home folder: no repository, no worker.
    let cwd = workdir_from_body(&task.body).ok_or_else(|| {
        anyhow!(
            "{task_id} has no Repository line naming an existing folder, so no worker was started. Dispatch it with a repository."
        )
    })?;
    let prompt = worker_prompt(task_id, agent, &task.body);
    fs::create_dir_all(vault.worker_logs())?;
    let log_path = vault.worker_logs().join(format!("{task_id}.log"));
    let (exe_name, args, stdin_text): (&str, Vec<String>, Option<String>) = match agent {
        "hermes" => {
            let qf = vault.worker_logs().join(format!("{task_id}.prompt.md"));
            fs::write(&qf, &prompt)?;
            (
                "hermes",
                vec![
                    "chat".into(),
                    "-Q".into(),
                    "--oneshot".into(),
                    "--max-turns".into(),
                    "60".into(),
                    "--query-file".into(),
                    qf.display().to_string(),
                ],
                None,
            )
        }
        "claude-code" => {
            // The repository's committed allowlist, read from the main checkout.
            let extra = repo_from_body(&task.body)
                .map(|r| crate::routing::allowed_tools(&r))
                .unwrap_or_default();
            let allowed = crate::routing::claude_allowed_tools(&extra);
            (
                "claude",
                claude_args(&allowed, model_from_body(&task.body).as_deref()),
                Some(prompt.clone()),
            )
        }
        "antigravity" => {
            let extra = repo_from_body(&task.body)
                .map(|r| crate::routing::allowed_tools(&r))
                .unwrap_or_default();
            let allow =
                crate::routing::agy_allow_rules(&crate::routing::claude_allowed_tools(&extra));
            let sf = vault
                .worker_logs()
                .join(format!("{task_id}.agy-settings.json"));
            fs::write(&sf, agy_settings(&allow))?;
            (
                "agy",
                agy_args(&sf, model_from_body(&task.body).as_deref()),
                Some(agy_input(&prompt)),
            )
        }
        other => bail!(
            "Agent '{other}' runs inside its IDE; only hermes, claude-code and antigravity run headless"
        ),
    };
    let exe = procs::find(exe_name)
        .ok_or_else(|| anyhow!("{exe_name} is not installed. Open Setup to install it."))?;
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&args)
        .current_dir(&cwd)
        .env("PATH", procs::search_path())
        .env("AGENTOS_VAULT", &vault.root)
        .stdin(if stdin_text.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(false);
    #[cfg(windows)]
    cmd.creation_flags(procs::CREATE_NO_WINDOW);
    // Callers run inside the Tauri (tokio) runtime, which tokio::process requires.
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("failed to start {exe_name}: {e}"))?;
    let pid = child.id().unwrap_or(0);
    let info = WorkerInfo {
        task_id: task_id.to_string(),
        agent: agent.to_string(),
        pid,
        started_at: crate::vault::now(),
        log_path: log_path.display().to_string(),
    };
    workers
        .lock()
        .map_err(|_| anyhow!("worker lock poisoned"))?
        .insert(task_id.to_string(), info.clone());
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    writeln!(
        log,
        "\n===== {} worker started {} (pid {pid}, cwd {}) =====",
        agent,
        info.started_at,
        cwd.display()
    )?;
    let log = Arc::new(Mutex::new(log));
    emit_line(
        &app,
        task_id,
        "system",
        &format!("{agent} worker started (pid {pid}) in {}", cwd.display()),
    );
    let _ = vault.log_activity(
        task_id,
        &format!("{agent} worker started from AgentOS Studio (pid {pid})"),
    );

    if let (Some(text), Some(mut stdin)) = (stdin_text, child.stdin.take()) {
        tauri::async_runtime::spawn(async move {
            let _ = stdin.write_all(text.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    let tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let renderer: Option<fn(&str) -> Vec<String>> = match agent {
        "claude-code" => Some(render_claude_event),
        "antigravity" => Some(render_agy_event),
        _ => None,
    };
    let mut readers = Vec::new();
    for (stream_name, reader) in [
        (
            "stdout",
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        ),
        (
            "stderr",
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        ),
    ] {
        let Some(reader) = reader else { continue };
        let (app, log, tail, tid) = (app.clone(), log.clone(), tail.clone(), task_id.to_string());
        readers.push(tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(raw)) = lines.next_line().await {
                let rendered = match renderer {
                    Some(render) if stream_name == "stdout" => render(&raw),
                    _ => vec![raw.clone()],
                };
                if let Ok(mut f) = log.lock() {
                    let _ = writeln!(f, "[{stream_name}] {raw}");
                }
                for line in rendered {
                    if let Ok(mut t) = tail.lock() {
                        t.push(line.clone());
                        let len = t.len();
                        if len > 40 {
                            t.drain(..len - 40);
                        }
                    }
                    emit_line(&app, &tid, stream_name, &line);
                }
            }
        }));
    }
    let (tid, agent_s) = (task_id.to_string(), agent.to_string());
    tauri::async_runtime::spawn(async move {
        let status = child.wait().await;
        for r in readers {
            let _ = r.await;
        }
        let code = status.ok().and_then(|s| s.code());
        if let Ok(mut w) = workers.lock() {
            w.remove(&tid);
        }
        if let Ok(mut f) = log.lock() {
            let _ = writeln!(
                f,
                "===== worker exited with {code:?} at {} =====",
                crate::vault::now()
            );
        }
        let still_open = vault
            .get_task(&tid)
            .map(|(t, _)| t.state == State::InProgress)
            .unwrap_or(false);
        let note = match code {
            Some(0) if still_open => {
                let tail_text = tail.lock().map(|t| t.join("\n")).unwrap_or_default();
                let summary = format!(
                    "{agent_s} worker exited cleanly but did not call complete_task, so AgentOS Studio closed it.\n\nOutput tail:\n\n```text\n{tail_text}\n```"
                );
                match vault.complete_task(&tid, &agent_s, &summary) {
                    Ok(_) => "completed by AgentOS Studio after a clean exit".to_string(),
                    Err(e) => format!("clean exit; could not complete task: {e:#}"),
                }
            }
            Some(0) => "clean exit; the agent closed the task itself".to_string(),
            other => {
                let _ = vault.log_activity(&tid, &format!("{agent_s} worker exited with {other:?}; see 04_Agent_Engine/worker_logs/{tid}.log"));
                format!("worker exited with {other:?}; task left in progress")
            }
        };
        let _ = app.emit(EV_WORKER, json!({"task_id": tid, "stream": "exit", "line": note, "code": code, "at": crate::vault::now()}));
    });
    Ok(info)
}

pub fn stop_worker(
    workers: &Arc<Mutex<HashMap<String, WorkerInfo>>>,
    task_id: &str,
) -> Result<String> {
    let info = workers
        .lock()
        .map_err(|_| anyhow!("worker lock poisoned"))?
        .get(task_id)
        .cloned()
        .ok_or_else(|| anyhow!("No running worker for {task_id}"))?;
    procs::kill_tree(info.pid);
    Ok(format!(
        "Stopped {} worker for {task_id} (pid {})",
        info.agent, info.pid
    ))
}

/// Create a task from a one-line goal and, for headless agents, start a worker immediately.
pub fn quick_mission_dispatch(
    app: AppHandle,
    workers: Arc<Mutex<HashMap<String, WorkerInfo>>>,
    vault: Vault,
    prompt: &str,
    project_path: Option<String>,
    target_agent: &str,
    queue_only: bool,
) -> Result<Value> {
    let goal = prompt.trim();
    if goal.is_empty() {
        bail!("Describe the mission first");
    }
    let agent = match target_agent.trim().to_lowercase().as_str() {
        "" | "auto" => "hermes".to_string(),
        other => crate::vault::norm_agent(other)?,
    };
    let repo = project_path
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from);
    if let Some(r) = &repo {
        if !r.is_dir() {
            bail!("Repository folder not found: {}", r.display());
        }
    }
    let headless = is_headless(&agent);
    if repo.is_none() && headless && !queue_only {
        bail!("Pick a repository: {agent} works in a project folder, never in the home folder");
    }
    let project = repo
        .as_ref()
        .and_then(|r| r.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "general".into());
    let first_line = goal.lines().next().unwrap_or(goal);
    let title: String = if first_line.chars().count() > 80 {
        format!("{}...", first_line.chars().take(77).collect::<String>())
    } else {
        first_line.to_string()
    };
    let mut instructions = goal.to_string();
    if let Some(r) = &repo {
        instructions.push_str(&format!("\n\nRepository: {}", r.display()));
    }
    let mut context_files = vec!["03_Memory/SYSTEM_CONTEXT.md".to_string()];
    if let Some(r) = &repo {
        context_files.push(r.join("README.md").display().to_string());
    }
    let task = vault.delegate_task(DelegateArgs {
        title,
        assigned_to: agent.clone(),
        creator: "agentos-studio".into(),
        project,
        context_files,
        instructions,
        priority: "high".into(),
    })?;
    let id = task["id"].as_str().unwrap_or_default().to_string();
    let worker = if !queue_only && headless {
        vault.claim_task(&id, &agent)?;
        Some(spawn_worker(app, workers, vault, &id, &agent)?)
    } else {
        None
    };
    Ok(json!({"task": task, "worker": worker, "agent": agent}))
}

/// Claim (if needed) and run an existing task with a headless agent.
pub fn run_task_now(
    app: AppHandle,
    workers: Arc<Mutex<HashMap<String, WorkerInfo>>>,
    vault: Vault,
    task_id: &str,
    agent: &str,
) -> Result<WorkerInfo> {
    let agent = crate::vault::norm_agent(agent)?;
    let (task, _) = vault.get_task(task_id)?;
    match task.state {
        State::Completed => bail!(
            "{} is already completed; move it back to Pending to run it again",
            task.id
        ),
        State::Pending => {
            vault.claim_task(&task.id, &agent)?;
        }
        State::InProgress => {}
    }
    spawn_worker(app, workers, vault, &task.id, &agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_claude_stream_events() {
        let a = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading files"},{"type":"tool_use","name":"Read","input":{"file_path":"a.rs"}}]}}"#;
        let out = render_claude_event(a);
        assert_eq!(out[0], "Reading files");
        assert!(out[1].starts_with("> Read"));
        let r = r#"{"type":"result","subtype":"success","result":"All done","num_turns":3,"total_cost_usd":0.0123}"#;
        assert!(render_claude_event(r)[0].contains("All done"));
        assert_eq!(
            render_claude_event("plain text"),
            vec!["plain text".to_string()]
        );
    }

    #[test]
    fn the_preferred_headless_agent_is_claude_then_antigravity_then_hermes() {
        assert_eq!(best_headless("qoder", |_| true), Some("claude-code"));
        assert_eq!(best_headless("claude-code", |_| true), Some("antigravity"));
        assert_eq!(best_headless("qoder", |a| a == "hermes"), Some("hermes"));
        assert_eq!(best_headless("qoder", |_| false), None);
        assert!(is_headless("antigravity") && !is_headless("qoder"));
    }

    #[test]
    fn an_antigravity_worker_reads_one_turn_and_never_skips_permissions() {
        let a = agy_args(Path::new("C:/logs/TASK-001.agy-settings.json"), None);
        assert!(!a.iter().any(|x| x.contains("dangerously")));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "--input-format" && w[1] == "stream-json"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "--settings" && w[1].ends_with(".json")));
        let line = agy_input("line one\n\"quoted\"");
        assert!(
            line.ends_with('\n') && line.matches('\n').count() == 1,
            "one NDJSON line"
        );
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["message"]["content"], "line one\n\"quoted\"");
        let s: Value =
            serde_json::from_str(&agy_settings(&["command(regex:git .*)".into()])).unwrap();
        assert_eq!(s["permissions"]["allow"][0], "command(regex:git .*)");

        let tool = r#"{"event":"step_update","step_update":{"state":"DONE","step_type":"tool","tool_info":{"name":"run_command","parameters":{"CommandLine":"git status"}}}}"#;
        assert!(render_agy_event(tool)[0].starts_with("> run_command"));
        let res = r#"{"event":"result","result":{"status":"SUCCESS","num_turns":2,"denied_actions":[{"action":"command"}]}}"#;
        let r = &render_agy_event(res)[0];
        assert!(r.contains("SUCCESS") && r.contains("denied"), "{r}");
    }

    #[test]
    fn a_claude_worker_never_skips_permissions() {
        let allowed = crate::routing::claude_allowed_tools(&["Bash(npm test*)".into()]);
        let a = claude_args(&allowed, Some("claude-opus-5-5"));
        assert!(!a.iter().any(|x| x.contains("dangerously")));
        assert!(!a.iter().any(|x| x == "bypassPermissions"));
        let i = a.iter().position(|x| x == "--allowedTools").unwrap();
        assert_eq!(&a[i + 1..], allowed.as_slice(), "the allowlist is the tail");
        assert!(a
            .windows(2)
            .any(|w| w[0] == "--model" && w[1] == "claude-opus-5-5"));
        assert!(!claude_args(&allowed, None).contains(&"--model".to_string()));
    }

    #[test]
    fn finds_repository_line() {
        let d = tempfile::tempdir().unwrap();
        let body = format!(
            "## Instructions\nDo it\n\nRepository: {}\n",
            d.path().display()
        );
        assert_eq!(repo_from_body(&body).unwrap(), d.path());
        assert!(repo_from_body("Repository: Z:/does/not/exist").is_none());
        assert!(repo_from_body("no repository line").is_none());
    }
}
