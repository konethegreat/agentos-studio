//! End-to-end chat turns through the real CLIs, using the exact arguments and parsers the
//! Chat Studio uses. Run with `cargo test --test live_chat -- --ignored --nocapture`.

use std::io::Write;
use std::process::Stdio;

use agentos_studio_lib::chat_stream::{
    build_args, parse_hermes_line, ChatEvent, ChatRequest, ClaudeParser,
};
use agentos_studio_lib::procs;

fn run(tool: &str, args: &[String], cwd: &std::path::Path, stdin: Option<&str>) -> String {
    let exe = procs::find(tool).expect("tool on PATH");
    let mut child = procs::command(&exe)
        .args(args)
        .current_dir(cwd)
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    if let (Some(t), Some(mut s)) = (stdin, child.stdin.take()) {
        s.write_all(t.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().expect("wait");
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn kinds(ev: &[ChatEvent]) -> Vec<String> {
    ev.iter().map(|e| e.kind.clone()).collect()
}

#[test]
#[ignore = "needs Hermes with the agentos-hub MCP server"]
fn hermes_turn_streams_tools_and_resumes() {
    let cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let d = tempfile::tempdir().unwrap();
    let q = d.path().join("q.md");
    std::fs::write(&q, "Call the agentos-hub tool fetch_pending_tasks with agent_name all, then reply with exactly: OPEN <count>").unwrap();
    let req = ChatRequest {
        session_id: None,
        prompt: String::new(),
        working_dir: cwd.display().to_string(),
        agent_backend: "hermes".into(),
        model: None,
    };
    let text = run("hermes", &build_args("hermes", &req, &q, &cwd), &cwd, None);
    let ev: Vec<ChatEvent> = text.lines().flat_map(parse_hermes_line).collect();
    println!("turn 1 kinds: {:?}", kinds(&ev));
    assert!(
        ev.iter()
            .any(|e| e.kind == "tool_start" && e.server.as_deref() == Some("agentos_hub")),
        "an agentos-hub tool card"
    );
    assert!(ev
        .iter()
        .any(|e| e.kind == "tool_result" && e.duration_ms.is_some()));
    let done = ev.iter().find(|e| e.kind == "complete").expect("complete");
    let sid = done.session_id.clone().expect("session id");
    println!("turn 1 answer: {:?} session {sid}", done.text);
    assert!(done.text.as_deref().unwrap_or("").contains("OPEN"));

    std::fs::write(
        &q,
        "What exact word did your previous reply start with? Answer with that single word only.",
    )
    .unwrap();
    let req2 = ChatRequest {
        session_id: Some(sid.clone()),
        ..req
    };
    let text2 = run("hermes", &build_args("hermes", &req2, &q, &cwd), &cwd, None);
    let ev2: Vec<ChatEvent> = text2.lines().flat_map(parse_hermes_line).collect();
    let done2 = ev2
        .iter()
        .find(|e| e.kind == "complete")
        .expect("complete 2");
    println!(
        "turn 2 answer: {:?} session {:?}",
        done2.text, done2.session_id
    );
    assert!(
        done2
            .text
            .as_deref()
            .unwrap_or("")
            .to_uppercase()
            .contains("OPEN"),
        "resume kept context"
    );
    let msgs = agentos_studio_lib::hermes_engine::load_session_messages(
        done2.session_id.as_deref().unwrap_or(&sid),
    )
    .unwrap();
    println!("stored messages in session: {}", msgs.len());
    let listed = agentos_studio_lib::hermes_engine::list_hermes_sessions(Some(&sid), 5).unwrap();
    println!(
        "listed source: {:?}",
        listed.first().map(|s| s.source.clone())
    );

    // Rename, export to a scratch vault, then delete: the sidebar actions, against real state.db.
    use agentos_studio_lib::hermes_engine as h;
    h::rename_session(&sid, "Studio live test session").unwrap();
    let renamed = h::list_hermes_sessions(Some(&sid), 5).unwrap();
    assert_eq!(renamed[0].title, "Studio live test session");
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = agentos_studio_lib::vault::Vault::new(vault_dir.path());
    vault.scaffold().unwrap();
    let rel = h::export_session_to_obsidian(&sid, &vault).unwrap();
    let md = std::fs::read_to_string(vault_dir.path().join(&rel)).unwrap();
    println!("exported {rel} ({} bytes)", md.len());
    assert!(md.contains("type: hermes-session") && md.contains("fetch_pending_tasks"));
    h::delete_session(&sid).unwrap();
    assert!(
        h::list_hermes_sessions(Some(&sid), 5).unwrap().is_empty(),
        "deleted"
    );
}

#[test]
#[ignore = "needs a signed-in Claude Code"]
fn claude_turn_streams_partial_text_and_tools() {
    let cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let req = ChatRequest {
        session_id: None,
        prompt: String::new(),
        working_dir: cwd.display().to_string(),
        agent_backend: "claude-code".into(),
        model: None,
    };
    let prompt = "Use the agentos-hub search_shared_memory tool with query fleet, then reply with exactly: HITS <number of results>";
    let text = run(
        "claude",
        &build_args("claude-code", &req, std::path::Path::new("unused"), &cwd),
        &cwd,
        Some(prompt),
    );
    let mut p = ClaudeParser::default();
    let ev: Vec<ChatEvent> = text.lines().flat_map(|l| p.parse(l)).collect();
    let k = kinds(&ev);
    println!(
        "claude kinds: {} tokens, {:?}",
        k.iter().filter(|x| *x == "token").count(),
        k.iter().filter(|x| *x != "token").collect::<Vec<_>>()
    );
    let done = ev.iter().find(|e| e.kind == "complete").expect("complete");
    println!("claude answer: {:?} cost {:?}", done.text, done.cost_usd);
    assert!(ev
        .iter()
        .any(|e| e.kind == "tool_start" && e.server.as_deref() == Some("agentos-hub")));
    assert!(ev.iter().any(|e| e.kind == "tool_result"));
    assert!(done.text.as_deref().unwrap_or("").contains("HITS"));
}
