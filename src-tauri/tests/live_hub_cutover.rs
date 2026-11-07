//! Opt-in: `cargo test --test live_hub_cutover -- --ignored --nocapture --test-threads 1`
//!
//! Does exactly what the "Update agentos-hub in every agent" button does, against the real agent
//! configs: stages HUB_SRC (default: the installed app exe) as this version's hub binary under
//! `%LOCALAPPDATA%/AgentOS Hub/<version>/`, registers it in Hermes, Claude Code, Qoder and
//! Antigravity, then probes every agent's registration with a real `initialize` + `tools/list`.
//! Back up the four config files before running it; sessions already open keep their old hub.

use std::path::PathBuf;
use std::time::Duration;

use agentos_studio_lib::{mcp_manager, settings};

#[test]
#[ignore = "rewrites the agentos-hub registration in all four agents"]
fn hub_cutover_to_staged_binary() {
    let src = std::env::var("HUB_SRC")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::data_local_dir()
                .unwrap()
                .join("Programs")
                .join("AgentOS Studio")
                .join("agentos-studio.exe")
        });
    let version = env!("CARGO_PKG_VERSION");
    let staged = mcp_manager::stage_hub_binary_at(&src, &mcp_manager::hub_root(), version)
        .expect("stage hub binary");
    println!("staged {} -> {}", src.display(), staged.display());

    let spec =
        mcp_manager::agentos_hub_spec(&staged.display().to_string(), &settings::vault_path());
    let all: Vec<String> = mcp_manager::AGENTS.iter().map(|s| s.to_string()).collect();
    for o in mcp_manager::add_mcp_server(&spec, &all).expect("register") {
        println!("{o:?}");
    }

    let norm = |s: &str| s.replace('\\', "/").to_lowercase();
    let want = norm(&staged.display().to_string());
    let sources = mcp_manager::all_sources();
    for agent in mcp_manager::AGENTS {
        let def = sources
            .get(agent)
            .and_then(|m| m.get("agentos-hub"))
            .unwrap_or_else(|| panic!("{agent} has no agentos-hub"));
        assert_eq!(
            def.command.as_deref().map(norm),
            Some(want.clone()),
            "{agent}"
        );
        let probe = mcp_manager::probe_stdio("agentos-hub", def, Duration::from_secs(60))
            .unwrap_or_else(|e| panic!("{agent} probe failed: {e:#}"));
        println!(
            "{agent}: ok={} {} v{} tools={} in {} ms",
            probe.ok,
            probe.server_name,
            probe.server_version,
            probe.tools.len(),
            probe.latency_ms
        );
        assert!(probe.ok, "{agent}: {:?}", probe.error);
        assert_eq!(
            probe.server_version, version,
            "{agent} runs another hub version"
        );
    }
    let hub = mcp_manager::get_all_mcp_servers()
        .into_iter()
        .find(|s| s.is_agentos_hub)
        .expect("hub listed");
    assert!(!hub.hub_outdated && !hub.legacy_python, "{hub:?}");
    assert!(
        hub.agents.iter().all(|a| a.matches_primary && a.enabled),
        "{hub:?}"
    );
}
