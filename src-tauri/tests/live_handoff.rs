//! Opt-in: `cargo test --test live_handoff -- --ignored --nocapture --test-threads 1`
//!
//! Phase C item 2 against the real machine: a Claude Code mission in the `agentos-sandbox-1`
//! fixture repository gets its worktree and bus task through `missions::start`, Claude Code makes
//! a change there, then hands the mission to Hermes through the same hub tool an agent calls
//! (`call_tool("handoff")` as the verified caller claude-code). The packet must carry the brief,
//! acceptance criteria, the real diff and the sender's notes; the mission moves to Hermes on a new
//! task in the same worktree; the sender's task is closed; the exchange is logged.
//!
//! The bus is a temporary vault, so the running app's watcher never starts a worker for it.
//! Everything is removed afterwards: worktree and branch, mission, exchange rows.

use agentos_studio_lib::{
    agent_log, bus, hub_identity::Caller, mcp_stdio, missions, review_agent, vault::State,
    vault::Vault, worktree,
};
use serde_json::{json, Value};

const REPO: &str = "agentos-sandbox-1";

#[test]
#[ignore = "creates a real worktree in the sandbox repository"]
fn a_handoff_reaches_the_receiver_with_a_full_packet() {
    let d = tempfile::tempdir().unwrap();
    let v = Vault::new(d.path().join("vault"));
    v.scaffold().unwrap();
    let dir = review_agent::project_dir(REPO).expect("agentos-sandbox-1 is a Hermes project");
    let m = missions::create(
        REPO,
        "Add a subtract(a, b) function to lib.js with a test, then document it",
        "lib.js exports subtract; test.js covers it; README mentions it; npm test passes",
        "implementer",
        "claude-code",
        "claude-subscription",
        "",
        0.0,
    )
    .unwrap();
    println!("mission {}", m.id);
    let started = missions::start(&v, &m.id).expect("start");
    let first = started.task_id.clone().unwrap();
    v.claim_task(&first, "claude-code").unwrap();
    let wt = worktree::recorded(&m.id).expect("worktree");
    // The sender's work so far: a real uncommitted change in the worktree.
    let lib = wt.join("lib.js");
    let mut text = std::fs::read_to_string(&lib).unwrap_or_default();
    text.push_str(
        "\nfunction subtract(a, b) { return a - b; }\nmodule.exports.subtract = subtract;\n",
    );
    std::fs::write(&lib, text).unwrap();

    mcp_stdio::set_thread_caller(Some(Caller::known("claude-code")));
    let out = mcp_stdio::call_tool(
        &v,
        "handoff",
        &json!({
            "to": "hermes",
            "mission_id": m.id,
            "summary": "subtract is in lib.js, not committed yet",
            "next_step": "Add the test in test.js and a README line, then open the PR",
            "files": ["lib.js", "test.js"],
            "questions": ["Should README list every export?"]
        }),
    );
    // A different agent cannot hand off a mission it does not hold.
    mcp_stdio::set_thread_caller(Some(Caller::known("antigravity")));
    let refused = mcp_stdio::call_tool(
        &v,
        "handoff",
        &json!({"to": "claude-code", "mission_id": m.id, "summary": "x", "next_step": "y"}),
    );
    mcp_stdio::set_thread_caller(None);

    let after = missions::get(&m.id).unwrap().unwrap();
    let events = agent_log::for_mission(&m.id).unwrap();
    let new_task = after.task_id.clone().unwrap_or_default();
    let body = v
        .get_task(&new_task)
        .map(|(t, _)| t.body)
        .unwrap_or_default();
    let first_state = v.task_state(&first);
    let receiver_dir = bus::workdir_from_body(&body);
    println!("handoff -> {out:?}");
    println!("refused -> {refused:?}");
    println!("new task {new_task}, packet {} chars", body.len());
    println!("{}", body.chars().take(1600).collect::<String>());

    // Clean up before asserting.
    let _ = worktree::discard(&dir, &m.id);
    let _ = missions::block(&m.id, "live test cleanup", true);
    let _ = missions::delete(&m.id);
    let _ = agent_log::delete_for_mission(&m.id);

    let out: Value = out.expect("handoff succeeded");
    assert_eq!(out["to"], "hermes");
    assert!(refused.unwrap_err().to_string().contains("held by"));
    assert_eq!(after.agent, "hermes");
    assert_eq!(after.status, "running");
    assert_ne!(new_task, first);
    assert_eq!(
        first_state,
        Some(State::Completed),
        "the sender's task is closed"
    );
    for want in [
        "HANDOFF from claude-code to hermes",
        "README mentions it",
        "subtract is in lib.js",
        "Add the test in test.js",
        "- Should README list every export?",
        "function subtract(a, b)",
        "## Project brief",
    ] {
        assert!(body.contains(want), "packet is missing {want:?}");
    }
    assert_eq!(receiver_dir.as_deref(), Some(wt.as_path()), "same worktree");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "handoff");
    assert_eq!(events[0].from_agent, "claude-code");
    assert!(!wt.exists(), "worktree removed");
    assert!(missions::get(&m.id).unwrap().is_none(), "mission removed");
}
