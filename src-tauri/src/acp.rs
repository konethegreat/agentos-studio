//! Agent Client Protocol client for Hermes (`hermes acp`).
//!
//! Hermes owns execution: `hermes acp` is a viewer on the Hermes gateway, which runs the agent,
//! stores the session and decides what needs approval. Studio is the editor side of the protocol:
//! it opens or loads a session in a folder, streams one prompt turn (text, reasoning, tool calls,
//! plans), answers `session/request_permission` through a callback that the UI turns into the
//! approval modal, and cancels a turn on request.
//!
//! The spike (this file plus `tests/live_acp_spike.rs`) measures this path against the
//! `hermes chat -q` transport in `chat_stream.rs`; nothing here replaces it yet.

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    CancelNotification, ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest,
    PermissionOptionKind, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigOptionValue,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, TextContent,
};
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::{AcpAgent, Agent};
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// One thing a turn produced, in arrival order. Serialised for the UI as `{kind, ...}`.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcpEvent {
    Session {
        session_id: String,
    },
    Text {
        text: String,
    },
    Thought {
        text: String,
    },
    ToolCall {
        id: String,
        title: String,
        tool_kind: String,
        status: String,
    },
    ToolUpdate {
        id: String,
        status: String,
        output: String,
    },
    Plan {
        entries: Vec<String>,
    },
    Permission {
        request_id: String,
        title: String,
        options: Vec<PermissionChoice>,
    },
    Done {
        stop_reason: String,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct PermissionChoice {
    pub id: String,
    pub name: String,
    /// `allow_once`, `allow_always`, `reject_once` or `reject_always`.
    pub kind: String,
}

/// A permission question the UI must answer. Reply with the chosen option id, or `None` to
/// cancel (treated by Hermes as a denial).
pub struct PermissionAsk {
    pub request_id: String,
    pub title: String,
    pub options: Vec<PermissionChoice>,
    pub reply: oneshot::Sender<Option<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct TurnOptions {
    /// Folder the session works in. New sessions only; a loaded session keeps its own.
    pub cwd: PathBuf,
    /// Continue this Hermes session instead of creating one.
    pub session_id: Option<String>,
    pub prompt: String,
    /// `none`, `low`, `medium`, `high`, ... or `default`; sent as the `reasoning` config option.
    pub reasoning: Option<String>,
    /// Model id as Hermes lists it; sent as the `model` config option.
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TurnOutcome {
    pub session_id: String,
    pub stop_reason: String,
    pub text: String,
    /// Process start to the first streamed text or reasoning chunk.
    pub first_token_ms: Option<u128>,
    pub total_ms: u128,
    /// Milliseconds since process start at each protocol step, for latency work.
    pub phases: Vec<(String, u128)>,
}

fn hermes_command() -> Result<Vec<String>> {
    let exe = crate::procs::find("hermes").context("hermes is not installed")?;
    Ok(vec![exe.display().to_string(), "acp".into()])
}

fn text_of(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(t) => Some(t.text.clone()),
        _ => None,
    }
}

fn kind_name(k: &PermissionOptionKind) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{k:?}").to_lowercase())
}

fn debug_name<T: Serialize + std::fmt::Debug>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{v:?}"))
}

/// Map one session update to the events the UI renders. Pure, so it is unit tested.
pub fn events_for(update: &SessionUpdate) -> Vec<AcpEvent> {
    match update {
        SessionUpdate::AgentMessageChunk(c) => text_of(&c.content)
            .map(|text| vec![AcpEvent::Text { text }])
            .unwrap_or_default(),
        SessionUpdate::AgentThoughtChunk(c) => text_of(&c.content)
            .map(|text| vec![AcpEvent::Thought { text }])
            .unwrap_or_default(),
        SessionUpdate::ToolCall(t) => vec![AcpEvent::ToolCall {
            id: t.tool_call_id.0.to_string(),
            title: t.title.clone(),
            tool_kind: debug_name(&t.kind),
            status: debug_name(&t.status),
        }],
        SessionUpdate::ToolCallUpdate(u) => vec![AcpEvent::ToolUpdate {
            id: u.tool_call_id.0.to_string(),
            status: u.fields.status.as_ref().map(debug_name).unwrap_or_default(),
            output: u
                .fields
                .content
                .as_ref()
                .map(|cs| {
                    cs.iter()
                        .filter_map(|c| match c {
                            agent_client_protocol::v1::schema::ToolCallContent::Content(x) => {
                                text_of(&x.content)
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
        }],
        SessionUpdate::Plan(p) => vec![AcpEvent::Plan {
            entries: p.entries.iter().map(|e| e.content.clone()).collect(),
        }],
        _ => Vec::new(),
    }
}

/// Run one prompt turn over ACP. `events` receives everything streamed, `permissions` receives
/// each approval question, and sending on `cancel` asks Hermes to stop the turn (the prompt then
/// ends with stop reason `cancelled`).
pub async fn run_turn(
    opts: TurnOptions,
    events: mpsc::UnboundedSender<AcpEvent>,
    permissions: mpsc::UnboundedSender<PermissionAsk>,
    cancel: oneshot::Receiver<()>,
) -> Result<TurnOutcome> {
    let started = Instant::now();
    let agent = AcpAgent::from_args(hermes_command()?).map_err(|e| anyhow!("{e}"))?;
    let first = Arc::new(Mutex::new(None::<u128>));
    let text = Arc::new(Mutex::new(String::new()));
    let (ev_n, first_n, text_n) = (events.clone(), first.clone(), text.clone());
    let ev_p = events.clone();
    let outcome = Arc::new(Mutex::new(TurnOutcome::default()));
    let out_c = outcome.clone();
    // Loading a session replays its transcript as message chunks; only chunks that arrive after
    // the prompt is sent belong to this turn.
    let live = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let live_n = live.clone();
    let mark = |o: &Arc<Mutex<TurnOutcome>>, name: &str| {
        o.lock()
            .unwrap()
            .phases
            .push((name.to_string(), started.elapsed().as_millis()));
    };

    agent_client_protocol::Client
        .v1()
        .name("agentos-studio")
        .on_receive_notification(
            async move |n: SessionNotification, _cx| {
                if !live_n.load(std::sync::atomic::Ordering::SeqCst) {
                    return Ok(());
                }
                if std::env::var_os("ACP_DEBUG").is_some() {
                    eprintln!("ACP update: {:?}", n.update);
                }
                for e in events_for(&n.update) {
                    if matches!(e, AcpEvent::Text { .. } | AcpEvent::Thought { .. }) {
                        let mut f = first_n.lock().unwrap();
                        if f.is_none() {
                            *f = Some(started.elapsed().as_millis());
                        }
                    }
                    if let AcpEvent::Text { text } = &e {
                        text_n.lock().unwrap().push_str(text);
                    }
                    let _ = ev_n.send(e);
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, cx: ConnectionTo<Agent>| {
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
                let _ = ev_p.send(AcpEvent::Permission {
                    request_id: request_id.clone(),
                    title: title.clone(),
                    options: options.clone(),
                });
                let (tx, rx) = oneshot::channel();
                let asked = permissions
                    .send(PermissionAsk {
                        request_id,
                        title,
                        options,
                        reply: tx,
                    })
                    .is_ok();
                // Wait for the user outside the dispatch loop, so streaming continues meanwhile.
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
        .connect_with(agent, async |cx: ConnectionTo<Agent>| {
            cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            mark(&out_c, "initialized");
            let session_id = match &opts.session_id {
                Some(id) => {
                    cx.send_request(LoadSessionRequest::new(id.clone(), opts.cwd.clone()))
                        .block_task()
                        .await?;
                    id.clone()
                }
                None => cx
                    .send_request(NewSessionRequest::new(opts.cwd.clone()))
                    .block_task()
                    .await?
                    .session_id
                    .0
                    .to_string(),
            };
            mark(&out_c, "session ready");
            let _ = events.send(AcpEvent::Session {
                session_id: session_id.clone(),
            });
            for (id, value) in [("model", &opts.model), ("reasoning", &opts.reasoning)] {
                if let Some(v) = value {
                    cx.send_request(SetSessionConfigOptionRequest::new(
                        session_id.clone(),
                        id,
                        SessionConfigOptionValue::from(v.as_str()),
                    ))
                    .block_task()
                    .await?;
                    mark(&out_c, &format!("{id} set"));
                }
            }
            let cancel_cx = cx.clone();
            let sid = session_id.clone();
            cx.spawn(async move {
                if cancel.await.is_ok() {
                    cancel_cx.send_notification(CancelNotification::new(sid))?;
                }
                Ok(())
            })?;
            live.store(true, std::sync::atomic::Ordering::SeqCst);
            mark(&out_c, "prompt sent");
            let resp = cx
                .send_request(PromptRequest::new(
                    session_id.clone(),
                    vec![ContentBlock::Text(TextContent::new(opts.prompt.clone()))],
                ))
                .block_task()
                .await?;
            let mut o = out_c.lock().unwrap();
            o.session_id = session_id;
            o.stop_reason = debug_name(&resp.stop_reason);
            Ok(())
        })
        .await
        .map_err(|e| anyhow!("ACP: {e}"))?;

    let mut o = outcome.lock().unwrap().clone();
    o.text = text.lock().unwrap().clone();
    o.first_token_ms = *first.lock().unwrap();
    o.total_ms = started.elapsed().as_millis();
    let _ = events.send(AcpEvent::Done {
        stop_reason: o.stop_reason.clone(),
    });
    Ok(o)
}

/// Bound for one live call in tests and the UI.
pub const TURN_TIMEOUT: Duration = Duration::from_secs(600);

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::v1::schema::ContentChunk;

    #[test]
    fn text_and_reasoning_chunks_become_separate_events() {
        let t = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new("hi"),
        )));
        let r = SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new("thinking"),
        )));
        assert_eq!(events_for(&t), vec![AcpEvent::Text { text: "hi".into() }]);
        assert_eq!(
            events_for(&r),
            vec![AcpEvent::Thought {
                text: "thinking".into()
            }]
        );
    }
}
