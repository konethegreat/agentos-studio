//! Native MCP server (`agentos-hub`) over stdio: newline-delimited JSON-RPC 2.0.
//!
//! Started with `agentos-studio --mcp-stdio [--vault <path>]`. Nothing but protocol
//! messages is ever written to stdout; diagnostics go to stderr.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::vault::{DelegateArgs, Vault, AGENTS, PRIORITIES};

pub const SERVER_NAME: &str = "AgentOS-Hub";
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const INSTRUCTIONS: &str = "Local-first Agent Bus and Shared Memory backed by an Obsidian vault. At session start call fetch_pending_tasks(agent_name=<you>) and search_shared_memory(<topic>). Hand work to other agents with delegate_task instead of asking the user to copy prompts. Record architectural decisions with write_shared_memory.";

/// Resolve the vault from `--vault <path>`, then `AGENTOS_VAULT`, then saved settings.
pub fn vault_from_args(args: &[String]) -> PathBuf {
    if let Some(i) = args.iter().position(|a| a == "--vault") {
        if let Some(p) = args.get(i + 1) {
            return PathBuf::from(p);
        }
    }
    if let Ok(p) = std::env::var("AGENTOS_VAULT") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    crate::settings::Settings::load().vault_path()
}

pub fn run_from_args(args: &[String]) -> i32 {
    let vault = Vault::new(vault_from_args(args));
    if args.iter().any(|a| a == "--check") {
        eprintln!("{SERVER_NAME} OK - vault: {}", vault.root.display());
        println!("{SERVER_NAME} OK - vault: {}", vault.root.display());
        return 0;
    }
    if let Err(e) = vault.ensure_dirs() {
        eprintln!(
            "{SERVER_NAME}: cannot prepare vault {}: {e:#}",
            vault.root.display()
        );
        return 1;
    }
    eprintln!(
        "{SERVER_NAME} listening on stdio - vault: {}",
        vault.root.display()
    );
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = handle_line(&vault, &line) {
            if writeln!(stdout, "{resp}")
                .and_then(|_| stdout.flush())
                .is_err()
            {
                break;
            }
        }
    }
    0
}

/// Handle one raw input line (single message or batch). Returns the serialized reply.
pub fn handle_line(vault: &Vault, line: &str) -> Option<String> {
    let parsed: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(error(Value::Null, -32700, &format!("Parse error: {e}")).to_string())
        }
    };
    match parsed {
        Value::Array(items) => {
            let replies: Vec<Value> = items
                .into_iter()
                .filter_map(|m| handle_message(vault, m))
                .collect();
            if replies.is_empty() {
                None
            } else {
                Some(Value::Array(replies).to_string())
            }
        }
        msg => handle_message(vault, msg).map(|v| v.to_string()),
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

pub fn handle_message(vault: &Vault, msg: Value) -> Option<Value> {
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let id = msg.get("id").cloned();
    let Some(id) = id else {
        return None; // notification (initialized, cancelled, ...): no reply
    };
    if method.is_empty() {
        return Some(error(id, -32600, "Invalid Request: missing method"));
    }
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    Some(match method.as_str() {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSIONS[1]);
            let version = if PROTOCOL_VERSIONS.contains(&requested) {
                requested
            } else {
                PROTOCOL_VERSIONS[1]
            };
            ok(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS
                }),
            )
        }
        "ping" => ok(id, json!({})),
        "tools/list" => ok(id, json!({"tools": tool_definitions()})),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(vault, name, &args) {
                Ok(v) => ok(
                    id,
                    json!({
                        "content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}],
                        "isError": false
                    }),
                ),
                Err(e) => ok(
                    id,
                    json!({
                        "content": [{"type": "text", "text": format!("Error: {e:#}")}],
                        "isError": true
                    }),
                ),
            }
        }
        "resources/list" => ok(id, json!({"resources": []})),
        "prompts/list" => ok(id, json!({"prompts": []})),
        other => error(id, -32601, &format!("Method not found: {other}")),
    })
}

fn str_arg<'a>(args: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required string argument '{key}'"))
}

pub fn call_tool(vault: &Vault, name: &str, args: &Value) -> anyhow::Result<Value> {
    // Per-tool policy is enforced here, not in the UI, so no client can route around it.
    if let Err(msg) = crate::policy::check("agentos-hub", name) {
        anyhow::bail!("{msg}");
    }
    match name {
        "delegate_task" => {
            let a: DelegateArgs = serde_json::from_value(args.clone())
                .map_err(|e| anyhow::anyhow!("invalid delegate_task arguments: {e}"))?;
            if args.get("creator").and_then(Value::as_str).is_none() {
                anyhow::bail!("missing required string argument 'creator'");
            }
            vault.delegate_task(a)
        }
        "fetch_pending_tasks" => {
            let agent = args
                .get("agent_name")
                .and_then(Value::as_str)
                .unwrap_or("all");
            Ok(serde_json::to_value(vault.fetch_pending_tasks(agent)?)?)
        }
        "claim_task" => Ok(json!(
            vault.claim_task(str_arg(args, "task_id")?, str_arg(args, "agent_name")?)?
        )),
        "complete_task" => Ok(json!(vault.complete_task(
            str_arg(args, "task_id")?,
            str_arg(args, "agent_name")?,
            str_arg(args, "summary_of_work")?
        )?)),
        "write_shared_memory" => {
            let category = args
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("architecture");
            let meta = crate::vault::MemMeta {
                kind: Some(crate::vault::MemKind::parse(
                    args.get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("decision"),
                )),
                supersedes: args
                    .get("supersedes")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                source_session: args
                    .get("source_session")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                agent: args
                    .get("agent")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                repo: args.get("repo").and_then(Value::as_str).map(str::to_string),
            };
            Ok(json!(vault.write_typed_memory(
                str_arg(args, "topic")?,
                str_arg(args, "content")?,
                category,
                &meta
            )?))
        }
        "search_shared_memory" => Ok(serde_json::to_value(
            vault.search_shared_memory(str_arg(args, "query")?)?,
        )?),
        "recall" => {
            let k = args.get("k").and_then(Value::as_u64).unwrap_or(8) as usize;
            let max_tokens = args
                .get("max_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(1500) as usize;
            let repo = args.get("repo").and_then(Value::as_str);
            Ok(serde_json::to_value(crate::memory_index::recall(
                vault,
                str_arg(args, "query")?,
                repo,
                k,
                max_tokens,
            )?)?)
        }
        "remember" => {
            let kind = args
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing required string argument 'type'"))?;
            let meta = crate::vault::MemMeta {
                kind: Some(crate::vault::MemKind::parse(kind)),
                supersedes: args
                    .get("supersedes")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                source_session: args
                    .get("source_session")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                agent: args
                    .get("agent")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                repo: args.get("repo").and_then(Value::as_str).map(str::to_string),
            };
            Ok(json!(vault.write_typed_memory(
                str_arg(args, "topic")?,
                str_arg(args, "content")?,
                args.get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("architecture"),
                &meta
            )?))
        }
        "project_brief" => Ok(json!(vault.project_brief(str_arg(args, "repo")?)?)),
        other => anyhow::bail!("Unknown tool '{other}'"),
    }
}

pub fn tool_definitions() -> Value {
    let agents: Vec<&str> = AGENTS.to_vec();
    let prios: Vec<&str> = PRIORITIES.to_vec();
    json!([
        {
            "name": "delegate_task",
            "description": "Create a new task on the Agent Bus and hand it to another agent. Writes a Markdown file with YAML frontmatter to 02_Agent_Bus/Pending/. Use this instead of asking the user to copy a prompt into another tool. hermes = CLI/DevOps/Notion sync; qoder/antigravity/claude-code = coding; human = credentials and approvals.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "Short, imperative task title."},
                    "assigned_to": {"type": "string", "enum": agents, "description": "Agent that should do the work."},
                    "creator": {"type": "string", "description": "Name of the agent or human creating the task."},
                    "project": {"type": "string", "description": "Project name (ideally matches a note in 01_Projects)."},
                    "context_files": {"type": "array", "items": {"type": "string"}, "description": "Vault-relative or absolute paths the assignee should read."},
                    "instructions": {"type": "string", "description": "Full, self-contained instructions and acceptance criteria."},
                    "priority": {"type": "string", "enum": prios, "default": "high"}
                },
                "required": ["title", "assigned_to", "creator", "project", "context_files", "instructions"]
            }
        },
        {
            "name": "fetch_pending_tasks",
            "description": "List open tasks (Pending + In_Progress) on the Agent Bus. Call this at the start of every session with your own agent name.",
            "inputSchema": {
                "type": "object",
                "properties": {"agent_name": {"type": "string", "default": "all", "description": "hermes | claude-code | qoder | antigravity | human, or all."}}
            }
        },
        {
            "name": "claim_task",
            "description": "Claim a pending task: move it Pending/ -> In_Progress/ and mark it in_progress.",
            "inputSchema": {
                "type": "object",
                "properties": {"task_id": {"type": "string", "description": "e.g. TASK-001"}, "agent_name": {"type": "string", "enum": agents}},
                "required": ["task_id", "agent_name"]
            }
        },
        {
            "name": "complete_task",
            "description": "Complete a task: move it In_Progress/ -> Completed/, append the summary, refresh DASHBOARD.md.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": {"type": "string"},
                    "agent_name": {"type": "string", "enum": agents},
                    "summary_of_work": {"type": "string", "description": "What was done, files changed, PR URL, verification, follow-ups."}
                },
                "required": ["task_id", "agent_name", "summary_of_work"]
            }
        },
        {
            "name": "write_shared_memory",
            "description": "Create or append a structured note in 03_Memory/ and log it in DECISION_LOG.md. Use for architectural decisions, conventions and gotchas.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "topic": {"type": "string", "description": "Note title; becomes 03_Memory/<Topic>.md"},
                    "content": {"type": "string", "description": "Markdown content for a new dated section."},
                    "category": {"type": "string", "default": "architecture"},
                    "type": {"type": "string", "enum": ["decision", "fact", "convention", "episode", "gotcha"], "default": "decision", "description": "Only decision is appended to DECISION_LOG.md."},
                    "supersedes": {"type": "string"},
                    "source_session": {"type": "string"},
                    "agent": {"type": "string"},
                    "repo": {"type": "string"}
                },
                "required": ["topic", "content"]
            }
        },
        {
            "name": "recall",
            "description": "Ranked recall across the vault: bm25 plus recency and wikilink in-degree, superseded notes last. Call this at the start of a task instead of reading every note. Returns chunks with path and line so you can open the exact source.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What you are trying to remember."},
                    "repo": {"type": "string", "description": "Optional repository name to bias ranking toward its notes."},
                    "k": {"type": "integer", "default": 8, "description": "Maximum chunks to return."},
                    "max_tokens": {"type": "integer", "default": 1500, "description": "Approximate token budget for the returned text."}
                },
                "required": ["query"]
            }
        },
        {
            "name": "remember",
            "description": "Write a typed memory note. type=decision is the only one appended to DECISION_LOG.md; fact, convention, episode and gotcha stay in their own note. Use supersedes to replace an outdated note, which marks the old one superseded instead of deleting it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "type": {"type": "string", "enum": ["decision", "fact", "convention", "episode", "gotcha"]},
                    "topic": {"type": "string", "description": "Note title; becomes 03_Memory/<Topic>.md"},
                    "content": {"type": "string", "description": "Markdown content for a new dated section."},
                    "supersedes": {"type": "string", "description": "Topic of the note this replaces."},
                    "category": {"type": "string", "default": "architecture"},
                    "source_session": {"type": "string"},
                    "agent": {"type": "string"},
                    "repo": {"type": "string"}
                },
                "required": ["type", "topic", "content"]
            }
        },
        {
            "name": "project_brief",
            "description": "One page for a repository: purpose, stack, commands, open bus tasks, the last five decisions and known gotchas. Call this at task start, then recall for anything specific.",
            "inputSchema": {
                "type": "object",
                "properties": {"repo": {"type": "string", "description": "Repository or project name, for example agentos-studio."}},
                "required": ["repo"]
            }
        },
        {
            "name": "search_shared_memory",
            "description": "Keyword search across all .md files in 01_Projects/ and 03_Memory/. Returns paths, scores and matching snippets.",
            "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string", "description": "One or more keywords."}},
                "required": ["query"]
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> (tempfile::TempDir, Vault) {
        let d = tempfile::tempdir().unwrap();
        let v = Vault::new(d.path());
        v.scaffold().unwrap();
        (d, v)
    }

    fn call(v: &Vault, id: u64, method: &str, params: Value) -> Value {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        serde_json::from_str(&handle_line(v, &msg.to_string()).unwrap()).unwrap()
    }

    #[test]
    fn handshake_and_tool_list() {
        let (_d, v) = vault();
        let init = call(
            &v,
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert!(handle_line(
            &v,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )
        .is_none());
        let tools = call(&v, 2, "tools/list", json!({}));
        let names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "delegate_task",
                "fetch_pending_tasks",
                "claim_task",
                "complete_task",
                "write_shared_memory",
                "recall",
                "remember",
                "project_brief",
                "search_shared_memory"
            ]
        );
    }

    #[test]
    fn memory_tools_round_trip() {
        let (_d, v) = vault();
        let wrote = call(
            &v,
            11,
            "tools/call",
            json!({"name": "remember", "arguments": {
                "type": "gotcha", "topic": "Port Clash", "content": "8080 is taken by Docker.",
                "repo": "agentos-studio", "agent": "hermes"
            }}),
        );
        assert!(wrote["result"]["content"].is_array());
        let log = v.read_note("03_Memory/DECISION_LOG.md").unwrap();
        assert!(!log.contains("Port Clash"), "a gotcha is not a decision");

        let found = call(
            &v,
            12,
            "tools/call",
            json!({"name": "recall", "arguments": {"query": "port docker", "k": 3}}),
        );
        let text = found["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("Port_Clash"),
            "recall returns the note: {text}"
        );

        let brief = call(
            &v,
            13,
            "tools/call",
            json!({"name": "project_brief", "arguments": {"repo": "agentos-studio"}}),
        );
        let brief_text = brief["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(brief_text.contains("## Gotchas"));
        assert!(brief_text.contains("## Open tasks"));
    }

    #[test]
    fn lifecycle_over_jsonrpc() {
        let (_d, v) = vault();
        let r = call(
            &v,
            3,
            "tools/call",
            json!({"name": "delegate_task", "arguments": {
            "title": "Wire it", "assigned_to": "claude-code", "creator": "hermes", "project": "AgentOS",
            "context_files": [], "instructions": "Do it.", "priority": "high"}}),
        );
        assert_eq!(r["result"]["isError"], false);
        let r = call(
            &v,
            4,
            "tools/call",
            json!({"name": "claim_task", "arguments": {"task_id": "TASK-001", "agent_name": "claude-code"}}),
        );
        assert_eq!(r["result"]["isError"], false);
        let r = call(
            &v,
            5,
            "tools/call",
            json!({"name": "complete_task", "arguments": {"task_id": "TASK-001", "agent_name": "claude-code", "summary_of_work": "ok"}}),
        );
        assert_eq!(r["result"]["isError"], false);
        let bad = call(
            &v,
            6,
            "tools/call",
            json!({"name": "delegate_task", "arguments": {
            "title": "x", "assigned_to": "nobody", "creator": "x", "project": "x", "context_files": [], "instructions": "x"}}),
        );
        assert_eq!(bad["result"]["isError"], true);
    }

    #[test]
    fn protocol_errors() {
        let (_d, v) = vault();
        let parse: Value = serde_json::from_str(&handle_line(&v, "{not json").unwrap()).unwrap();
        assert_eq!(parse["error"]["code"], -32700);
        let missing = call(&v, 7, "nope/method", json!({}));
        assert_eq!(missing["error"]["code"], -32601);
        let batch = handle_line(&v, r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"}]"#).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&batch)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
