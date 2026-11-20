//! The native MCP client in `mcp_manager` against the real `--mcp-stdio` binary.

use std::path::Path;
use std::time::Duration;

use agentos_studio_lib::mcp_manager::{probe_stdio, ServerDef};
use agentos_studio_lib::{hub_identity, permissions, policy, store};
use serde_json::{json, Value};

const TOKEN: &str = "native-isolated-qoder-token";

fn native_def(root: &Path, token: &str) -> ServerDef {
    ServerDef {
        transport: "stdio".into(),
        command: Some(env!("CARGO_BIN_EXE_agentos-studio").into()),
        args: vec![
            "--mcp-stdio".into(),
            "--vault".into(),
            root.join("vault").display().to_string(),
        ],
        env: [
            (
                "AGENTOS_STUDIO_DB".into(),
                root.join("studio.db").display().to_string(),
            ),
            (
                "AGENTOS_INDEX_DIR".into(),
                root.join("index").display().to_string(),
            ),
            (
                "HERMES_HOME".into(),
                root.join("hermes").display().to_string(),
            ),
            ("AGENTOS_HUB_TOKEN".into(), token.into()),
            ("AGENTOS_REPO".into(), String::new()),
        ]
        .into_iter()
        .collect(),
        enabled: true,
        ..Default::default()
    }
}

fn seeded_store(root: &Path) -> rusqlite::Connection {
    let c = store::open_at(&root.join("studio.db")).unwrap();
    c.pragma_update(None, "journal_mode", "delete").unwrap();
    store::kv_set(&c, "hub.token.qoder", &hub_identity::hash(TOKEN)).unwrap();
    c
}

#[test]
fn native_probe_lists_every_hub_tool() {
    let d = tempfile::tempdir().unwrap();
    drop(seeded_store(d.path()));
    let before = std::fs::read(d.path().join("studio.db")).unwrap();
    let def = native_def(d.path(), TOKEN);
    let r = probe_stdio("agentos-hub", &def, Duration::from_secs(30)).unwrap();
    assert!(r.ok);
    assert_eq!(r.server_name, "AgentOS-Hub");
    assert_eq!(r.tools.len(), agentos_studio_lib::bootstrapper::HUB_TOOLS);
    for name in [
        "recall",
        "remember",
        "project_brief",
        "delegate_task",
        "write_shared_memory",
        "agents",
    ] {
        assert!(r.tools.iter().any(|t| t.name == name), "{name} is listed");
    }
    assert!(r
        .tools
        .iter()
        .any(|t| t.name == "claim_task" && t.input_schema.is_object()));
    for name in agentos_studio_lib::studio_read::TOOLS {
        assert!(r.tools.iter().any(|tool| tool.name == name));
    }
    assert!(r.latency_ms < 30_000);
    assert_eq!(std::fs::read(d.path().join("studio.db")).unwrap(), before);
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
}

fn native_messages(root: &Path, token: &str, messages: &[Value]) -> Vec<Value> {
    let def = native_def(root, token);
    let args: Vec<&str> = def.args.iter().map(String::as_str).collect();
    let env: Vec<(&str, &str)> = def
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let input = messages
        .iter()
        .map(|m| format!("{m}\n"))
        .collect::<String>();
    let (code, stdout, _) = agentos_studio_lib::procs::run_with_timeout_env(
        def.command.as_deref().unwrap(),
        &args,
        Some(&input),
        None,
        Duration::from_secs(30),
        &env,
    )
    .unwrap();
    assert_eq!(code, Some(0));
    stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn native_catalog(root: &Path, token: &str) -> Vec<Value> {
    let replies = native_messages(
        root,
        token,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        ],
    );
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "AgentOS-Hub");
    assert_eq!(replies[1]["id"], 2);
    assert!(replies
        .iter()
        .all(|reply| reply["jsonrpc"] == "2.0" && reply.get("error").is_none()));
    replies[1]["result"]["tools"].as_array().unwrap().clone()
}

#[test]
fn native_catalog_respects_scoped_policy_without_writes() {
    let root = tempfile::tempdir().unwrap();
    let c = seeded_store(root.path());
    for tool in ["recall", "studio_status"] {
        permissions::set_scoped_in(&c, "agent", "qoder", "agentos-hub", tool, Some("deny"))
            .unwrap();
    }
    drop(c);
    let db = root.path().join("studio.db");
    let before = std::fs::read(&db).unwrap();
    let tools = native_catalog(root.path(), TOKEN);
    assert_eq!(tools.len(), 17);
    assert!(!tools
        .iter()
        .any(|tool| ["recall", "studio_status"].contains(&tool["name"].as_str().unwrap())));
    assert!(tools.iter().any(|tool| tool["name"] == "whoami"));
    assert_eq!(std::fs::read(&db).unwrap(), before);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn native_catalog_without_token_can_be_legacy_or_empty() {
    let root = tempfile::tempdir().unwrap();
    drop(seeded_store(root.path()));
    let db = root.path().join("studio.db");
    let before = std::fs::read(&db).unwrap();
    let tools = native_catalog(root.path(), "");
    assert_eq!(tools.len(), 13);
    assert!(tools
        .iter()
        .all(|tool| !agentos_studio_lib::studio_read::TOOLS
            .contains(&tool["name"].as_str().unwrap())));
    assert_eq!(std::fs::read(&db).unwrap(), before);

    let c = store::open_at(&db).unwrap();
    c.pragma_update(None, "journal_mode", "delete").unwrap();
    policy::set_in(&c, "agentos-hub", "*", "deny").unwrap();
    drop(c);
    let before = std::fs::read(&db).unwrap();
    assert!(native_catalog(root.path(), "").is_empty());
    assert_eq!(std::fs::read(&db).unwrap(), before);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn native_discovery_does_not_initialize_missing_paths() {
    let root = tempfile::tempdir().unwrap();
    let messages = native_messages(
        root.path(),
        "unknown-isolated-token",
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"studio_status","arguments":{}}}),
        ],
    );
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["result"]["serverInfo"]["name"], "AgentOS-Hub");
    assert_eq!(messages[1]["error"]["code"], -32603);
    assert_eq!(messages[2]["result"]["isError"], true);
    assert!(messages[2]["result"]["structuredContent"]["error"]["code"].is_string());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn native_studio_status_preserves_database_and_requires_registered_token() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("studio.db");
    drop(seeded_store(root.path()));
    let before = std::fs::read(&db).unwrap();
    let request = [
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"studio_status","arguments":{}}}),
    ];
    let allowed = native_messages(root.path(), TOKEN, &request);
    assert_eq!(allowed[0]["result"]["isError"], false);
    assert_eq!(allowed[0]["result"]["structuredContent"]["ok"], true);
    let denied = native_messages(root.path(), "", &request);
    assert_eq!(denied[0]["result"]["isError"], true);
    assert_eq!(
        denied[0]["result"]["structuredContent"]["error"]["code"],
        "FORBIDDEN"
    );
    assert_eq!(std::fs::read(&db).unwrap(), before);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn native_probe_reports_a_missing_command() {
    let root = tempfile::tempdir().unwrap();
    let mut def = native_def(root.path(), "");
    def.command = Some("definitely-not-a-real-mcp-server-binary".into());
    let err = probe_stdio("ghost", &def, Duration::from_secs(5)).unwrap_err();
    assert!(err.to_string().contains("not found"));
}
