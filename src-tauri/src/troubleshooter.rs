//! Troubleshooter agent (S4): when rules find no fix, starts a read-only Hermes session
//! on a healthy lane with recent provider logs, `hermes doctor`, `hermes auth status`, and
//! kanban diagnostics, then runs the proposed command ONLY if it matches a fixed allow list.

use serde::Serialize;
use std::sync::{Mutex, OnceLock};

use crate::agent_log::{self, NewEvent};
use crate::hermes_surfaces;
use crate::quota_grid;

const ALLOWED_PREFIXES: &[&str] = &[
    "hermes auth refresh",
    "hermes auth reset",
    "hermes plugins enable",
    "hermes kanban reclaim",
    "hermes kanban dispatch",
];

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TroubleshootResult {
    pub provider: String,
    pub diagnosis: String,
    pub proposed_command: Option<String>,
    pub allowed: bool,
    pub executed: bool,
    pub execution_output: Option<String>,
    pub escalated: bool,
    pub at: i64,
}

static LAST_RESULT: OnceLock<Mutex<Option<TroubleshootResult>>> = OnceLock::new();

fn last_lock() -> &'static Mutex<Option<TroubleshootResult>> {
    LAST_RESULT.get_or_init(|| Mutex::new(None))
}

pub fn status() -> Option<TroubleshootResult> {
    last_lock().lock().unwrap().clone()
}

pub fn is_allowed_command(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty()
        || trimmed.contains(';')
        || trimmed.contains('|')
        || trimmed.contains('&')
        || trimmed.contains('`')
        || trimmed.contains("$(")
        || trimmed.contains('>')
        || trimmed.contains('<')
    {
        return false;
    }
    ALLOWED_PREFIXES
        .iter()
        .any(|prefix| trimmed == *prefix || trimmed.starts_with(&format!("{prefix} ")))
}

pub fn parse_diagnosis_and_command(reply: &str) -> (String, Option<String>) {
    let mut diagnosis_lines = Vec::new();
    let mut proposed = None;
    for line in reply.lines() {
        let t = line.trim();
        if let Some(rest) = t
            .strip_prefix("COMMAND:")
            .or_else(|| t.strip_prefix("Command:"))
            .or_else(|| t.strip_prefix("command:"))
        {
            let clean = rest.trim().trim_matches('`').trim();
            if !clean.is_empty() && !clean.eq_ignore_ascii_case("none") {
                proposed = Some(clean.to_string());
            }
        } else if !t.is_empty() {
            diagnosis_lines.push(t.to_string());
        }
    }
    let diag = if diagnosis_lines.is_empty() {
        reply.trim().to_string()
    } else {
        diagnosis_lines.join(" ")
    };
    (diag, proposed)
}

pub fn run_for_provider(provider: &str, recent_logs: &str) -> TroubleshootResult {
    let lanes = quota_grid::lanes(false);
    let healthy = lanes.iter().find(|l| l.open);
    let doctor_out = run_readonly_cli(&["doctor"]);
    let auth_out = run_readonly_cli(&["auth", "status"]);
    let diag_out = run_readonly_cli(&["kanban", "diagnostics", "--json"]);

    let context = format!(
        "Provider failing: {provider}\nRecent logs:\n{recent_logs}\n\nhermes doctor:\n{doctor_out}\n\nhermes auth status:\n{auth_out}\n\nhermes kanban diagnostics --json:\n{diag_out}\n\nReply with a short diagnosis and at most one line formatted as: COMMAND: <command or none>"
    );

    let res = troubleshoot_with(
        provider,
        &context,
        healthy
            .as_ref()
            .map(|l| (l.provider.as_str(), l.model.as_str())),
        run_hermes_readonly_session,
        execute_allowed_cli,
        |msg| {
            let target = hermes_surfaces::notify_target().unwrap_or_else(|| "whatsapp".into());
            let _ = hermes_surfaces::send(&target, msg);
        },
    );
    *last_lock().lock().unwrap() = Some(res.clone());
    res
}

pub fn troubleshoot_with<A, E, S>(
    provider: &str,
    prompt: &str,
    healthy_lane: Option<(&str, &str)>,
    ask_agent: A,
    exec_cmd: E,
    escalate: S,
) -> TroubleshootResult
where
    A: FnOnce(&str, Option<(&str, &str)>) -> Result<String, String>,
    E: FnOnce(&str) -> Result<String, String>,
    S: FnOnce(&str),
{
    let now = chrono::Utc::now().timestamp();
    let reply = match ask_agent(prompt, healthy_lane) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Troubleshooter session failed for {provider}: {e}");
            escalate(&msg);
            let _ = agent_log::record(&NewEvent {
                kind: "note",
                from_agent: "troubleshooter",
                to_agent: "fleet",
                summary: "Troubleshooter",
                detail: &msg,
                ..Default::default()
            });
            return TroubleshootResult {
                provider: provider.to_string(),
                diagnosis: msg,
                proposed_command: None,
                allowed: false,
                executed: false,
                execution_output: None,
                escalated: true,
                at: now,
            };
        }
    };

    let (diagnosis, proposed_command) = parse_diagnosis_and_command(&reply);
    let allowed = proposed_command
        .as_deref()
        .map(is_allowed_command)
        .unwrap_or(false);

    let mut executed = false;
    let mut execution_output = None;
    let mut escalated = false;

    if allowed {
        if let Some(cmd) = proposed_command.as_deref() {
            match exec_cmd(cmd) {
                Ok(out) => {
                    executed = true;
                    execution_output = Some(out);
                }
                Err(e) => {
                    execution_output = Some(e.clone());
                    escalated = true;
                    escalate(&format!(
                        "Allowed fix `{cmd}` failed for {provider}: {e}. Diagnosis: {diagnosis}"
                    ));
                }
            }
        }
    } else {
        escalated = true;
        let cmd_note = proposed_command.as_deref().unwrap_or("none");
        escalate(&format!(
            "Troubleshooter for {provider}: {diagnosis} (proposed: {cmd_note})"
        ));
    }

    let detail = format!("{provider}: executed={executed} escalated={escalated} diag={diagnosis}");
    let _ = agent_log::record(&NewEvent {
        kind: "note",
        from_agent: "troubleshooter",
        to_agent: "fleet",
        summary: "Troubleshooter",
        detail: &detail,
        ..Default::default()
    });

    TroubleshootResult {
        provider: provider.to_string(),
        diagnosis,
        proposed_command,
        allowed,
        executed,
        execution_output,
        escalated,
        at: now,
    }
}

fn run_readonly_cli(args: &[&str]) -> String {
    match std::process::Command::new("hermes").args(args).output() {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            s.chars().take(1500).collect()
        }
        Err(e) => format!("error: {e}"),
    }
}

fn run_hermes_readonly_session(prompt: &str, lane: Option<(&str, &str)>) -> Result<String, String> {
    let (prov, model) = lane.ok_or_else(|| "no healthy lane available".to_string())?;
    let out = std::process::Command::new("hermes")
        .args(["-z", "--provider", prov, "--model", model, "-p", prompt])
        .output()
        .map_err(|e| format!("hermes -z: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn execute_allowed_cli(cmd: &str) -> Result<String, String> {
    if !is_allowed_command(cmd) {
        return Err("command not on allow list".to_string());
    }
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.first() != Some(&"hermes") || parts.len() < 2 {
        return Err("invalid hermes command".to_string());
    }
    let out = std::process::Command::new("hermes")
        .args(&parts[1..])
        .output()
        .map_err(|e| format!("execute `{cmd}`: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_accepts_only_five_safe_hermes_commands_without_shell_meta() {
        assert!(is_allowed_command("hermes auth refresh"));
        assert!(is_allowed_command(
            "hermes auth reset --provider openrouter"
        ));
        assert!(is_allowed_command("hermes plugins enable antigravity"));
        assert!(is_allowed_command("hermes kanban reclaim task-123"));
        assert!(is_allowed_command("hermes kanban dispatch"));

        assert!(!is_allowed_command("rm -rf /"));
        assert!(!is_allowed_command("hermes auth refresh; whoami"));
        assert!(!is_allowed_command("hermes auth refresh | cat"));
        assert!(!is_allowed_command("hermes config edit"));
    }

    #[test]
    fn executes_allowlisted_command_and_escalates_disallowed_command() {
        let db_dir = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(db_dir.path().join("studio.db")));
        let mut escalated = false;
        let res_ok = troubleshoot_with(
            "antigravity",
            "prompt",
            Some(("claude", "claude-sonnet-5-5")),
            |_, _| {
                Ok("Plugin disabled.
COMMAND: hermes plugins enable antigravity"
                    .into())
            },
            |c| Ok(format!("ran {c}")),
            |_| escalated = true,
        );
        assert!(res_ok.allowed);
        assert!(res_ok.executed);
        assert!(!escalated);

        let mut escalated_msg = String::new();
        let res_bad = troubleshoot_with(
            "antigravity",
            "prompt",
            Some(("claude", "claude-sonnet-5-5")),
            |_, _| {
                Ok("Need manual token.
COMMAND: open browser and login"
                    .into())
            },
            |_| unreachable!(),
            |m| escalated_msg = m.to_string(),
        );
        assert!(!res_bad.allowed);
        assert!(!res_bad.executed);
        assert!(res_bad.escalated);
        assert!(escalated_msg.contains("Need manual token."));
        crate::store::set_test_db(None);
    }
}
