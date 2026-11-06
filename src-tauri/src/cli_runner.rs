//! In-app terminal: runs a command line through PowerShell (Windows) or `sh` and streams
//! stdout and stderr to `agentos://cli-output`, so `hermes`, `claude`, `git`, `gh` and any
//! shell command run inside Studio without an external terminal window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::procs;

pub const EV_CLI: &str = "agentos://cli-output";

#[derive(Serialize, Clone, Debug)]
pub struct CliLine {
    pub run_id: String,
    pub stream: String,
    pub line: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u128>,
}

#[derive(Default)]
pub struct CliState {
    runs: Arc<Mutex<HashMap<String, u32>>>,
}

/// Commands that need an interactive terminal and would hang or exit without one.
pub fn interactive_hint(command: &str) -> Option<&'static str> {
    let words: Vec<&str> = command.split_whitespace().collect();
    match words.as_slice() {
        ["hermes"] | ["hermes", "chat"] => Some("Interactive Hermes runs in the Chat Studio tab. Use hermes chat -q \"...\" here for one-shot answers."),
        ["claude"] => Some("Interactive Claude Code runs in the Chat Studio tab (backend: Claude Code). Use claude -p \"...\" here."),
        ["hermes", "model"] | ["hermes", "setup"] => Some("This wizard is interactive. Use the Control Plane tab to switch providers and models."),
        _ => None,
    }
}

pub fn shell_invocation(command: &str) -> (String, Vec<String>) {
    if cfg!(windows) {
        let script = format!(
            "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; $OutputEncoding=[System.Text.Encoding]::UTF8; $ProgressPreference='SilentlyContinue'; {command}; if ($LASTEXITCODE -ne $null) {{ exit $LASTEXITCODE }}"
        );
        (
            "powershell".into(),
            vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-Command".into(),
                script,
            ],
        )
    } else {
        ("sh".into(), vec!["-lc".into(), command.into()])
    }
}

fn emit(
    app: &AppHandle,
    run_id: &str,
    stream: &str,
    line: String,
    code: Option<i32>,
    duration_ms: Option<u128>,
) {
    let _ = app.emit(
        EV_CLI,
        CliLine {
            run_id: run_id.into(),
            stream: stream.into(),
            line,
            code,
            duration_ms,
        },
    );
}

pub fn execute_cli_command(
    app: AppHandle,
    state: &CliState,
    command: &str,
    cwd: &str,
) -> Result<String> {
    let command = command.trim();
    if command.is_empty() {
        bail!("Type a command");
    }
    if let Some(hint) = interactive_hint(command) {
        bail!("{hint}");
    }
    let dir = PathBuf::from(cwd.trim());
    let dir = if dir.is_dir() {
        dir
    } else {
        crate::settings::home()
    };
    let (shell, args) = shell_invocation(command);
    let exe = procs::find(&shell).ok_or_else(|| anyhow!("{shell} was not found"))?;
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&args)
        .current_dir(&dir)
        .env("PATH", procs::search_path())
        .env("PYTHONIOENCODING", "utf-8")
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .env("TERM", "dumb")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(false);
    #[cfg(windows)]
    cmd.creation_flags(procs::CREATE_NO_WINDOW);
    let mut child = cmd.spawn().with_context(|| format!("starting {shell}"))?;
    let run_id = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let pid = child.id().unwrap_or(0);
    state
        .runs
        .lock()
        .map_err(|_| anyhow!("cli lock poisoned"))?
        .insert(run_id.clone(), pid);
    emit(
        &app,
        &run_id,
        "start",
        format!("{} $ {command}", dir.display()),
        None,
        None,
    );
    let started = Instant::now();
    let mut readers = Vec::new();
    for (name, reader) in [
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
        let (app, rid) = (app.clone(), run_id.clone());
        readers.push(tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(raw)) = lines.next_line().await {
                emit(&app, &rid, name, procs::clean_line(&raw), None, None);
            }
        }));
    }
    let runs = state.runs.clone();
    let rid = run_id.clone();
    tauri::async_runtime::spawn(async move {
        let status = child.wait().await;
        for r in readers {
            let _ = r.await;
        }
        let code = status.ok().and_then(|s| s.code());
        let killed = runs
            .lock()
            .map(|mut r| r.remove(&rid).is_none())
            .unwrap_or(false);
        let note = if killed {
            "stopped".to_string()
        } else {
            format!(
                "exited with code {}",
                code.map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".into())
            )
        };
        emit(
            &app,
            &rid,
            "exit",
            note,
            code,
            Some(started.elapsed().as_millis()),
        );
    });
    Ok(run_id)
}

pub fn kill_cli_command(state: &CliState, run_id: &str) -> Result<()> {
    let pid = state
        .runs
        .lock()
        .map_err(|_| anyhow!("cli lock poisoned"))?
        .remove(run_id)
        .ok_or_else(|| anyhow!("Command {run_id} is not running"))?;
    procs::kill_tree(pid);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_interactive_commands() {
        assert!(interactive_hint("hermes").is_some());
        assert!(interactive_hint("  claude  ").is_some());
        assert!(interactive_hint("hermes model").is_some());
        assert!(interactive_hint("hermes doctor").is_none());
        assert!(interactive_hint("claude auth status").is_none());
        assert!(interactive_hint("git status").is_none());
    }

    #[test]
    fn wraps_commands_for_the_platform_shell() {
        let (shell, args) = shell_invocation("git status");
        if cfg!(windows) {
            assert_eq!(shell, "powershell");
            let script = args.last().unwrap();
            assert!(script.contains("git status;"));
            assert!(script.contains("exit $LASTEXITCODE"));
            assert!(args.contains(&"-NonInteractive".to_string()));
        } else {
            assert_eq!(shell, "sh");
            assert_eq!(args, vec!["-lc".to_string(), "git status".to_string()]);
        }
    }
}
