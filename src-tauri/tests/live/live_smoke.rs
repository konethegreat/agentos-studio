//! Read-only checks against the real Hermes install on this machine.
//! Run with `cargo test --test live -- live_smoke:: --ignored --nocapture`; CI skips them.

use agentos_studio_lib::{hermes_engine as h, mcp_manager as m};

#[test]
#[ignore = "needs a local Hermes install"]
fn live_hermes_engine() {
    let cfg = h::get_provider_config().expect("provider config");
    println!(
        "active: {} / {} ({:?})",
        cfg.active_label, cfg.active.model, cfg.active_id
    );
    for p in &cfg.providers {
        println!(
            "  {:<22} ready={} active={} {}",
            p.id, p.ready, p.active, p.detail
        );
    }
    for id in ["claude-subscription", "openrouter", "gemini", "local"] {
        let models = h::list_available_models(id).expect("models");
        println!(
            "models[{id}] = {} e.g. {:?}",
            models.len(),
            models.iter().take(4).collect::<Vec<_>>()
        );
    }
    let sessions = h::list_hermes_sessions(None, 10).expect("sessions");
    assert!(!sessions.is_empty());
    for s in sessions.iter().take(5) {
        println!(
            "session {} | {} | {} msgs | {}",
            &s.id[..18.min(s.id.len())],
            s.title,
            s.message_count,
            s.source
        );
    }
    let msgs = h::load_session_messages(&sessions[0].id).expect("messages");
    let tools: usize = msgs.iter().map(|m| m.tool_calls.len()).sum();
    println!(
        "first session: {} messages, {} tool calls",
        msgs.len(),
        tools
    );
    let status = h::HermesState::default().status(true);
    println!(
        "status: hermes {:?} claude {:?} provider {} model {} mcp {}/{} sessions {}",
        status.version,
        status.claude_version,
        status.provider_label,
        status.model,
        status.mcp_enabled,
        status.mcp_total,
        status.sessions
    );
    assert!(status.installed && status.config_ok);
}

#[test]
#[ignore = "needs a local Hermes install"]
fn live_mcp_registry_and_probe() {
    let servers = m::get_all_mcp_servers();
    for s in &servers {
        let agents: Vec<String> = s
            .agents
            .iter()
            .map(|a| format!("{}{}", a.agent, if a.matches_primary { "" } else { "*" }))
            .collect();
        println!(
            "{:<14} {:<5} legacy={} agents={:?}",
            s.name, s.transport, s.legacy_python, agents
        );
    }
    let hub = m::test_mcp_server("agentos-hub").expect("probe");
    println!(
        "agentos-hub via {}: ok={} {}ms {} tools",
        hub.via,
        hub.ok,
        hub.latency_ms,
        hub.tools.len()
    );
    assert!(hub.ok && hub.tools.len() == 6);
    if servers.iter().any(|s| s.name == "notion") {
        let n = m::test_mcp_server("notion").expect("notion probe");
        println!(
            "notion via {}: ok={} {}ms {} tools err={:?}",
            n.via,
            n.ok,
            n.latency_ms,
            n.tools.len(),
            n.error
        );
    }
}

#[test]
#[ignore = "needs a local Hermes install; re-applies the active provider (no net change)"]
fn live_provider_reapply_and_test() {
    let before = h::get_provider_config().unwrap();
    let id = before
        .active_id
        .clone()
        .expect("active provider is in the catalog");
    let (after, log) = h::set_active_provider(&id, &before.active.model, None, None).unwrap();
    println!("reapply log: {log:?}");
    assert_eq!(after.active, before.active, "re-applying is a no-op");
    let sessions_before = h::list_hermes_sessions(None, 1000).unwrap().len();
    let t = h::test_provider(&id, &before.active.model, None).unwrap();
    println!(
        "test: ok={} {}ms reply={:?} err={:?}",
        t.ok, t.latency_ms, t.reply, t.error
    );
    assert!(t.ok);
    let sessions_after = h::list_hermes_sessions(None, 1000).unwrap().len();
    assert_eq!(
        sessions_before, sessions_after,
        "the probe session is removed"
    );
    let bad = h::set_active_provider("openrouter", "anthropic/claude-sonnet-4.6", None, None);
    println!(
        "switch without key: {:?}",
        bad.as_ref().err().map(|e| e.to_string())
    );
    assert!(bad.is_err(), "refuses a provider with no key");
    assert_eq!(
        h::get_provider_config().unwrap().active,
        before.active,
        "config untouched on refusal"
    );
}
