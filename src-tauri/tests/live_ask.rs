//! Opt-in: `cargo test --test live_ask -- --ignored --nocapture --test-threads 1`
//!
//! Phase C item 3 against the real machine, through the hub tool an agent calls: Claude Code asks
//! Hermes, and Hermes asks Claude Code, each a short headless turn in the sandbox repository. Both
//! answers come back to the caller and every ask and answer is logged on the mission. The Hermes
//! session the ask created is deleted afterwards (`hermes sessions delete --yes`), and so are the
//! mission and its exchange rows.

use std::process::Command;

use agentos_studio_lib::{
    agent_log, hub_identity::Caller, mcp_stdio, missions, procs, vault::Vault,
};
use serde_json::{json, Value};

const REPO: &str = "agentos-sandbox-1";

fn answer_of(r: anyhow::Result<Value>) -> Value {
    match r {
        Ok(v) => v,
        Err(e) => panic!("ask failed: {e:#}"),
    }
}

#[test]
#[ignore = "runs real Hermes and Claude Code turns"]
fn agents_ask_each_other_and_every_exchange_is_logged() {
    let d = tempfile::tempdir().unwrap();
    let v = Vault::new(d.path().join("vault"));
    v.scaffold().unwrap();
    let m = missions::create(
        REPO,
        "Live ask test",
        "Answers come back to the caller",
        "implementer",
        "claude-code",
        "",
        "",
        0.0,
    )
    .unwrap();

    mcp_stdio::set_thread_caller(Some(Caller::known("claude-code")));
    let a1 = mcp_stdio::call_tool(
        &v,
        "ask_agent",
        &json!({
            "to": "hermes",
            "question": "Read package.json in this folder. What is the exact value of its \"name\" field? Reply with the name only.",
            "mission_id": m.id,
            "timeout": 240
        }),
    );
    mcp_stdio::set_thread_caller(Some(Caller::known("hermes")));
    let a2 = mcp_stdio::call_tool(
        &v,
        "ask_agent",
        &json!({
            "to": "claude-code",
            "question": "In one short sentence: which test command does this repository's package.json define under scripts.test?",
            "mission_id": m.id,
            "timeout": 240
        }),
    );
    // Qoder is IDE only: refused before anything runs.
    let q = mcp_stdio::call_tool(&v, "ask_agent", &json!({"to": "qoder", "question": "hi"}));
    mcp_stdio::set_thread_caller(None);
    let events = agent_log::for_mission(&m.id).unwrap();

    // Clean up first.
    let mut deleted = Vec::new();
    for v in [&a1, &a2].into_iter().flatten() {
        {
            if let Some(sid) = v["session_id"].as_str() {
                let out = Command::new(procs::find("hermes").unwrap())
                    .args(["sessions", "delete", sid, "--yes"])
                    .env("PATH", procs::search_path())
                    .output();
                deleted.push((
                    sid.to_string(),
                    out.map(|o| o.status.success()).unwrap_or(false),
                ));
            }
        }
    }
    let _ = missions::delete(&m.id);
    let _ = agent_log::delete_for_mission(&m.id);

    let a1 = answer_of(a1);
    let a2 = answer_of(a2);
    println!(
        "claude-code asked hermes -> {:?} in {} ms",
        a1["answer"], a1["duration_ms"]
    );
    println!(
        "hermes asked claude-code -> {:?} in {} ms",
        a2["answer"], a2["duration_ms"]
    );
    println!("sessions deleted: {deleted:?}");
    for e in &events {
        println!(
            "{} {} -> {}: {}",
            e.kind,
            e.from_agent,
            e.to_agent,
            e.summary.chars().take(120).collect::<String>()
        );
    }
    assert!(
        a1["answer"].as_str().unwrap().contains("agentos-sandbox"),
        "{a1}"
    );
    assert!(
        a2["answer"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("node"),
        "{a2}"
    );
    assert!(q.unwrap_err().to_string().contains("IDE"));
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["ask", "answer", "ask", "answer"]);
    assert!(events[1].duration_ms.unwrap_or(0) > 0);
    assert!(
        deleted.iter().all(|(_, ok)| *ok),
        "hermes sessions removed: {deleted:?}"
    );
    assert!(missions::get(&m.id).unwrap().is_none());
}
