//! Opt-in: `cargo test --test live -- live_hub_identity:: --ignored --nocapture --test-threads 1`
//!
//! Phase B item 6 against the real machine. Gives every registered agent its own agentos-hub
//! token, checks each entry resolves to its own agent, then runs the built hub binary with each
//! token: `whoami` names the agent, writing as another agent is refused, and with no token the hub
//! is read-only. A second provisioning pass changes nothing.
//!
//! Restores what it touched: Hermes `config.yaml`, `~/.qoder/settings.json` and
//! `~/.gemini/config/mcp_config.json` byte for byte; the Claude Code agentos-hub entry through
//! `claude mcp` (the file itself is rewritten by every running Claude session, so only that entry
//! is compared); and the stored token hashes.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use agentos_studio_lib::{hub_identity, mcp_manager, procs, settings, store};
use serde_json::{json, Value};

fn hub_call(token: Option<&str>, calls: &[Value]) -> Vec<Value> {
    let exe = env!("CARGO_BIN_EXE_agentos-studio");
    let vault = settings::vault_path();
    let mut cmd = Command::new(exe);
    cmd.args(["--mcp-stdio", "--vault", &vault.display().to_string()])
        .env_remove(hub_identity::ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(t) = token {
        cmd.env(hub_identity::ENV, t);
    }
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut replies = Vec::new();
    for (i, c) in calls.iter().enumerate() {
        let msg = json!({"jsonrpc": "2.0", "id": i + 1, "method": "tools/call", "params": c});
        writeln!(stdin, "{msg}").unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        replies.push(v["result"].clone());
    }
    drop(stdin);
    let _ = child.wait();
    replies
}

fn text(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or("").to_string()
}

#[test]
#[ignore = "writes identity tokens into the real agent configs, then restores them"]
fn every_agent_gets_its_own_identity() {
    let files = [
        procs::hermes_config(),
        procs::qoder_settings(),
        procs::gemini_mcp_config(),
    ];
    let backups: Vec<Option<Vec<u8>>> = files.iter().map(|f| std::fs::read(f).ok()).collect();
    let before = mcp_manager::all_sources();
    let claude_before = before
        .get("claude-code")
        .and_then(|m| m.get("agentos-hub"))
        .cloned();
    let had_hash: Vec<(String, Option<String>)> = {
        let c = store::open().unwrap();
        hub_identity::AGENTS
            .iter()
            .map(|a| {
                let k = format!("hub.token.{a}");
                (k.clone(), store::kv_get(&c, &k).unwrap())
            })
            .collect()
    };

    let result = std::panic::catch_unwind(|| {
        let given = mcp_manager::provision_hub_tokens();
        for o in &given {
            println!("{}: ok={} {}", o.agent, o.ok, o.detail);
            assert!(o.ok, "{}: {}", o.agent, o.detail);
            assert!(!o.detail.contains("AGENTOS_HUB_TOKEN="), "never the token");
        }
        let after = mcp_manager::all_sources();
        let mut tokens = Vec::new();
        for agent in hub_identity::AGENTS {
            let Some(def) = after.get(agent).and_then(|m| m.get("agentos-hub")) else {
                println!("{agent}: no agentos-hub entry, skipped");
                continue;
            };
            let t = def
                .env
                .get(hub_identity::ENV)
                .cloned()
                .expect("token in entry");
            assert_eq!(hub_identity::agent_for(&t).as_deref(), Some(agent));
            tokens.push((agent, t));
        }
        assert!(tokens.len() >= 2, "Hermes and Claude Code at least");
        let distinct: std::collections::BTreeSet<&String> = tokens.iter().map(|(_, t)| t).collect();
        assert_eq!(distinct.len(), tokens.len(), "one token per agent");

        assert!(
            mcp_manager::provision_hub_tokens().is_empty(),
            "a second pass changes nothing"
        );

        for (agent, token) in &tokens {
            let other = if *agent == "hermes" {
                "claude-code"
            } else {
                "hermes"
            };
            let r = hub_call(
                Some(token),
                &[
                    json!({"name": "whoami", "arguments": {}}),
                    json!({"name": "claim_task", "arguments": {"task_id": "TASK-999", "agent_name": other}}),
                ],
            );
            let who: Value = serde_json::from_str(&text(&r[0])).unwrap();
            println!("{agent}: whoami -> {who}");
            assert_eq!(who["agent"], *agent);
            assert_eq!(r[1]["isError"], true);
            assert!(
                text(&r[1]).contains(&format!("not {other}")),
                "{}",
                text(&r[1])
            );
        }
        let anon = hub_call(
            None,
            &[
                json!({"name": "whoami", "arguments": {}}),
                json!({"name": "fetch_pending_tasks", "arguments": {"agent_name": "all"}}),
                json!({"name": "remember", "arguments": {"type": "fact", "topic": "x", "content": "x"}}),
            ],
        );
        let who: Value = serde_json::from_str(&text(&anon[0])).unwrap();
        assert_eq!(who["read_only"], true);
        assert_eq!(anon[1]["isError"], false, "reads work");
        assert_eq!(anon[2]["isError"], true, "writes do not");
        assert!(text(&anon[2]).contains("read-only"));
    });

    // Restore everything, whatever happened above.
    for (f, b) in files.iter().zip(&backups) {
        if let Some(bytes) = b {
            std::fs::write(f, bytes).unwrap();
        }
    }
    if let Some(def) = &claude_before {
        let spec = mcp_manager::ServerSpec {
            name: "agentos-hub".into(),
            transport: "stdio".into(),
            command_or_url: def.command.clone().unwrap_or_default(),
            args: def.args.clone(),
            env_vars: def.env.clone(),
            auth: None,
        };
        let r = mcp_manager::add_mcp_server(&spec, &["claude-code".to_string()]).unwrap();
        assert!(r.iter().all(|o| o.ok), "claude entry restored: {r:?}");
    }
    {
        let c = store::open().unwrap();
        for (k, v) in &had_hash {
            match v {
                Some(v) => store::kv_set(&c, k, v).unwrap(),
                None => store::kv_delete(&c, k).unwrap(),
            }
        }
    }
    for (f, b) in files.iter().zip(&backups) {
        if let Some(bytes) = b {
            assert_eq!(
                &std::fs::read(f).unwrap(),
                bytes,
                "{} restored",
                f.display()
            );
        }
    }
    let now = mcp_manager::all_sources();
    let claude_now = now.get("claude-code").and_then(|m| m.get("agentos-hub"));
    assert_eq!(
        claude_now.map(|d| (&d.command, &d.args, &d.env)),
        claude_before
            .as_ref()
            .map(|d| (&d.command, &d.args, &d.env)),
        "claude agentos-hub entry restored"
    );
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
