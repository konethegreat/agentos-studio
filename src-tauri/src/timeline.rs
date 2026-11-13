//! One timeline across agents: who did what on a mission, from creation to merge.
//!
//! Built read-only from every place the work left a trace:
//! - the mission row (created, pull request, review verdict, current status),
//! - the exchange log (`agent_log`: handoffs, asks and answers, council positions, assignments),
//! - Agent Bus tasks that name the mission (their activity logs and completion summaries),
//! - worker logs (tool calls and permission denials of Claude Code and Antigravity workers),
//! - Hermes `state.db` for Hermes sessions the mission used (tool calls with their times, cost),
//!   read only, never written,
//! - Hermes kanban runs for Hermes missions,
//! - the memory ledger for those sessions (what the work read from and wrote to memory).
//!
//! Steps are sorted by time; replay in the UI walks them in that order.

use std::collections::BTreeSet;

use anyhow::Result;
use serde::Serialize;

use crate::missions::{self, Mission};
use crate::vault::{State, Vault};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Step {
    pub at: i64,
    pub agent: String,
    /// created, status, bus, tool, denied, handoff, ask, answer, council, assign, defer, review,
    /// pr, kanban, memory, note.
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<i64>,
    /// Where the step was read from.
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Timeline {
    pub mission: Mission,
    pub steps: Vec<Step>,
    pub agents: Vec<String>,
    pub handoffs: usize,
    pub asks: usize,
    pub total_cost_usd: f64,
    pub duration_s: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub id: String,
    pub repo: String,
    pub goal: String,
    pub status: String,
    pub agents: Vec<String>,
    pub exchanges: usize,
    pub updated_at: i64,
}

fn step(at: i64, agent: &str, kind: &str, title: &str, detail: &str, source: &str) -> Step {
    Step {
        at,
        agent: agent.to_string(),
        kind: kind.to_string(),
        title: title.to_string(),
        detail: detail.to_string(),
        cost_usd: None,
        duration_ms: None,
        source: source.to_string(),
    }
}

/// RFC 3339 to epoch seconds.
pub fn epoch(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s.trim().trim_matches('\''))
        .ok()
        .map(|d| d.timestamp())
}

/// Steps from a bus task body: each `- <time> - <text>` activity line and the completion summary.
/// Pure.
pub fn bus_steps(task_id: &str, agent: &str, body: &str) -> Vec<Step> {
    let mut out = Vec::new();
    let mut in_log = false;
    let mut summary = String::new();
    let mut in_summary = false;
    let mut completed_at = None;
    for line in body.lines() {
        if line.starts_with("## ") {
            in_log = line.trim() == "## Activity log";
            in_summary = line.trim() == "## Completion summary";
            continue;
        }
        if in_log {
            if let Some(rest) = line.strip_prefix("- ") {
                if let Some((time, text)) = rest.split_once(" - ") {
                    if let Some(at) = epoch(time) {
                        let actor = text
                            .rsplit_once(" by ")
                            .map(|(_, who)| who.trim().to_string())
                            .filter(|w| !w.contains(' '))
                            .unwrap_or_else(|| agent.to_string());
                        if text.starts_with("completed by") {
                            completed_at = Some(at);
                        }
                        out.push(step(
                            at,
                            &actor,
                            "bus",
                            &format!("{task_id}: {text}"),
                            "",
                            "agent bus",
                        ));
                    }
                }
            }
        } else if in_summary && !line.trim_start().starts_with("> [!success]") {
            summary.push_str(line);
            summary.push('\n');
        }
    }
    if let (Some(at), false) = (completed_at, summary.trim().is_empty()) {
        out.push(step(
            at,
            agent,
            "bus",
            &format!("{task_id}: completion summary"),
            summary.trim(),
            "agent bus",
        ));
    }
    out
}

/// Tool calls, denials and the Hermes session id from a worker log. Pure.
pub fn worker_log_steps(
    task_id: &str,
    agent: &str,
    log: &str,
    at: i64,
) -> (Vec<Step>, Vec<String>) {
    let mut out = Vec::new();
    let mut sessions = Vec::new();
    for raw in log.lines() {
        if let Some(sid) = raw
            .strip_prefix("[stderr] ")
            .and_then(|l| l.trim().strip_prefix("session_id:"))
        {
            sessions.push(sid.trim().to_string());
            continue;
        }
        let Some(json) = raw.strip_prefix("[stdout] ") else {
            continue;
        };
        let lines = match agent {
            "claude-code" => crate::bus::render_claude_event(json),
            "antigravity" => crate::bus::render_agy_event(json),
            _ => continue,
        };
        for l in lines {
            if let Some(tool) = l.strip_prefix("> ") {
                if out.len() < 80 {
                    let name = tool.split_whitespace().next().unwrap_or("tool");
                    out.push(step(
                        at,
                        agent,
                        "tool",
                        &format!("{task_id}: {name}"),
                        tool,
                        "worker log",
                    ));
                }
            } else if l.starts_with("[result]") {
                let kind = if l.contains("denied") {
                    "denied"
                } else {
                    "tool"
                };
                out.push(step(
                    at,
                    agent,
                    kind,
                    &format!("{task_id}: {l}"),
                    "",
                    "worker log",
                ));
            }
        }
    }
    // Claude Code's final event lists refused calls by name.
    for raw in log.lines().rev() {
        let Some(json) = raw.strip_prefix("[stdout] ") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
            continue;
        };
        if v["type"] == "result" {
            if let Some(d) = v["permission_denials"].as_array().filter(|d| !d.is_empty()) {
                out.push(step(
                    at,
                    agent,
                    "denied",
                    &format!("{task_id}: {} tool calls denied", d.len()),
                    &serde_json::Value::Array(d.clone()).to_string(),
                    "worker log",
                ));
            }
            break;
        }
    }
    (out, sessions)
}

/// The start time a worker log header records.
fn log_start(log: &str) -> Option<i64> {
    log.lines().find_map(|l| {
        let rest = l.strip_prefix("===== ")?;
        let i = rest.find(" worker started ")?;
        let tail = &rest[i + " worker started ".len()..];
        epoch(tail.split_whitespace().next()?)
    })
}

/// Hermes session steps from `state.db`, read only: tool calls with their times, and the cost.
fn hermes_session_steps(sid: &str, agent: &str) -> Vec<Step> {
    let Ok(c) = crate::hermes_engine::open_ro() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Ok(mut st) = c.prepare(
        "SELECT timestamp, tool_name, substr(COALESCE(content, ''), 1, 300) FROM messages
         WHERE session_id = ?1 AND role = 'tool' AND tool_name IS NOT NULL
         ORDER BY timestamp LIMIT 120",
    ) {
        if let Ok(rows) = st.query_map([sid], |r| {
            Ok((
                r.get::<_, f64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        }) {
            for (t, name, content) in rows.flatten() {
                out.push(step(
                    t as i64,
                    agent,
                    "tool",
                    &name,
                    &content,
                    "hermes state.db",
                ));
            }
        }
    }
    if let Ok((started, cost)) = c.query_row(
        "SELECT started_at, COALESCE(actual_cost_usd, estimated_cost_usd) FROM sessions WHERE id = ?1",
        [sid],
        |r| Ok((r.get::<_, Option<f64>>(0)?, r.get::<_, Option<f64>>(1)?)),
    ) {
        let mut s = step(
            started.unwrap_or(0.0) as i64,
            agent,
            "note",
            &format!("Hermes session {sid}"),
            "",
            "hermes state.db",
        );
        s.cost_usd = Some(crate::usage::metered_cost_of(&c, &[sid.to_string()]));
        if cost.is_some() {
            out.push(s);
        }
    }
    for e in crate::memory_ledger::list(sid, 50).unwrap_or_default() {
        out.push(step(
            e.at,
            agent,
            "memory",
            &format!("memory {}: {}", e.op, e.target),
            &e.path,
            "memory ledger",
        ));
    }
    out
}

/// Every bus task that belongs to a mission: its current task and any task naming it.
fn mission_tasks(v: &Vault, m: &Mission) -> Vec<(String, String, String)> {
    let marker = format!("Mission: {}.", m.id);
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for s in State::ALL {
        let Ok(rd) = std::fs::read_dir(v.state_dir(s)) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(text) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let current = m.task_id.as_deref().is_some_and(|t| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{t}_"))
            });
            if !(current || text.contains(&marker)) {
                continue;
            }
            let doc = crate::vault::Doc::parse(&text);
            let id = doc.get("id").unwrap_or_default().to_string();
            if id.is_empty() || !seen.insert(id.clone()) {
                continue;
            }
            let agent = doc.get("assigned_to").unwrap_or_default().to_string();
            out.push((id, agent, text));
        }
    }
    out
}

/// The full timeline of one mission.
pub fn mission(v: &Vault, id: &str) -> Result<Timeline> {
    let m = missions::get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    let mut steps = vec![step(
        m.created_at,
        "owner",
        "created",
        &format!("{} created for {} ({})", m.id, m.agent, m.role),
        &format!("{}\n\nAcceptance: {}", m.goal, m.acceptance),
        "missions",
    )];
    for e in crate::agent_log::for_mission(id).unwrap_or_default() {
        let title = match e.kind.as_str() {
            "handoff" => format!("handoff {} to {}", e.from_agent, e.to_agent),
            "ask" => format!("{} asked {}", e.from_agent, e.to_agent),
            "answer" => format!("{} answered {}", e.from_agent, e.to_agent),
            "council" => format!(
                "council: {}",
                e.summary.chars().take(80).collect::<String>()
            ),
            "assign" => format!("assigned to {}", e.to_agent),
            "defer" => "deferred until a quota window resets".to_string(),
            _ => e.summary.chars().take(80).collect(),
        };
        steps.push(Step {
            at: e.at,
            agent: e.from_agent.clone(),
            kind: e.kind.clone(),
            title,
            detail: if e.kind == "handoff" {
                e.summary.clone()
            } else {
                e.detail.clone()
            },
            cost_usd: e.cost_usd,
            duration_ms: e.duration_ms,
            source: "exchange log".into(),
        });
    }
    let mut sessions: Vec<(String, String)> = crate::agent_log::for_mission(id)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            let (_, sid) = e.detail.rsplit_once("Hermes session: ")?;
            Some((sid.trim().to_string(), e.from_agent.clone()))
        })
        .collect();
    for (task, agent, body) in mission_tasks(v, &m) {
        steps.extend(bus_steps(&task, &agent, &body));
        let log_path = v.worker_logs().join(format!("{task}.log"));
        if let Ok(log) = std::fs::read_to_string(&log_path) {
            let at = log_start(&log).unwrap_or(m.created_at);
            let (s, sids) = worker_log_steps(&task, &agent, &log, at);
            steps.extend(s);
            sessions.extend(sids.into_iter().map(|sid| (sid, agent.clone())));
        }
    }
    if let Some(task) = m
        .task_id
        .as_deref()
        .filter(|t| crate::kanban::is_kanban(Some(t)))
    {
        if let Ok(view) = crate::kanban::show(task) {
            for r in &view.runs {
                steps.push(step(
                    view.last_activity.unwrap_or(m.updated_at),
                    r.profile.as_deref().unwrap_or("hermes"),
                    "kanban",
                    &format!(
                        "kanban run {}{}",
                        r.status.as_deref().unwrap_or("?"),
                        r.outcome
                            .as_deref()
                            .map(|o| format!(" ({o})"))
                            .unwrap_or_default()
                    ),
                    r.summary.as_deref().or(r.error.as_deref()).unwrap_or(""),
                    "hermes kanban",
                ));
            }
            for sid in crate::kanban::session_ids(&view) {
                sessions.push((sid, "hermes".into()));
            }
        }
    }
    let mut seen = BTreeSet::new();
    for (sid, agent) in sessions {
        if seen.insert(sid.clone()) {
            steps.extend(hermes_session_steps(&sid, &agent));
        }
    }
    if let Some(pr) = &m.pr_url {
        steps.push(step(
            m.updated_at,
            &m.agent,
            "pr",
            &format!("pull request {pr}"),
            pr,
            "missions",
        ));
    }
    if let (Some(rv), Some(verdict)) = (&m.reviewer, &m.review_verdict) {
        steps.push(step(
            m.updated_at,
            rv,
            "review",
            &format!("{rv} {verdict}d the work"),
            m.review_note.as_deref().unwrap_or(""),
            "missions",
        ));
    }
    steps.push(step(
        m.updated_at,
        &m.agent,
        "status",
        &format!("now {}", m.status),
        "",
        "missions",
    ));
    steps.sort_by_key(|s| s.at);
    let agents: BTreeSet<String> = steps
        .iter()
        .filter(|s| crate::vault::AGENTS.contains(&s.agent.as_str()) && s.agent != "human")
        .map(|s| s.agent.clone())
        .collect();
    let total_cost_usd = steps
        .iter()
        .filter_map(|s| s.cost_usd)
        .sum::<f64>()
        .max(m.spent_usd);
    Ok(Timeline {
        handoffs: steps.iter().filter(|s| s.kind == "handoff").count(),
        asks: steps.iter().filter(|s| s.kind == "ask").count(),
        duration_s: steps.last().map(|s| s.at).unwrap_or(m.updated_at) - m.created_at,
        agents: agents.into_iter().collect(),
        total_cost_usd,
        steps,
        mission: m,
    })
}

/// Missions for the timeline picker, with how many agents and exchanges each involved.
pub fn summaries(limit: usize) -> Result<Vec<Summary>> {
    let all = missions::list(limit)?;
    Ok(all
        .into_iter()
        .map(|m| {
            let ev = crate::agent_log::for_mission(&m.id).unwrap_or_default();
            let mut agents: BTreeSet<String> = ev
                .iter()
                .flat_map(|e| [e.from_agent.clone(), e.to_agent.clone()])
                .filter(|a| !a.is_empty())
                .collect();
            agents.insert(m.agent.clone());
            if let Some(r) = &m.reviewer {
                agents.insert(r.clone());
            }
            Summary {
                id: m.id,
                repo: m.repo,
                goal: m.goal,
                status: m.status,
                agents: agents.into_iter().collect(),
                exchanges: ev.len(),
                updated_at: m.updated_at,
            }
        })
        .collect())
}

fn iso(at: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(at, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_else(|| at.to_string())
}

/// Render one mission's prompt, acceptance criteria, cost, tool calls and results as Markdown.
pub fn format_markdown(tl: &Timeline) -> String {
    let m = &tl.mission;
    let pr = m.pr_url.as_deref().unwrap_or("");
    let mut out = format!(
        "---
title: Run {id}
mission: {id}
repo: {repo}
role: {role}
agent: {agent}
model: {model}
status: {status}
cost_usd: {cost:.4}
duration_s: {dur}
pr_url: {pr}
---

# {id}: {goal}

## Prompt & Acceptance

**Goal:** {goal}

**Acceptance criteria:**
{acc}

## Summary

- Agents: {agents}
- Handoffs: {handoffs}, Asks: {asks}
- Duration: {dur}s
- Metered cost: ${cost:.2}
",
        id = m.id,
        repo = m.repo,
        role = m.role,
        agent = m.agent,
        model = if m.model.is_empty() {
            "inherit"
        } else {
            &m.model
        },
        status = m.status,
        cost = tl.total_cost_usd,
        dur = tl.duration_s.max(0),
        goal = crate::constitution_gate::redact(&m.goal),
        acc = crate::constitution_gate::redact(&m.acceptance),
        agents = if tl.agents.is_empty() {
            m.agent.clone()
        } else {
            tl.agents.join(", ")
        },
        handoffs = tl.handoffs,
        asks = tl.asks,
    );
    if let (Some(rv), Some(verdict)) = (&m.reviewer, &m.review_verdict) {
        out.push_str(&format!(
            "- Review: {verdict} by {rv}
"
        ));
    }
    out.push_str(
        "
## Timeline

",
    );
    for s in &tl.steps {
        let mut meta = vec![s.agent.clone(), s.kind.clone()];
        if let Some(ms) = s.duration_ms {
            meta.push(format!("{:.1}s", ms as f64 / 1000.0));
        }
        if let Some(c) = s.cost_usd.filter(|c| *c > 0.0) {
            meta.push(format!("${c:.3}"));
        }
        meta.push(s.source.clone());
        out.push_str(&format!(
            "- **{}** [{}]: {}
",
            iso(s.at),
            meta.join(", "),
            crate::constitution_gate::redact(&s.title)
        ));
        if !s.detail.trim().is_empty() {
            let safe = crate::constitution_gate::redact(s.detail.trim());
            out.push_str(
                "
```text
",
            );
            out.push_str(&safe);
            out.push_str(
                "
```

",
            );
        }
    }
    out
}

/// Export one mission's prompt, tool calls and results to `04_Agent_Engine/runs/<id>.md` in the vault.
pub fn export_markdown(v: &Vault, id: &str) -> Result<String> {
    let tl = mission(v, id)?;
    let md = format_markdown(&tl);
    let rel = format!("04_Agent_Engine/runs/{id}.md");
    let dest = v
        .root
        .join("04_Agent_Engine")
        .join("runs")
        .join(format!("{id}.md"));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dest, md)?;
    Ok(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "# TASK-012: x\n\n## Instructions\nMission: M-004.\n\n## Activity log\n\
- 2026-11-08T10:00:00+02:00 - created by agentos-studio\n\
- 2026-11-08T10:00:05+02:00 - claimed by claude-code\n\
- 2026-11-08T10:01:00+02:00 - claude-code worker started from AgentOS Studio (pid 4)\n\
- 2026-11-08T10:09:00+02:00 - completed by claude-code\n\n\
## Completion summary\n> [!success] Completed by **claude-code** at 2026-11-08T10:09:00+02:00\n\nOpened https://github.com/o/r/pull/5\n";

    #[test]
    fn format_markdown_includes_prompt_steps_and_redacts() {
        let m = Mission {
            id: "M-099".into(),
            repo: "r".into(),
            goal: "Export run sk-ant-api03-secretvalue1234567890".into(),
            acceptance: "Tests pass".into(),
            role: "implementer".into(),
            agent: "hermes".into(),
            status: "done".into(),
            ..Default::default()
        };
        let tl = Timeline {
            mission: m,
            steps: vec![step(
                1_800_000_000,
                "hermes",
                "tool",
                "Bash",
                "npm test",
                "state.db",
            )],
            agents: vec!["hermes".into()],
            handoffs: 0,
            asks: 0,
            total_cost_usd: 0.12,
            duration_s: 42,
        };
        let md = format_markdown(&tl);
        assert!(md.contains("## Prompt & Acceptance"));
        assert!(md.contains("Tests pass"));
        assert!(md.contains("Bash"));
        assert!(md.contains("npm test"));
        assert!(!md.contains("secretvalue1234567890"));
    }

    #[test]
    fn bus_activity_becomes_timed_steps_with_their_actor() {
        let s = bus_steps("TASK-012", "claude-code", BODY);
        assert_eq!(s.len(), 5, "{s:#?}");
        assert_eq!(s[0].agent, "agentos-studio");
        assert_eq!(s[1].agent, "claude-code");
        assert_eq!(s[2].agent, "claude-code", "a sentence is not an actor");
        assert_eq!(s[0].at, epoch("2026-11-08T10:00:00+02:00").unwrap());
        assert!(s[4].detail.contains("pull/5"));
        assert!(s[1].at > s[0].at);
    }

    #[test]
    fn worker_logs_give_tool_calls_denials_and_hermes_sessions() {
        let log = [
            "===== claude-code worker started 2026-11-08T10:01:00+02:00 (pid 4, cwd C:/r) =====",
            r#"[stdout] {"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"npm test"}}]}}"#,
            r#"[stdout] {"type":"result","subtype":"success","result":"done","num_turns":3,"total_cost_usd":0.0,"permission_denials":[{"tool_name":"Bash"}]}"#,
            "[stderr] session_id: local-abc123",
        ]
        .join("\n");
        assert_eq!(log_start(&log), epoch("2026-11-08T10:01:00+02:00"));
        let (s, sids) = worker_log_steps("TASK-012", "claude-code", &log, 7);
        assert!(
            s.iter()
                .any(|x| x.kind == "tool" && x.title.ends_with("Bash")),
            "{s:#?}"
        );
        assert!(s.iter().any(|x| x.kind == "denied"), "{s:#?}");
        assert_eq!(sids, vec!["local-abc123"]);
        assert!(s.iter().all(|x| x.at == 7));
    }
}
