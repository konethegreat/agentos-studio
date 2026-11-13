//! Ask another agent: a short headless turn on the target agent, answered back to the caller.
//!
//! The `ask_agent` hub tool calls `ask()`. Claude Code asks Hermes to run a Notion or CLI step;
//! Hermes asks Claude Code or Antigravity for a second opinion. The target runs one turn
//! (`hermes chat -Q --oneshot`, `claude -p`, or `agy` with one stream-json line), in the mission's
//! worktree when the question is about a mission, and its final text is the answer. Every ask and
//! answer is logged on the mission (`agent_events`), with duration and metered cost.
//!
//! Loop guard: while an agent is answering, kv `ask.busy.<agent>` holds the deadline, and that
//! agent's own `ask_agent` calls are refused. The hub runs as a separate process per agent session
//! and MCP clients do not pass their environment on, so the guard lives in `studio.db`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::Value;

use crate::store;

pub const DEFAULT_TIMEOUT_S: u64 = 120;
pub const MAX_TIMEOUT_S: u64 = 600;
const MIN_TIMEOUT_S: u64 = 10;
const MAX_ANSWER: usize = 12_000;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Answer {
    pub from: String,
    pub to: String,
    pub answer: String,
    pub duration_ms: i64,
    pub cost_usd: f64,
    pub session_id: Option<String>,
    pub mission_id: Option<String>,
}

/// A clamped timeout in seconds. Pure.
pub fn timeout_secs(requested: Option<u64>) -> u64 {
    requested
        .unwrap_or(DEFAULT_TIMEOUT_S)
        .clamp(MIN_TIMEOUT_S, MAX_TIMEOUT_S)
}

/// Which agent may be asked, and why not. Pure over `installed`.
pub fn validate(from: &str, to: &str, installed: impl Fn(&str) -> bool) -> Result<String> {
    let to = crate::vault::norm_agent(to)?;
    if to == "human" {
        bail!("ask_agent asks an agent; questions for the owner go through delegate_task with assigned_to human");
    }
    if to == from {
        bail!("{from} cannot ask itself");
    }
    if !crate::bus::is_headless(&to) {
        bail!("{to} runs inside its IDE and cannot answer a headless question; use delegate_task to leave it the question");
    }
    if !installed(&to) {
        bail!("{to} is not installed on this machine");
    }
    Ok(to)
}

/// The prompt the target agent answers. Pure.
pub fn prompt(from: &str, question: &str, context: &str, mission: Option<&str>) -> String {
    let mut p = format!(
        "{from} is asking you a question through AgentOS Studio. Answer it directly and concisely; your final message is returned to {from} as the answer.\n\n\
Rules for this turn: do not edit, create or delete files, do not commit or push, do not call the agentos-hub tools ask_agent, handoff, claim_task or complete_task. You may read files and use recall, project_brief and search_shared_memory.\n\n## Question\n{}\n",
        question.trim()
    );
    if !context.trim().is_empty() {
        p.push_str(&format!("\n## Context from {from}\n{}\n", context.trim()));
    }
    if let Some(m) = mission {
        p.push_str(&format!("\n## Mission\n{}\n", m.trim()));
    }
    p
}

/// Read-only commands an asked or reviewing Antigravity may run, in Claude's rule syntax. A review
/// reads the pull request and the history; headless Antigravity cannot prompt, so anything not on
/// this list is denied and the turn ends with no answer (the M-077 review). Nothing here writes.
pub const AGY_READ_COMMANDS: [&str; 9] = [
    "Bash(git diff*)",
    "Bash(git log*)",
    "Bash(git show*)",
    "Bash(git status*)",
    "Bash(git branch*)",
    "Bash(gh pr view*)",
    "Bash(gh pr diff*)",
    "Bash(gh pr checks*)",
    "Bash(gh repo view*)",
];

/// The Antigravity allow rules for a headless ask or review turn: the hub and the read-only
/// commands. Pure.
pub fn agy_turn_rules() -> Vec<String> {
    let mut claude: Vec<String> = vec!["mcp__agentos-hub".to_string()];
    claude.extend(AGY_READ_COMMANDS.iter().map(|s| s.to_string()));
    crate::routing::agy_allow_rules(&claude)
}

/// The text of an Antigravity stream-json run: `result.response` when present, else the
/// streamed `text_delta` of every agent response step (they arrive while ACTIVE and on DONE). Pure.
pub fn agy_answer(stdout: &str) -> String {
    let mut streamed = String::new();
    let mut result = None;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v["event"].as_str() {
            Some("step_update")
                if v["step_update"]["tool_info"].is_null()
                    && v["step_update"]["step_type"] != "user_input" =>
            {
                if let Some(t) = v["step_update"]["text_delta"].as_str() {
                    streamed.push_str(t);
                }
            }
            Some("result") => {
                result = ["response", "text", "result"]
                    .iter()
                    .find_map(|k| v["result"][*k].as_str())
                    .map(str::to_string)
                    .filter(|t| !t.trim().is_empty());
            }
            _ => {}
        }
    }
    result.unwrap_or(streamed).trim().to_string()
}

/// The answer text of a `claude -p --output-format json` run. Pure.
pub fn claude_answer(stdout: &str) -> Option<(String, bool)> {
    let start = stdout.find('{')?;
    let v: Value = serde_json::from_str(stdout[start..].trim()).ok()?;
    Some((
        v.get("result")?.as_str()?.to_string(),
        v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
    ))
}

fn busy_key(agent: &str) -> String {
    format!("ask.busy.{agent}")
}

/// Whether `agent` is answering a question right now (its asks are refused meanwhile).
pub fn answering(agent: &str) -> bool {
    let now = store::now();
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, &busy_key(agent)).ok().flatten())
        .and_then(|v| v.parse::<i64>().ok())
        .is_some_and(|until| until > now)
}

fn set_busy(agent: &str, until: Option<i64>) {
    if let Ok(c) = store::open() {
        let _ = match until {
            Some(t) => store::kv_set(&c, &busy_key(agent), &t.to_string()),
            None => store::kv_delete(&c, &busy_key(agent)),
        };
    }
}

/// Where the asked agent runs: the mission's worktree, its repository, or Studio's own asks folder.
/// Never the home folder.
fn workdir(mission: Option<&crate::missions::Mission>) -> Result<PathBuf> {
    if let Some(m) = mission {
        if let Some(wt) = crate::worktree::recorded(&m.id).filter(|p| p.is_dir()) {
            return Ok(wt);
        }
        if let Some(d) = crate::review_agent::project_dir(&m.repo).filter(|p| p.is_dir()) {
            return Ok(d);
        }
    }
    let d = store::studio_dir().join("asks");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// One headless turn on `agent` in `dir`. Returns (answer, session id, metered cost).
pub fn run_turn(
    agent: &str,
    prompt_text: &str,
    dir: &Path,
    limit: Duration,
) -> Result<(String, Option<String>, f64)> {
    let qdir = store::studio_dir().join("asks");
    std::fs::create_dir_all(&qdir)?;
    let stamp = uuid::Uuid::new_v4().simple().to_string();
    match agent {
        "hermes" => {
            let qf = qdir.join(format!("{stamp}.md"));
            std::fs::write(&qf, prompt_text)?;
            let qs = qf.display().to_string();
            let ds = dir.display().to_string();
            let r = crate::procs::run_with_timeout(
                "hermes",
                &[
                    "chat",
                    "-Q",
                    "--oneshot",
                    "--max-turns",
                    "12",
                    "--query-file",
                    &qs,
                    "--in",
                    &ds,
                ],
                None,
                Some(dir),
                limit,
            );
            let _ = std::fs::remove_file(&qf);
            let (code, out, err) = r?;
            let sid = crate::review_agent::session_id_in(&err);
            let cost = sid
                .as_ref()
                .map(|s| crate::usage::metered_cost(std::slice::from_ref(s)))
                .unwrap_or(0.0);
            if out.trim().is_empty() {
                bail!(
                    "hermes gave no answer (exit {code:?}): {}",
                    err.lines()
                        .rev()
                        .find(|l| !l.trim().is_empty() && !l.starts_with("Skipping unreadable"))
                        .unwrap_or("")
                );
            }
            Ok((out.trim().to_string(), sid, cost))
        }
        "claude-code" => {
            let allowed = [
                "mcp__agentos-hub__recall",
                "mcp__agentos-hub__project_brief",
                "mcp__agentos-hub__search_shared_memory",
                "Read",
                "Grep",
                "Glob",
            ];
            let mut args = vec!["-p", "--output-format", "json", "--allowedTools"];
            args.extend(allowed);
            let (code, out, err) = crate::procs::run_with_timeout(
                "claude",
                &args,
                Some(prompt_text),
                Some(dir),
                limit,
            )?;
            let (text, is_error) = claude_answer(&out).ok_or_else(|| {
                anyhow::anyhow!(
                    "claude gave no answer (exit {code:?}): {}",
                    err.trim().chars().take(300).collect::<String>()
                )
            })?;
            if is_error {
                bail!(
                    "claude failed: {}",
                    text.chars().take(300).collect::<String>()
                );
            }
            Ok((text.trim().to_string(), None, 0.0))
        }
        "antigravity" => {
            let sf = qdir.join(format!("{stamp}.agy-settings.json"));
            let allow = agy_turn_rules();
            std::fs::write(&sf, crate::bus::agy_settings(&allow))?;
            let args = crate::bus::agy_args(&sf, crate::bus::agy_model().as_deref());
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let r = crate::procs::run_with_timeout(
                "agy",
                &argv,
                Some(&crate::bus::agy_input(prompt_text)),
                Some(dir),
                limit,
            );
            let _ = std::fs::remove_file(&sf);
            let (code, out, err) = r?;
            let text = agy_answer(&out);
            if text.trim().is_empty() {
                bail!(
                    "antigravity gave no answer (exit {code:?}): {}",
                    err.trim().chars().take(300).collect::<String>()
                );
            }
            Ok((text.trim().to_string(), None, 0.0))
        }
        other => bail!("{other} cannot run a headless turn"),
    }
}

/// `from` (the verified hub caller) asks `to`. Blocks until the answer or the timeout.
pub fn ask(
    from: &str,
    to: &str,
    question: &str,
    context: &str,
    mission_id: Option<&str>,
    timeout: Option<u64>,
) -> Result<Answer> {
    if question.trim().is_empty() {
        bail!("ask_agent needs a question");
    }
    if answering(from) {
        bail!("{from} is answering another agent's question right now; answer it instead of asking onward");
    }
    let to = validate(from, to, crate::bus::headless_available)?;
    let mission = match mission_id.map(str::trim).filter(|m| !m.is_empty()) {
        Some(id) => {
            Some(crate::missions::get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?)
        }
        None => None,
    };
    let mission_text = mission.as_ref().map(|m| {
        format!(
            "{} ({}, repository {}): {}\nAcceptance: {}",
            m.id, m.status, m.repo, m.goal, m.acceptance
        )
    });
    let mid = mission.as_ref().map(|m| m.id.clone());
    let secs = timeout_secs(timeout);
    let dir = workdir(mission.as_ref())?;
    let text = prompt(from, question, context, mission_text.as_deref());
    let ask_id = crate::agent_log::record(&crate::agent_log::NewEvent {
        mission_id: mid.as_deref(),
        kind: "ask",
        from_agent: from,
        to_agent: &to,
        summary: &question.trim().chars().take(300).collect::<String>(),
        detail: &format!("{}\n\n{}", question.trim(), context.trim()),
        ..Default::default()
    })?;
    set_busy(&to, Some(store::now() + secs as i64 + 30));
    let started = Instant::now();
    let r = run_turn(&to, &text, &dir, Duration::from_secs(secs));
    set_busy(&to, None);
    let ms = started.elapsed().as_millis() as i64;
    match r {
        Ok((answer, sid, cost)) => {
            let answer: String = answer.chars().take(MAX_ANSWER).collect();
            crate::agent_log::record(&crate::agent_log::NewEvent {
                mission_id: mid.as_deref(),
                kind: "answer",
                from_agent: &to,
                to_agent: from,
                summary: &answer.chars().take(300).collect::<String>(),
                detail: &format!(
                    "ask #{ask_id}\n\n{answer}{}",
                    sid.as_deref()
                        .map(|s| format!("\n\nHermes session: {s}"))
                        .unwrap_or_default()
                ),
                duration_ms: Some(ms),
                cost_usd: Some(cost),
            })?;
            if let (Some(m), true) = (mission.as_ref(), cost > 0.0) {
                let _ = crate::missions::set_spent(&m.id, m.spent_usd + cost);
            }
            Ok(Answer {
                from: from.into(),
                to,
                answer,
                duration_ms: ms,
                cost_usd: cost,
                session_id: sid,
                mission_id: mid,
            })
        }
        Err(e) => {
            let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
                mission_id: mid.as_deref(),
                kind: "answer",
                from_agent: &to,
                to_agent: from,
                summary: &format!("no answer: {e:#}"),
                detail: &format!("ask #{ask_id} failed after {ms} ms: {e:#}"),
                duration_ms: Some(ms),
                ..Default::default()
            });
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_may_read_the_pull_request_but_never_write() {
        let rules = agy_turn_rules();
        assert!(
            rules.contains(&"mcp(agentos-hub/*)".to_string()),
            "{rules:?}"
        );
        for needed in ["git diff", "git log", "gh pr diff", "gh pr view"] {
            assert!(
                rules
                    .iter()
                    .any(|r| r.starts_with(&format!("command(regex:{}", regex::escape(needed)))),
                "{needed} missing from {rules:?}"
            );
        }
        for r in &rules {
            for write in [
                "push", "commit", "merge", "reset", "checkout", "rm ", "close", "edit",
            ] {
                assert!(!r.contains(write), "{r} could write");
            }
            assert!(!r.contains("dangerously"));
            assert_ne!(r, "command(regex:.*)");
        }
    }

    #[test]
    fn only_another_installed_headless_agent_can_be_asked() {
        let all = |_: &str| true;
        assert_eq!(validate("claude-code", "Hermes", all).unwrap(), "hermes");
        assert!(validate("hermes", "hermes", all).is_err());
        assert!(validate("hermes", "human", all).is_err());
        let e = validate("hermes", "qoder", all).unwrap_err().to_string();
        assert!(e.contains("IDE"), "{e}");
        assert!(validate("hermes", "antigravity", |_| false).is_err());
        assert!(validate("hermes", "nobody", all).is_err());
    }

    #[test]
    fn timeouts_are_clamped() {
        assert_eq!(timeout_secs(None), DEFAULT_TIMEOUT_S);
        assert_eq!(timeout_secs(Some(1)), 10);
        assert_eq!(timeout_secs(Some(100_000)), MAX_TIMEOUT_S);
    }

    #[test]
    fn the_prompt_forbids_edits_and_onward_asks() {
        let p = prompt(
            "claude-code",
            "Which Notion page holds the board?",
            "M-004 needs it",
            Some("M-004: g"),
        );
        assert!(p.contains("Which Notion page"));
        assert!(p.contains("do not call the agentos-hub tools ask_agent, handoff"));
        assert!(p.contains("## Context from claude-code\nM-004 needs it"));
        assert!(p.contains("## Mission\nM-004: g"));
        assert!(!prompt("a", "q", " ", None).contains("## Context"));
    }

    #[test]
    fn answers_are_read_from_each_cli_format() {
        let c = r#"{"type":"result","subtype":"success","is_error":false,"result":"Use port 3000.","total_cost_usd":0.01}"#;
        assert_eq!(claude_answer(c).unwrap(), ("Use port 3000.".into(), false));
        assert!(claude_answer("not json").is_none());
        let a = [
            r#"{"event":"init","init":{"permission_mode":"accept-edits"}}"#,
            r#"{"event":"step_update","step_update":{"state":"DONE","tool_info":{"name":"view_file"}}}"#,
            r#"{"event":"step_update","step_update":{"state":"ACTIVE","step_type":"agent_response","text_delta":"Port 3000 "}}"#,
            r#"{"event":"step_update","step_update":{"state":"DONE","step_type":"agent_response","text_delta":"is fine.\n"}}"#,
            r#"{"event":"result","result":{"status":"SUCCESS","num_turns":1}}"#,
        ]
        .join("\n");
        assert_eq!(agy_answer(&a), "Port 3000 is fine.");
        let with_response = format!(
            "{a}\n{}",
            r#"{"event":"result","result":{"status":"SUCCESS","response":"hello council\n"}}"#
        );
        assert_eq!(agy_answer(&with_response), "hello council");
    }

    #[test]
    fn an_agent_that_is_answering_cannot_ask_onward() {
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        assert!(!answering("hermes"));
        set_busy("hermes", Some(store::now() + 60));
        assert!(answering("hermes"));
        let e = ask("hermes", "claude-code", "q", "", None, None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("answering"), "{e}");
        set_busy("hermes", Some(store::now() - 1));
        assert!(!answering("hermes"), "an expired guard does not block");
        set_busy("hermes", None);
        crate::store::set_test_db(None);
    }
}
