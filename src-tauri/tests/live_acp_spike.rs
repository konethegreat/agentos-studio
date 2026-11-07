//! ACP spike against the real Hermes install. Creates sessions and deletes them at the end.
//! Run: cargo test --test live_acp_spike -- --ignored --nocapture --test-threads 1
use agentos_studio_lib::acp::{run_turn, AcpEvent, PermissionAsk, TurnOptions, TURN_TIMEOUT};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("acp-spike-{}", std::process::id()));
    std::fs::create_dir_all(d.join("keep-me")).unwrap();
    std::fs::write(d.join("keep-me").join("note.txt"), "do not delete").unwrap();
    d
}

/// The machine's default provider may be broken (a disabled plugin); pin a known-good one.
const PROVIDER: &str = "claude-subscription-directsdk-experimental";
const MODEL: &str = "claude-haiku-5-5[1m]";

fn pinned() -> Option<String> {
    Some(format!("{PROVIDER}:{MODEL}"))
}

fn delete_session(id: &str) {
    let _ = Command::new("hermes")
        .args(["sessions", "delete", id, "--yes"])
        .output();
}

struct Run {
    events: Vec<AcpEvent>,
    outcome: agentos_studio_lib::acp::TurnOutcome,
}

async fn turn(opts: TurnOptions, answer: Option<&'static str>, cancel_after_first: bool) -> Run {
    let (etx, mut erx) = mpsc::unbounded_channel();
    let (ptx, mut prx) = mpsc::unbounded_channel::<PermissionAsk>();
    let (ctx, crx) = oneshot::channel();
    let task = tokio::spawn(tokio::time::timeout(
        TURN_TIMEOUT,
        run_turn(opts, etx, ptx, crx),
    ));
    let mut events = Vec::new();
    let mut ctx = Some(ctx);
    loop {
        tokio::select! {
            Some(e) = erx.recv() => {
                if cancel_after_first && matches!(e, AcpEvent::Text { .. } | AcpEvent::Thought { .. } | AcpEvent::ToolCall { .. }) {
                    if let Some(c) = ctx.take() { println!("  -> cancelling"); let _ = c.send(()); }
                }
                let done = matches!(e, AcpEvent::Done { .. });
                events.push(e);
                if done { break; }
            }
            Some(ask) = prx.recv() => {
                println!("  permission: {} options={:?}", ask.title, ask.options);
                let pick = answer.and_then(|k| ask.options.iter().find(|o| o.kind == k)).map(|o| o.id.clone());
                println!("  answering {pick:?}");
                let _ = ask.reply.send(pick);
            }
            else => break,
        }
    }
    let outcome = task
        .await
        .unwrap()
        .expect("timed out")
        .expect("turn failed");
    Run { events, outcome }
}

fn count(events: &[AcpEvent], f: fn(&AcpEvent) -> bool) -> usize {
    events.iter().filter(|e| f(e)).count()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn acp_spike() {
    let dir = scratch();
    let mut created = Vec::new();

    // 1. New session, one turn with reasoning on.
    let a = turn(
        TurnOptions {
            cwd: dir.clone(),
            prompt: "Reply with exactly the word PONG.".into(),
            reasoning: Some("low".into()),
            model: pinned(),
            ..Default::default()
        },
        None,
        false,
    )
    .await;
    created.push(a.outcome.session_id.clone());
    println!(
        "turn 1: session={} stop={} ttft={:?}ms total={}ms text={:?} thoughts={}",
        a.outcome.session_id,
        a.outcome.stop_reason,
        a.outcome.first_token_ms,
        a.outcome.total_ms,
        a.outcome.text.trim(),
        count(&a.events, |e| matches!(e, AcpEvent::Thought { .. }))
    );
    println!("  phases: {:?}", a.outcome.phases);
    assert!(a.outcome.text.contains("PONG"));

    // 1b. Same thing again, warm: is the cost per process or per session?
    let w = turn(
        TurnOptions {
            cwd: dir.clone(),
            prompt: "Reply with exactly the word PONG.".into(),
            model: pinned(),
            ..Default::default()
        },
        None,
        false,
    )
    .await;
    created.push(w.outcome.session_id.clone());
    println!(
        "turn 1b: ttft={:?}ms total={}ms phases={:?}",
        w.outcome.first_token_ms, w.outcome.total_ms, w.outcome.phases
    );

    // 2. Load the same session and trigger a dangerous command; reject it.
    let b = turn(
        TurnOptions {
            cwd: dir.clone(),
            session_id: Some(a.outcome.session_id.clone()),
            prompt: "Use your terminal tool to run this exact command, nothing else: `rm -rf keep-me` . If it is refused, say REFUSED.".into(),
            ..Default::default()
        },
        Some("reject_once"),
        false,
    )
    .await;
    let asked = count(&b.events, |e| matches!(e, AcpEvent::Permission { .. }));
    println!(
        "turn 2 (loaded): stop={} permissions={asked} tools={} text={:?}",
        b.outcome.stop_reason,
        count(&b.events, |e| matches!(e, AcpEvent::ToolCall { .. })),
        b.outcome.text.trim()
    );
    println!("  keep-me still exists: {}", dir.join("keep-me").exists());
    assert!(asked >= 1, "no request_permission arrived");
    assert!(dir.join("keep-me").exists(), "rejected command ran anyway");

    // 3. Cancel a turn as soon as anything streams.
    let c = turn(
        TurnOptions {
            cwd: dir.clone(),
            prompt: "Write a 600 word essay about rivers.".into(),
            model: pinned(),
            ..Default::default()
        },
        None,
        true,
    )
    .await;
    created.push(c.outcome.session_id.clone());
    println!(
        "turn 3: stop={} chars={}",
        c.outcome.stop_reason,
        c.outcome.text.len()
    );
    assert_eq!(c.outcome.stop_reason, "cancelled");

    // 4. Load a session Studio created through `hermes chat --source gui`.
    let t = Instant::now();
    let mut child = Command::new("hermes")
        .args([
            "chat",
            "-q",
            "Reply with exactly the word PING.",
            "--source",
            "gui",
            "--format",
            "stream-json",
            "--provider",
            PROVIDER,
            "-m",
            MODEL,
        ])
        .current_dir(&dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = None;
    let mut gui_id = String::new();
    {
        use std::io::BufRead;
        let out = std::io::BufReader::new(child.stdout.take().unwrap());
        for line in out.lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if first.is_none() && v.get("type").and_then(|x| x.as_str()) == Some("text") {
                first = Some(t.elapsed().as_millis());
            }
            if let Some(id) = v.get("session_id").and_then(|x| x.as_str()) {
                gui_id = id.to_string();
            }
        }
    }
    let _ = child.wait();
    println!(
        "chat -q: ttft={first:?}ms total={}ms session={gui_id}",
        t.elapsed().as_millis()
    );
    if !gui_id.is_empty() {
        created.push(gui_id.clone());
        let d = turn(
            TurnOptions {
                cwd: dir.clone(),
                session_id: Some(gui_id.clone()),
                prompt: "What word did you just reply with? One word.".into(),
                ..Default::default()
            },
            None,
            false,
        )
        .await;
        println!(
            "turn 4 (gui session over ACP): stop={} ttft={:?} text={:?}",
            d.outcome.stop_reason,
            d.outcome.first_token_ms,
            d.outcome.text.trim()
        );
    }

    for id in created {
        delete_session(&id);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Second probe: reasoning stream, slash commands over ACP, and the session's source tag.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn acp_probe_gaps() {
    let dir = scratch();
    let r = turn(
        TurnOptions {
            cwd: dir.clone(),
            prompt: "Think carefully, then answer: what is 17 * 23? Answer with the number only."
                .into(),
            model: Some(
                std::env::var("ACP_REASONING_MODEL")
                    .unwrap_or_else(|_| format!("{PROVIDER}:claude-sonnet-5-5[1m]")),
            ),
            reasoning: Some("high".into()),
            ..Default::default()
        },
        None,
        false,
    )
    .await;
    let sid = r.outcome.session_id.clone();
    let thoughts: String = r
        .events
        .iter()
        .filter_map(|e| {
            if let AcpEvent::Thought { text } = e {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    println!(
        "reasoning: chars={} text={:?}",
        thoughts.len(),
        r.outcome.text.trim()
    );
    let cmds: &[&str] = if std::env::var("ACP_REASONING_MODEL").is_ok() {
        &[]
    } else {
        &["/rollback", "/reasoning", "/yolo", "/skills"]
    };
    for cmd in cmds {
        let c = turn(
            TurnOptions {
                cwd: dir.clone(),
                session_id: Some(sid.clone()),
                prompt: (*cmd).into(),
                ..Default::default()
            },
            None,
            false,
        )
        .await;
        let reply: String = c.outcome.text.chars().take(240).collect();
        println!("{cmd}: stop={} reply={:?}", c.outcome.stop_reason, reply);
    }
    let db = dirs::data_local_dir()
        .unwrap()
        .join("hermes")
        .join("state.db");
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let src: String = conn
        .query_row("SELECT source FROM sessions WHERE id = ?1", [&sid], |r| {
            r.get(0)
        })
        .unwrap_or_else(|e| format!("not found: {e}"));
    println!("state.db source for ACP session: {src}");
    delete_session(&sid);
    let _ = std::fs::remove_dir_all(&dir);
}
