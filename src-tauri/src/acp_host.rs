//! Hermes chat over the Agent Client Protocol, with creation policy intact.
//!
//! ACP's `session/new` cannot carry a model, skills or checkpoints (see docs/ACP-SPIKE.md), so a
//! new chat is created the way a person creates one: interactive `hermes chat` with those flags.
//! It stores the session with its policy and prints `Session: <id>` in its banner before any turn;
//! Studio reads the id and ends that process. Every turn then runs over one long-lived
//! `hermes acp` connection, which gives Studio what `chat -q` cannot: dangerous-command approvals
//! asked instead of silently denied, a real cancel, and the reasoning stream.
//!
//! Hermes still owns execution: the gateway runs the agent, stores the session and decides what
//! needs approval. Studio is the viewer that answers.

use crate::acp::{debug_name, events_for, kind_name, AcpEvent, PermissionAsk, PermissionChoice};
use crate::procs;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    CancelNotification, ContentBlock, InitializeRequest, LoadSessionRequest, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, TextContent,
};
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// `studio.db` kv: `acp` (default) or `cli` (always `hermes chat -q`).
pub const KV_TRANSPORT: &str = "chat.hermes_transport";

pub fn enabled() -> bool {
    crate::store::open()
        .ok()
        .and_then(|c| crate::store::kv_get(&c, KV_TRANSPORT).ok().flatten())
        .map_or(true, |v| v.trim() != "cli")
}

pub fn set_enabled(on: bool) -> Result<()> {
    let c = crate::store::open()?;
    crate::store::kv_set(&c, KV_TRANSPORT, if on { "acp" } else { "cli" })
}

// ------------------------------------------------------------------- session creation

/// The id in the interactive banner line `Session: 611fceac`. Pure.
pub fn banner_session(line: &str) -> Option<String> {
    let rest = &line[line.find("Session:")? + "Session:".len()..];
    let id: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (id.len() >= 6).then_some(id)
}

/// Arguments for creating a session with its policy. Pure.
pub fn create_args(
    cwd: &Path,
    model: Option<&str>,
    provider: Option<&str>,
    skills: &[String],
) -> Vec<String> {
    let mut a: Vec<String> = [
        "chat",
        "--source",
        crate::chat_stream::STUDIO_SOURCE,
        "--checkpoints",
    ]
    .map(String::from)
    .to_vec();
    a.extend(["--in".into(), cwd.display().to_string()]);
    if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
        a.extend(["-m".into(), m.into()]);
        if let Some(p) = provider.map(str::trim).filter(|p| !p.is_empty()) {
            a.extend(["--provider".into(), p.into()]);
        }
    }
    let skills: Vec<&str> = skills
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if !skills.is_empty() {
        a.extend(["-s".into(), skills.join(",")]);
    }
    a
}

/// One `sessions` row, as far as matching needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub source: String,
    pub started_at: f64,
    /// Hermes may fill this in after creation (git metadata runs in the background).
    pub cwd: Option<String>,
}

/// `s` with LIKE's wildcards and the escape character escaped, for `LIKE ? ESCAPE '\'`. Pure.
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn norm_dir(p: &str) -> String {
    p.trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// The session Studio just created: its id starts with the banner's short id (with or without the
/// `local-` prefix), it is a Studio (`gui`) session, it started after the spawn (2 s clock slack),
/// and its folder is the requested one or not recorded yet. Newest wins. Pure.
pub fn match_session(
    short: &str,
    spawned_at: f64,
    cwd: &Path,
    rows: &[SessionRow],
) -> Option<String> {
    let want = norm_dir(&cwd.display().to_string());
    rows.iter()
        .filter(|r| {
            let bare = r.id.strip_prefix("local-").unwrap_or(&r.id);
            bare.starts_with(short)
                && r.source == crate::chat_stream::STUDIO_SOURCE
                && r.started_at >= spawned_at - 2.0
                && r.cwd
                    .as_deref()
                    .map_or(true, |c| c.trim().is_empty() || norm_dir(c) == want)
        })
        .max_by(|a, b| a.started_at.total_cmp(&b.started_at))
        .map(|r| r.id.clone())
}

fn session_rows(short: &str, since: f64) -> Result<Vec<SessionRow>> {
    let db = crate::hermes_engine::state_db();
    let c = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut st = c.prepare(
        "SELECT id, COALESCE(source, ''), COALESCE(started_at, 0), cwd FROM sessions
         WHERE (id LIKE ?1 ESCAPE '\\' OR id LIKE ?2 ESCAPE '\\') AND started_at >= ?3",
    )?;
    let e = like_escape(short);
    let rows = st.query_map(
        rusqlite::params![format!("local-{e}%"), format!("{e}%"), since - 2.0],
        |r| {
            Ok(SessionRow {
                id: r.get(0)?,
                source: r.get(1)?,
                started_at: r.get(2)?,
                cwd: r.get(3)?,
            })
        },
    )?;
    Ok(rows.flatten().collect())
}

/// Full stored id for the banner's short id. Polls `state.db` for up to 5 s, since the row can
/// land a moment after the banner prints.
fn resolve_session(short: &str, spawned_at: f64, cwd: &Path) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(id) = match_session(short, spawned_at, cwd, &session_rows(short, spawned_at)?) {
            return Ok(id);
        }
        if Instant::now() >= deadline {
            bail!("Hermes printed session {short}, but no matching Studio session appeared in state.db within 5 s");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Create a Hermes session in `cwd` with its creation policy and no turn. Blocking.
pub fn create_session(
    cwd: &Path,
    model: Option<&str>,
    provider: Option<&str>,
    skills: &[String],
) -> Result<String> {
    let exe =
        procs::find("hermes").context("hermes is not installed. Open Setup to install it.")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(create_args(cwd, model, provider, skills))
        .current_dir(cwd)
        .env("PATH", procs::search_path())
        .env("PYTHONIOENCODING", "utf-8")
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(procs::CREATE_NO_WINDOW);
    }
    let spawned_at = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
    let mut child = cmd.spawn().context("starting hermes chat")?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let tail = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut readers = Vec::new();
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let (tx, tail) = (tx.clone(), tail.clone());
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(|l| l.ok()) {
                let clean = procs::clean_line(&line);
                if let Some(id) = banner_session(&clean) {
                    let _ = tx.send(Ok(id));
                    return;
                }
                let t = clean
                    .trim()
                    .trim_matches(|c: char| !c.is_ascii_graphic() && c != ' ');
                if !t.is_empty() && !t.starts_with("Skipping unreadable plugin") {
                    if let Ok(mut v) = tail.lock() {
                        v.push(t.to_string());
                        let n = v.len();
                        if n > 6 {
                            v.drain(..n - 6);
                        }
                    }
                }
            }
        }));
    }
    drop(tx);
    let got = rx.recv_timeout(Duration::from_secs(90));
    // The session is stored before the banner prints; the interactive process is not needed.
    procs::kill_tree(pid);
    let _ = child.wait();
    match got {
        Ok(Ok(short)) => resolve_session(&short, spawned_at, cwd),
        _ => {
            let t = tail.lock().map(|v| v.join("\n")).unwrap_or_default();
            bail!(
                "Hermes did not create a session{}",
                if t.is_empty() {
                    String::new()
                } else {
                    format!(": {t}")
                }
            )
        }
    }
}

// ------------------------------------------------------------------- the connection

struct Route {
    events: mpsc::UnboundedSender<AcpEvent>,
    permissions: mpsc::UnboundedSender<PermissionAsk>,
    /// False while `session/load` replays history, which is not part of this turn.
    live: bool,
}

type Routes = Arc<Mutex<HashMap<String, Route>>>;

struct TurnRequest {
    session_id: String,
    cwd: PathBuf,
    prompt: String,
    events: mpsc::UnboundedSender<AcpEvent>,
    permissions: mpsc::UnboundedSender<PermissionAsk>,
    cancel: oneshot::Receiver<()>,
    ready: oneshot::Sender<Result<(), String>>,
    done: oneshot::Sender<Result<String, String>>,
}

static HOST: OnceLock<Mutex<Option<mpsc::UnboundedSender<TurnRequest>>>> = OnceLock::new();

fn host() -> &'static Mutex<Option<mpsc::UnboundedSender<TurnRequest>>> {
    HOST.get_or_init(|| Mutex::new(None))
}

/// Start the connection now if it is not running. Errors surface on the next turn instead.
pub fn warm() {
    let _ = sender();
}

/// Drop the connection: `hermes acp` is killed and any turn on it ends. The next turn reconnects.
pub fn reset() {
    if let Ok(mut h) = host().lock() {
        *h = None;
    }
}

fn sender() -> Result<mpsc::UnboundedSender<TurnRequest>> {
    let mut h = host()
        .lock()
        .map_err(|_| anyhow!("ACP host lock poisoned"))?;
    if let Some(tx) = h.as_ref().filter(|t| !t.is_closed()) {
        return Ok(tx.clone());
    }
    let exe = procs::find("hermes").context("hermes is not installed")?;
    let agent = AcpAgent::new(
        AcpAgentConfig::new(exe)
            .arg("acp")
            .env("PATH", procs::search_path().to_string_lossy().to_string())
            .env("PYTHONIOENCODING", "utf-8"),
    );
    let (tx, rx) = mpsc::unbounded_channel();
    tauri::async_runtime::spawn(run(agent, rx));
    *h = Some(tx.clone());
    Ok(tx)
}

async fn run(agent: AcpAgent, rx: mpsc::UnboundedReceiver<TurnRequest>) {
    let routes: Routes = Arc::default();
    let (rn, rp, rm) = (routes.clone(), routes.clone(), routes.clone());
    let result = agent_client_protocol::Client
        .v1()
        .name("agentos-studio")
        .on_receive_notification(
            async move |n: SessionNotification, _cx| {
                if let Ok(r) = rn.lock() {
                    if let Some(route) = r.get(&n.session_id.0.to_string()).filter(|r| r.live) {
                        for e in events_for(&n.update) {
                            let _ = route.events.send(e);
                        }
                    }
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, cx: ConnectionTo<Agent>| {
                let sinks = rp.lock().ok().and_then(|r| {
                    r.get(&req.session_id.0.to_string())
                        .map(|x| (x.events.clone(), x.permissions.clone()))
                });
                let request_id = req.tool_call.tool_call_id.0.to_string();
                let title = req
                    .tool_call
                    .fields
                    .title
                    .clone()
                    .unwrap_or_else(|| "Hermes asks for permission".into());
                let options: Vec<PermissionChoice> = req
                    .options
                    .iter()
                    .map(|o| PermissionChoice {
                        id: o.option_id.0.to_string(),
                        name: o.name.clone(),
                        kind: kind_name(&o.kind),
                    })
                    .collect();
                let (tx, rx) = oneshot::channel();
                let asked = sinks.is_some_and(|(ev, perm)| {
                    let _ = ev.send(AcpEvent::Permission {
                        request_id: request_id.clone(),
                        title: title.clone(),
                        options: options.clone(),
                    });
                    perm.send(PermissionAsk {
                        request_id,
                        title,
                        options,
                        reply: tx,
                    })
                    .is_ok()
                });
                cx.spawn(async move {
                    let choice = if asked { rx.await.ok().flatten() } else { None };
                    responder.respond(RequestPermissionResponse::new(match choice {
                        Some(id) => {
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id))
                        }
                        None => RequestPermissionOutcome::Cancelled,
                    }))
                })?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, async move |cx: ConnectionTo<Agent>| {
            cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let loaded: Arc<Mutex<HashSet<String>>> = Arc::default();
            let mut rx = rx;
            while let Some(t) = rx.recv().await {
                let (cx2, routes, loaded) = (cx.clone(), rm.clone(), loaded.clone());
                cx.spawn(async move {
                    turn(cx2, t, routes, loaded).await;
                    Ok(())
                })?;
            }
            Ok(())
        })
        .await;
    if let Err(e) = result {
        eprintln!("hermes acp connection ended: {e}");
    }
    reset();
}

async fn turn(
    cx: ConnectionTo<Agent>,
    t: TurnRequest,
    routes: Routes,
    loaded: Arc<Mutex<HashSet<String>>>,
) {
    let TurnRequest {
        session_id: sid,
        cwd,
        prompt,
        events,
        permissions,
        cancel,
        ready,
        done,
    } = t;
    if let Ok(mut r) = routes.lock() {
        r.insert(
            sid.clone(),
            Route {
                events,
                permissions,
                live: false,
            },
        );
    }
    let drop_route = |routes: &Routes| {
        if let Ok(mut r) = routes.lock() {
            r.remove(&sid);
        }
    };
    let need_load = loaded.lock().map(|l| !l.contains(&sid)).unwrap_or(true);
    if need_load {
        let load = cx
            .send_request(LoadSessionRequest::new(sid.clone(), cwd))
            .block_task();
        match tokio::time::timeout(Duration::from_secs(90), load).await {
            Ok(Ok(_)) => {
                if let Ok(mut l) = loaded.lock() {
                    l.insert(sid.clone());
                }
            }
            Ok(Err(e)) => {
                drop_route(&routes);
                let _ = ready.send(Err(format!("session/load: {e}")));
                return;
            }
            Err(_) => {
                drop_route(&routes);
                let _ = ready.send(Err("session/load timed out".into()));
                return;
            }
        }
    }
    if let Ok(mut r) = routes.lock() {
        if let Some(route) = r.get_mut(&sid) {
            route.live = true;
        }
    }
    let _ = ready.send(Ok(()));
    let (cancel_cx, csid) = (cx.clone(), sid.clone());
    let _ = cx.spawn(async move {
        if cancel.await.is_ok() {
            cancel_cx.send_notification(CancelNotification::new(csid))?;
        }
        Ok(())
    });
    let resp = cx
        .send_request(PromptRequest::new(
            sid.clone(),
            vec![ContentBlock::Text(TextContent::new(prompt))],
        ))
        .block_task()
        .await;
    drop_route(&routes);
    let _ = done.send(
        resp.map(|r| debug_name(&r.stop_reason))
            .map_err(|e| e.to_string()),
    );
}

/// A turn that has started: everything it streams, its approval questions, a cancel switch,
/// and its stop reason when it ends.
pub struct Turn {
    pub events: mpsc::UnboundedReceiver<AcpEvent>,
    pub permissions: mpsc::UnboundedReceiver<PermissionAsk>,
    pub cancel: oneshot::Sender<()>,
    pub done: oneshot::Receiver<Result<String, String>>,
    /// Connection start (if any) plus session load, for latency reporting.
    pub setup_ms: u128,
}

/// Start one prompt turn on an existing session. Fails, without sending the prompt, when the
/// connection or the session load does not work, so the caller can fall back to `chat -q`.
pub async fn start_turn(session_id: &str, cwd: &Path, prompt: &str) -> Result<Turn> {
    let started = Instant::now();
    let (etx, erx) = mpsc::unbounded_channel();
    let (ptx, prx) = mpsc::unbounded_channel();
    let (ctx, crx) = oneshot::channel();
    let (rtx, rrx) = oneshot::channel();
    let (dtx, drx) = oneshot::channel();
    sender()?
        .send(TurnRequest {
            session_id: session_id.to_string(),
            cwd: cwd.to_path_buf(),
            prompt: prompt.to_string(),
            events: etx,
            permissions: ptx,
            cancel: crx,
            ready: rtx,
            done: dtx,
        })
        .map_err(|_| anyhow!("the ACP connection closed"))?;
    match tokio::time::timeout(Duration::from_secs(120), rrx).await {
        Ok(Ok(Ok(()))) => Ok(Turn {
            events: erx,
            permissions: prx,
            cancel: ctx,
            done: drx,
            setup_ms: started.elapsed().as_millis(),
        }),
        Ok(Ok(Err(e))) => bail!("{e}"),
        Ok(Err(_)) => {
            reset();
            bail!("hermes acp exited before the turn started")
        }
        Err(_) => {
            reset();
            bail!("hermes acp did not answer within 120 s")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_short_id_from_the_banner() {
        assert_eq!(
            banner_session("│  Session: 611fceac                   agent-rules-rollout, +5 more │")
                .as_deref(),
            Some("611fceac")
        );
        assert_eq!(
            banner_session("Session: local-abc123def").as_deref(),
            Some("local-abc123def")
        );
        assert_eq!(banner_session("no session here"), None);
        assert_eq!(banner_session("Session: ab"), None);
    }

    #[test]
    fn the_lookup_finds_only_the_session_studio_just_made() {
        let row = |id: &str, source: &str, at: f64, cwd: Option<&str>| SessionRow {
            id: id.into(),
            source: source.into(),
            started_at: at,
            cwd: cwd.map(String::from),
        };
        let dir = Path::new("C:\\Users\\me\\repo");
        let rows = vec![
            // Same 8-char prefix, but an older session from another client.
            row(
                "local-611fceac0000",
                "cli",
                1000.0,
                Some("C:/Users/me/repo"),
            ),
            // Near collision: same prefix, right source, another folder.
            row("local-611fceacffff", "gui", 1001.0, Some("D:/other")),
            // Same prefix, right folder, but started long before the spawn.
            row("local-611fceac1111", "gui", 500.0, Some("C:/Users/me/repo")),
            // The one: right source, after the spawn, folder not recorded yet.
            row("local-611fceac6e81", "gui", 1000.5, None),
        ];
        assert_eq!(
            match_session("611fceac", 1000.0, dir, &rows).as_deref(),
            Some("local-611fceac6e81")
        );
        // With the folder recorded, case and slashes do not matter.
        let rows2 = vec![row(
            "local-611fceac6e81",
            "gui",
            1000.5,
            Some("c:/users/me/repo/"),
        )];
        assert!(match_session("611fceac", 1000.0, dir, &rows2).is_some());
        assert!(match_session("611fceac", 1000.0, dir, &rows[..3]).is_none());
        assert!(match_session("611fceab", 1000.0, dir, &rows).is_none());
    }

    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(like_escape("ab_c%d\\e"), "ab\\_c\\%d\\\\e");
        assert_eq!(like_escape("611fceac"), "611fceac");
    }

    #[test]
    fn creation_carries_the_policy_resume_cannot() {
        let a = create_args(
            Path::new("C:/repo"),
            Some("claude-haiku-5-5[1m]"),
            Some("claude-subscription-directsdk-experimental"),
            &["github".into(), " ".into(), "obsidian".into()],
        )
        .join(" ");
        assert!(
            a.starts_with("chat --source gui --checkpoints --in C:/repo"),
            "{a}"
        );
        assert!(a.contains(
            "-m claude-haiku-5-5[1m] --provider claude-subscription-directsdk-experimental"
        ));
        assert!(a.ends_with("-s github,obsidian"));
        assert!(!a.contains("-q") && !a.contains("--resume"));
        let b = create_args(Path::new("C:/r"), None, Some("x"), &[]).join(" ");
        assert!(!b.contains("--provider") && !b.contains("-s "));
    }
}
