//! Checkpoints and undo against the real Hermes install: a Studio-style chat turn with
//! `--checkpoints` edits a file, then the Ink TUI opened on that session restores it with
//! `/rollback`. Creates one session in a scratch folder and deletes it.
//! Run: cargo test --test live_checkpoints -- --ignored --nocapture --test-threads 1
use agentos_studio_lib::pty;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PROVIDER: &str = "claude-subscription-directsdk-experimental";
const MODEL: &str = "claude-haiku-5-5[1m]";

fn strip_ansi(s: &str) -> String {
    let re = regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07|\x1b[()][0-9A-Za-z]")
        .unwrap();
    re.replace_all(s, "").to_string()
}

fn wait_for(out: &Arc<Mutex<String>>, needle: &str, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if strip_ansi(&out.lock().unwrap())
            .to_lowercase()
            .contains(needle)
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    false
}

#[test]
#[ignore]
fn checkpointed_turn_rolls_back_in_the_tui() {
    let dir = std::env::temp_dir().join(format!("ckpt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("hello.txt");
    std::fs::write(&file, "original").unwrap();

    // The same arguments chat_stream builds for a new Hermes chat, plus a pinned model.
    let req = agentos_studio_lib::chat_stream::ChatRequest {
        session_id: None,
        prompt: String::new(),
        working_dir: dir.display().to_string(),
        agent_backend: "hermes".into(),
        model: None,
        skills: vec![],
        turn: None,
    };
    let qf = dir.join("q.txt");
    std::fs::write(&qf, "Use the write_file tool to replace the whole content of hello.txt with the word CHANGED. Then reply DONE.").unwrap();
    let mut args = agentos_studio_lib::chat_stream::build_args("hermes", &req, &qf, &dir);
    assert!(
        args.contains(&"--checkpoints".to_string()),
        "new sessions must enable checkpoints"
    );
    args.extend([
        "--provider".into(),
        PROVIDER.into(),
        "-m".into(),
        MODEL.into(),
    ]);
    let out = Command::new("hermes")
        .args(&args)
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let sid = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| {
            v.get("session_id")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .expect("session id");
    println!(
        "session {sid}; file now {:?}",
        std::fs::read_to_string(&file).unwrap()
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), "CHANGED");

    // A skill picked after the chat started: resume args carry no -s, the prompt invokes it.
    let resume = agentos_studio_lib::chat_stream::ChatRequest {
        session_id: Some(sid.clone()),
        skills: vec!["obsidian".into()],
        ..req.clone()
    };
    let (prompt, _) = agentos_studio_lib::chat_stream::hermes_resume_prompt(
        "Reply with exactly SKILLOK.",
        &resume.skills,
        Some(&[]),
    );
    std::fs::write(&qf, &prompt).unwrap();
    let rargs = agentos_studio_lib::chat_stream::build_args("hermes", &resume, &qf, &dir);
    let r = Command::new("hermes")
        .args(&rargs)
        .current_dir(&dir)
        .output()
        .unwrap();
    let rout = String::from_utf8_lossy(&r.stdout);
    let result = rout
        .lines()
        .rev()
        .find(|l| l.contains("\"result\""))
        .unwrap_or("")
        .to_string();
    println!(
        "resume with /obsidian prefix: {}",
        result.chars().take(300).collect::<String>()
    );
    assert!(
        result.contains("\"exit_code\": 0"),
        "resume with an added skill failed"
    );

    let state = pty::PtyState::default();
    let buf = Arc::new(Mutex::new(String::new()));
    let b = buf.clone();
    let sink: pty::Sink = Arc::new(move |ev, payload| {
        if ev == pty::EV_PTY_DATA {
            b.lock()
                .unwrap()
                .push_str(payload["data"].as_str().unwrap_or(""));
        }
    });
    let info = pty::spawn_with(
        sink,
        &state,
        pty::SpawnRequest {
            profile: "hermes-tui-resume".into(),
            session_id: Some(sid.clone()),
            cwd: Some(dir.display().to_string()),
            cols: 140,
            rows: 40,
            type_when_ready: Some("/rollback".into()),
        },
    )
    .expect("spawn tui");
    // ConPTY asks for the cursor position and waits; xterm.js answers in the app, the test here.
    {
        let (b, id) = (buf.clone(), info.id.clone());
        let st = state.clone();
        std::thread::spawn(move || {
            let mut answered = 0;
            let end = Instant::now() + Duration::from_secs(150);
            while Instant::now() < end {
                let asked = b.lock().unwrap().matches("\x1b[6n").count();
                while answered < asked {
                    let _ = pty::write(&st, &id, "\x1b[1;1R");
                    answered += 1;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }
    let typed = wait_for(&buf, "/rollback", 60);
    println!("TUI shows the typed /rollback: {typed}");
    std::thread::sleep(Duration::from_secs(2));
    pty::write(&state, &info.id, "\r").unwrap();
    let listed = wait_for(&buf, "checkpoint", 30);
    let screen = strip_ansi(&buf.lock().unwrap());
    let tail: String = screen
        .chars()
        .rev()
        .take(1500)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    println!("listed={listed}\n--- screen tail ---\n{tail}\n---");
    pty::write(&state, &info.id, "/rollback 1\r").unwrap();
    let end = Instant::now() + Duration::from_secs(40);
    while Instant::now() < end && std::fs::read_to_string(&file).unwrap().trim() == "CHANGED" {
        std::thread::sleep(Duration::from_millis(500));
    }
    let after = std::fs::read_to_string(&file).unwrap();
    println!("after /rollback 1: {after:?}");
    let _ = pty::kill(&state, &info.id);
    let _ = Command::new("hermes")
        .args(["sessions", "delete", &sid, "--yes"])
        .output();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(typed, "the TUI never showed the typed command");
    assert_eq!(
        after.trim(),
        "original",
        "rollback did not restore the file"
    );
}
