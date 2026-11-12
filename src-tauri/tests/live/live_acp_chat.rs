//! Hermes chat over ACP with creation policy: create the session with interactive `hermes chat`
//! (model, checkpoints), run turns on one long-lived `hermes acp`, deny an approval, measure a warm
//! turn, cancel a turn. Deletes the session it creates.
//! Run: cargo test --test live -- live_acp_chat:: --ignored --nocapture
use agentos_studio_lib::acp::AcpEvent;
use agentos_studio_lib::acp_host;
use std::process::Command;
use std::time::{Duration, Instant};

const MODEL: &str = "claude-haiku-5-5[1m]";
const PROVIDER: &str = "claude-subscription-directsdk-experimental";

struct Ran {
    text: String,
    thoughts: usize,
    asked: Vec<String>,
    first_ms: Option<u128>,
    setup_ms: u128,
    stop: String,
}

async fn turn(
    sid: &str,
    dir: &std::path::Path,
    prompt: &str,
    cancel_after: Option<Duration>,
) -> Ran {
    let t0 = Instant::now();
    let mut t = acp_host::start_turn(sid, dir, prompt)
        .await
        .expect("start turn");
    let mut cancel = Some(t.cancel);
    let mut ran = Ran {
        text: String::new(),
        thoughts: 0,
        asked: vec![],
        first_ms: None,
        setup_ms: t.setup_ms,
        stop: String::new(),
    };
    let deadline = tokio::time::sleep(cancel_after.unwrap_or(Duration::from_secs(3600)));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            Some(e) = t.events.recv() => match e {
                AcpEvent::Text { text } => { ran.first_ms.get_or_insert(t0.elapsed().as_millis()); ran.text.push_str(&text) }
                AcpEvent::Thought { .. } => { ran.first_ms.get_or_insert(t0.elapsed().as_millis()); ran.thoughts += 1 }
                _ => {}
            },
            Some(p) = t.permissions.recv() => {
                ran.asked.push(p.title.clone());
                let deny = p.options.iter().find(|o| o.kind == "reject_once").map(|o| o.id.clone());
                let _ = p.reply.send(deny);
            },
            _ = &mut deadline, if cancel.is_some() => { let _ = cancel.take().unwrap().send(()); },
            r = &mut t.done => { ran.stop = r.unwrap().unwrap_or_else(|e| format!("error: {e}")); break; }
        }
    }
    ran
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn acp_chat_with_creation_policy() {
    let dir = std::env::temp_dir().join(format!("acp-chat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A recursive delete in the home folder, which the smart-approval guardian escalates.
    let target = dirs::home_dir()
        .unwrap()
        .join(format!("acp-approval-probe-{}", std::process::id()));
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("x.txt"), "x").unwrap();
    let target_s = target.display().to_string().replace('\\', "/");

    // approvals.mode smart lets Hermes' guardian approve on its own; manual makes it always ask.
    let cfg = dirs::data_local_dir()
        .unwrap()
        .join("hermes")
        .join("config.yaml");
    let cfg_before = std::fs::read(&cfg).unwrap();
    let set = Command::new("hermes")
        .args(["config", "set", "approvals.mode", "manual"])
        .output()
        .unwrap();
    assert!(set.status.success());
    let t0 = Instant::now();
    let d2 = dir.clone();
    let sid = tokio::task::spawn_blocking(move || {
        acp_host::create_session(&d2, Some(MODEL), Some(PROVIDER), &[])
    })
    .await
    .unwrap()
    .expect("create session");
    println!("created {sid} in {} ms", t0.elapsed().as_millis());

    let a = turn(&sid, &dir, &format!("Run exactly this command with the terminal tool, nothing else: rm -rf \"{target_s}\" (nothing else). If it is denied, reply DENIED and stop."), None).await;
    println!(
        "turn 1: stop={} setup={}ms first={:?}ms thoughts={} asked={:?}\n  {}",
        a.stop,
        a.setup_ms,
        a.first_ms,
        a.thoughts,
        a.asked,
        a.text.trim().chars().take(160).collect::<String>()
    );
    let b = turn(&sid, &dir, "Reply with the single word pong.", None).await;
    println!(
        "turn 2 (warm): stop={} setup={}ms first={:?}ms text={:?}",
        b.stop,
        b.setup_ms,
        b.first_ms,
        b.text.trim()
    );
    let c = turn(
        &sid,
        &dir,
        "Count slowly from 1 to 400, one number per line, with a short sentence after each.",
        Some(Duration::from_secs(6)),
    )
    .await;
    println!("turn 3 (cancel): stop={} chars={}", c.stop, c.text.len());

    // Turn 4: an approval nobody answers is denied by Studio's timer, and Hermes honours it.
    let target2 = dirs::home_dir()
        .unwrap()
        .join(format!("acp-timeout-probe-{}", std::process::id()));
    std::fs::create_dir_all(&target2).unwrap();
    std::fs::write(target2.join("x.txt"), "x").unwrap();
    let t2s = target2.display().to_string().replace('\\', "/");
    let pending: std::sync::Arc<
        std::sync::Mutex<
            std::collections::HashMap<String, tokio::sync::oneshot::Sender<Option<String>>>,
        >,
    > = Default::default();
    let mut t4 = acp_host::start_turn(&sid, &dir, &format!("Run exactly this command with the terminal tool, nothing else: rm -rf \"{t2s}\" (nothing else). If it is denied, reply DENIED and stop.")).await.expect("turn 4");
    let mut expired = Vec::new();
    let mut timers = Vec::new();
    let stop4 = loop {
        tokio::select! {
            Some(_) = t4.events.recv() => {},
            Some(p) = t4.permissions.recv() => {
                let deny = agentos_studio_lib::chat_stream::timeout_choice(&p.options);
                pending.lock().unwrap().insert(p.request_id.clone(), p.reply);
                let (pm, rid) = (pending.clone(), p.request_id.clone());
                timers.push(tokio::spawn(async move {
                    agentos_studio_lib::chat_stream::expire_after(&pm, &rid, Duration::from_secs(10), deny).await
                }));
            },
            r = &mut t4.done => break r.unwrap().unwrap_or_else(|e| format!("error: {e}")),
        }
    };
    for h in timers {
        expired.push(h.await.unwrap());
    }
    let survived2 = target2.exists();
    let _ = std::fs::remove_dir_all(&target2);
    println!("turn 4 (timeout): stop={stop4} expired={expired:?} survived={survived2}");

    let model = Command::new("python")
        .args(["-I", "-c", &format!("import sqlite3;c=sqlite3.connect('file:'+__import__('os').environ['LOCALAPPDATA'].replace(chr(92),'/')+'/hermes/state.db?mode=ro',uri=True);print(c.execute('select model from sessions where id=?',('{sid}',)).fetchone())")])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("stored model: {model}");
    acp_host::reset();
    std::fs::write(&cfg, &cfg_before).unwrap();
    assert_eq!(
        std::fs::read(&cfg).unwrap(),
        cfg_before,
        "config.yaml restored"
    );
    let _ = Command::new("hermes")
        .args(["sessions", "delete", &sid, "--yes"])
        .output();
    let survived = target.exists();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&target);

    assert!(!a.asked.is_empty(), "approval was asked");
    assert!(survived, "denied delete did not run");
    assert_eq!(b.stop, "end_turn");
    assert!(b.text.to_lowercase().contains("pong"));
    assert_eq!(c.stop, "cancelled");
    assert!(model.contains(MODEL), "model fixed at creation: {model}");
    assert!(
        !expired.is_empty() && expired.iter().all(|e| *e),
        "approval denied by the timer"
    );
    assert!(survived2, "timed-out delete did not run");
    assert_eq!(stop4, "end_turn");
}
