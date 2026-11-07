//! Session forking: branch a Hermes chat at any message into a new Hermes session.
//!
//! Hermes's gateway is the only writer of `state.db`, so Studio never inserts rows there. A fork
//! is written as a Claude Code style transcript and handed to `hermes sessions import`, which goes
//! through the gateway's own `session.mutate import`. The new session is then verified by reading
//! it back, renamed through `hermes sessions rename`, and linked to its parent in `studio.db`.
//!
//! What a fork carries: the user and assistant text up to the chosen message, with each tool call
//! kept as a one-line `[ran tool: name]` marker. Tool outputs, reasoning and the system prompt are
//! not copied, because the import path accepts conversation turns only. The forked session runs on
//! the active model when it is resumed.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;

use crate::hermes_engine::{self, ChatMessage};

/// One conversation turn as Hermes will store it after import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: &'static str,
    pub content: String,
}

/// What `plan` decided before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Turns the new session starts with. Empty when the fork point is the first user message.
    pub turns: Vec<Turn>,
    /// When the fork point is a user message, its text goes back into the composer so the user
    /// can edit and resend it instead of the new session ending on an unanswered prompt.
    pub draft: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
pub struct ForkResult {
    /// The new Hermes session, or `None` when nothing came before the fork point.
    pub session_id: Option<String>,
    pub parent_id: String,
    pub title: String,
    pub cwd: String,
    /// Turns carried into the new session (after merging, as Hermes stores them).
    pub turns: usize,
    pub draft: Option<String>,
}

/// User turns Hermes's importer drops as tool wrappers (`foreign_sessions._WRAPPER_TAG_RE`).
/// A real user message that happens to start this way is prefixed so it survives the import.
fn wrapper_tag() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^<(?:user_instructions|environment_context|recommended_plugins|skills_instructions|permissions[_-]instructions|turn_context|command-name|command-message|local-command-stdout|system-reminder)\b",
        )
        .expect("static regex")
    })
}

fn assistant_text(m: &ChatMessage) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !m.content.trim().is_empty() {
        parts.push(m.content.trim().to_string());
    }
    for c in &m.tool_calls {
        parts.push(format!("[ran tool: {}]", c.name));
    }
    parts.join("\n\n")
}

/// Merge consecutive same-role turns the way Hermes does on import, so the turn count Studio
/// expects is the count Hermes stores and the read-back check is exact.
fn merge(raw: Vec<Turn>) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for t in raw {
        let text = t.content.trim();
        if text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.role == t.role => {
                last.content.push_str("\n\n");
                last.content.push_str(text);
            }
            _ => out.push(Turn {
                role: t.role,
                content: text.to_string(),
            }),
        }
    }
    if out.first().is_some_and(|t| t.role == "assistant") {
        out.insert(
            0,
            Turn {
                role: "user",
                content: "(imported conversation begins with an assistant reply)".into(),
            },
        );
    }
    out
}

/// Decide what a fork at message `at` contains. Pure, so it is tested without Hermes.
///
/// - At an assistant or tool message: everything up to and including it.
/// - At a user message: everything before it, and the message itself becomes the draft.
pub fn plan(messages: &[ChatMessage], at: i64) -> Result<Plan> {
    let Some(pos) = messages.iter().position(|m| m.id == at) else {
        bail!("Message {at} is not in this session");
    };
    let point = &messages[pos];
    let (upto, draft) = if point.role == "user" {
        (
            pos,
            Some(point.content.clone()).filter(|t| !t.trim().is_empty()),
        )
    } else {
        (pos + 1, None)
    };
    let mut raw = Vec::new();
    for m in &messages[..upto] {
        match m.role.as_str() {
            "user" => {
                let text = m.content.trim();
                let text = if wrapper_tag().is_match(text) {
                    format!("(quoted) {text}")
                } else {
                    text.to_string()
                };
                raw.push(Turn {
                    role: "user",
                    content: text,
                });
            }
            "assistant" => raw.push(Turn {
                role: "assistant",
                content: assistant_text(m),
            }),
            _ => {}
        }
    }
    Ok(Plan {
        turns: merge(raw),
        draft,
    })
}

/// The transcript in the Claude Code JSONL shape `hermes sessions import --from claude` reads.
pub fn transcript(turns: &[Turn], cwd: &str, fork_id: &str) -> String {
    let mut out = String::new();
    for t in turns {
        let mut line = json!({
            "type": t.role,
            "sessionId": fork_id,
            "message": { "role": t.role, "content": t.content },
        });
        if !cwd.is_empty() {
            line["cwd"] = json!(cwd);
        }
        out.push_str(&line.to_string());
        out.push('\n');
    }
    out
}

/// The session id from `hermes sessions import` output (`Imported ... session as <id>`).
pub fn imported_id(output: &str) -> Option<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"session as ([A-Za-z0-9_.:-]+)").expect("static regex"));
    re.captures_iter(output)
        .last()
        .map(|c| c[1].trim_end_matches('.').to_string())
}

fn fork_title(parent_title: &str) -> String {
    let base = parent_title.trim();
    let base = if base.is_empty() {
        "Hermes session"
    } else {
        base
    };
    let base: String = base.chars().take(70).collect();
    format!("Fork of {base}")
}

/// Title and folder of a session, read only.
fn session_meta(c: &Connection, id: &str) -> Result<(String, String)> {
    c.query_row(
        "SELECT COALESCE(title, ''), COALESCE(cwd, '') FROM sessions WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .with_context(|| format!("Session {id} not found"))
}

/// Count of stored user and assistant turns, which must equal what was imported.
fn stored_turns(c: &Connection, id: &str) -> Result<usize> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND role IN ('user', 'assistant')",
        params![id],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

fn scratch_file(fork_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("agentos-fork-{fork_id}.jsonl"))
}

fn write_transcript(path: &Path, body: &str) -> Result<()> {
    let mut f = std::fs::File::create(path)
        .with_context(|| format!("creating fork transcript {}", path.display()))?;
    f.write_all(body.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

/// Fork `session_id` at message `at` into a new Hermes session and link it as a branch.
pub fn fork(session_id: &str, at: i64) -> Result<ForkResult> {
    let c = hermes_engine::open_ro()?;
    let (parent_title, cwd) = session_meta(&c, session_id)?;
    let messages = hermes_engine::load_messages_in(&c, session_id, 5000)?;
    drop(c);
    let plan = plan(&messages, at)?;
    let title = fork_title(&parent_title);
    if plan.turns.is_empty() {
        return Ok(ForkResult {
            session_id: None,
            parent_id: session_id.to_string(),
            title,
            cwd,
            turns: 0,
            draft: plan.draft,
        });
    }

    let fork_id = uuid::Uuid::new_v4().to_string();
    let path = scratch_file(&fork_id);
    write_transcript(&path, &transcript(&plan.turns, &cwd, &fork_id))?;
    let path_arg = path.to_string_lossy().to_string();
    let imported = hermes_engine::hermes_ok(&["sessions", "import", "--from", "claude", &path_arg]);
    let _ = std::fs::remove_file(&path);
    let output = imported?;
    let Some(new_id) = imported_id(&output) else {
        bail!("Hermes imported the fork but printed no session id: {output}");
    };

    let stored = stored_turns(&hermes_engine::open_ro()?, &new_id)?;
    if stored != plan.turns.len() {
        bail!(
            "Fork {new_id} holds {stored} turns, expected {}. It was kept so nothing is lost; delete it from the chat list if unwanted.",
            plan.turns.len()
        );
    }

    // Titles are unique in Hermes, so a second fork of the same chat gets a numbered title.
    let mut final_title = title.clone();
    for n in 1..=5 {
        let candidate = if n == 1 {
            title.clone()
        } else {
            format!("{title} ({n})")
        };
        if hermes_engine::hermes_ok(&["sessions", "rename", &new_id, &candidate]).is_ok() {
            final_title = candidate;
            break;
        }
    }
    crate::projects::link_branch(session_id, None, &final_title, &new_id)?;

    Ok(ForkResult {
        session_id: Some(new_id),
        parent_id: session_id.to_string(),
        title: final_title,
        cwd,
        turns: plan.turns.len(),
        draft: plan.draft,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hermes_engine::ToolCallView;

    fn msg(id: i64, role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            id,
            role: role.into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_name: None,
            tool_call_id: None,
            timestamp: id as f64,
        }
    }

    fn convo() -> Vec<ChatMessage> {
        let mut a2 = msg(2, "assistant", "");
        a2.tool_calls.push(ToolCallView {
            id: "c1".into(),
            name: "mcp__agentos-hub__recall".into(),
            arguments: "{}".into(),
        });
        let mut t3 = msg(3, "tool", "recall output");
        t3.tool_call_id = Some("c1".into());
        vec![
            msg(1, "user", "Find the flaky test"),
            a2,
            t3,
            msg(4, "assistant", "It is in usage.rs"),
            msg(5, "user", "Fix it"),
            msg(6, "assistant", "Fixed"),
        ]
    }

    #[test]
    fn fork_at_assistant_keeps_through_that_reply() {
        let p = plan(&convo(), 4).unwrap();
        assert_eq!(p.draft, None);
        assert_eq!(p.turns.len(), 2);
        assert_eq!(p.turns[0].content, "Find the flaky test");
        assert_eq!(
            p.turns[1].content,
            "[ran tool: mcp__agentos-hub__recall]\n\nIt is in usage.rs"
        );
    }

    #[test]
    fn fork_at_user_drops_it_and_returns_it_as_draft() {
        let p = plan(&convo(), 5).unwrap();
        assert_eq!(p.draft.as_deref(), Some("Fix it"));
        assert_eq!(p.turns.len(), 2);
        assert_eq!(p.turns.last().unwrap().role, "assistant");
    }

    #[test]
    fn fork_at_first_user_message_creates_no_session() {
        let p = plan(&convo(), 1).unwrap();
        assert!(p.turns.is_empty());
        assert_eq!(p.draft.as_deref(), Some("Find the flaky test"));
    }

    #[test]
    fn fork_at_a_tool_result_keeps_the_call() {
        let p = plan(&convo(), 3).unwrap();
        assert_eq!(p.turns.len(), 2);
        assert_eq!(p.turns[1].content, "[ran tool: mcp__agentos-hub__recall]");
    }

    #[test]
    fn unknown_message_is_refused() {
        assert!(plan(&convo(), 99).is_err());
    }

    #[test]
    fn wrapper_like_user_text_survives_the_import_filter() {
        let msgs = vec![
            msg(1, "user", "<system-reminder> is what I saw"),
            msg(2, "assistant", "ok"),
        ];
        let p = plan(&msgs, 2).unwrap();
        assert!(p.turns[0].content.starts_with("(quoted) <system-reminder>"));
    }

    #[test]
    fn leading_assistant_gets_the_same_stub_hermes_inserts() {
        let msgs = vec![msg(1, "assistant", "hello"), msg(2, "assistant", "again")];
        let p = plan(&msgs, 2).unwrap();
        assert_eq!(p.turns.len(), 2);
        assert_eq!(p.turns[0].role, "user");
        assert_eq!(p.turns[1].content, "hello\n\nagain");
    }

    #[test]
    fn transcript_is_claude_jsonl() {
        let turns = vec![
            Turn {
                role: "user",
                content: "hi".into(),
            },
            Turn {
                role: "assistant",
                content: "hello".into(),
            },
        ];
        let body = transcript(&turns, "C:/repo", "f1");
        let lines: Vec<serde_json::Value> = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["type"], "user");
        assert_eq!(lines[0]["message"]["content"], "hi");
        assert_eq!(lines[1]["cwd"], "C:/repo");
        assert!(transcript(&turns, "", "f1")
            .lines()
            .all(|l| !l.contains("cwd")));
    }

    #[test]
    fn imported_id_is_parsed_from_cli_output() {
        let out = "Skipping unreadable plugin directory x\n\u{2713} Imported Claude Code session as 20261107_101500_ab12cd";
        assert_eq!(imported_id(out).as_deref(), Some("20261107_101500_ab12cd"));
        assert_eq!(imported_id("nothing here"), None);
    }

    #[test]
    fn fork_title_is_bounded() {
        assert_eq!(fork_title(""), "Fork of Hermes session");
        assert!(fork_title(&"x".repeat(300)).chars().count() <= 78);
    }
}
