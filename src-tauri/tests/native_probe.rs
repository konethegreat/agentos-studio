//! The native MCP client in `mcp_manager` against the real `--mcp-stdio` binary.

use std::path::PathBuf;
use std::time::Duration;

use agentos_studio_lib::mcp_manager::{probe_stdio, ServerDef};
use agentos_studio_lib::vault::Vault;

#[test]
fn native_probe_lists_the_six_hub_tools() {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_agentos-studio"));
    let d = tempfile::tempdir().unwrap();
    Vault::new(d.path()).scaffold().unwrap();
    let def = ServerDef {
        transport: "stdio".into(),
        command: Some(exe.display().to_string()),
        args: vec![
            "--mcp-stdio".into(),
            "--vault".into(),
            d.path().display().to_string(),
        ],
        enabled: true,
        ..Default::default()
    };
    let r = probe_stdio("agentos-hub", &def, Duration::from_secs(30)).unwrap();
    assert!(r.ok);
    assert_eq!(r.server_name, "AgentOS-Hub");
    assert_eq!(r.tools.len(), 6);
    assert!(r
        .tools
        .iter()
        .any(|t| t.name == "claim_task" && t.input_schema.is_object()));
    assert!(r.latency_ms < 30_000);
}

#[test]
fn native_probe_reports_a_missing_command() {
    let def = ServerDef {
        transport: "stdio".into(),
        command: Some("definitely-not-a-real-mcp-server-binary".into()),
        enabled: true,
        ..Default::default()
    };
    let err = probe_stdio("ghost", &def, Duration::from_secs(5)).unwrap_err();
    assert!(err.to_string().contains("not found"));
}
