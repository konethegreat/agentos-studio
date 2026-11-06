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

fn worker_prompt(task_id: &str, agent: &str, body: &str) -> String {
    format!(
        "You are {agent}, working AgentOS task {task_id}. It is already claimed for you on the agentos-hub Agent Bus.\n\n\
         --- TASK SPEC ---\n{body}\n--- END TASK SPEC ---\n\n\
         Follow the AgentOS constitution (CLAUDE.md / AGENTS.md / SYSTEM_CONTEXT). Work autonomously.\n\
         When finished, call the agentos-hub MCP tool complete_task with task_id \"{task_id}\", agent_name \"{agent}\" \
         and a concrete summary_of_work (what changed, files, PR URL, verification). If you cannot finish, explain why in your final message."
    )
}

/// Extract `Repository: <path>` from a task body, if the directory exists.
pub fn repo_from_body(body: &str) -> Option<PathBuf> {
    let re = Regex::new(r"(?m)^Repository:\s*`?([^`\n]+?)`?\s*$").expect("static regex");
    re.captures(body)
        .map(|c| PathBuf::from(c[1].trim()))
        .filter(|p| p.is_dir())
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
    let cwd = repo_from_body(&task.body).unwrap_or_else(crate::settings::home);
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
        "claude-code" => (
            "claude",
            vec![
                "-p".into(),
                "--output-format".into(),
                "stream-json".into(),
                "--verbose".into(),
                "--permission-mode".into(),
                "acceptEdits".into(),
                "--allowedTools".into(),
                "mcp__agentos-hub".into(),
            ],
            Some(prompt.clone()),
        ),
        other => bail!(
            "Agent '{other}' runs inside its IDE; only hermes and claude-code can run headless"
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
    let is_claude = agent == "claude-code";
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
                let rendered = if is_claude && stream_name == "stdout" {
                    render_claude_event(&raw)
                } else {
                    vec![raw.clone()]
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
    let worker = if !queue_only && (agent == "hermes" || agent == "claude-code") {
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
    fn finds_repository_line() {
        let d = tempfile::tempdir().unwrap();
        let body = format!(
            "## Instructions\nDo it\n\nRepository: {}\n",
            d.path().display()
        );
        assert_eq!(repo_from_body(&body).unwrap(), d.path());
        assert!(repo_from_body("Repository: Z:/does/not/exist").is_none());
    }
}
