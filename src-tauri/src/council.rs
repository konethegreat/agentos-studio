//! Council for decisions: two or three agents on different providers answer the same design
//! question, then one synthesizer writes the decision.
//!
//! A mission whose kind is `decision` does not go to a single worker. `missions::start` hands it
//! here instead: each member gives its position in one headless turn (the same runner as
//! `ask_agent`), every position is logged on the mission, and the synthesizer (Hermes when
//! installed) turns them into an ADR. The ADR is committed in the mission's worktree under
//! `docs/adr/`, pushed and opened as a pull request, so it goes through the same review and merge
//! gate as code, and a decision note with each agent's position is written to the vault.
//!
//! Hermes `kanban swarm` was checked for this: its workers are Hermes profiles, so it cannot seat
//! Claude Code or Antigravity next to Hermes. Different providers are the point of a council.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde::Serialize;

use crate::missions::{self, Mission};
use crate::store;
use crate::vault::Vault;

/// Task id a council mission runs under, instead of a bus or kanban task.
pub const PREFIX: &str = "council:";
const POSITION_TIMEOUT: Duration = Duration::from_secs(420);
const SYNTH_TIMEOUT: Duration = Duration::from_secs(420);
/// A council that has not finished in this long is treated as lost (the app closed mid-run).
pub const STALE_SECS: i64 = 45 * 60;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Position {
    pub agent: String,
    pub text: String,
    pub duration_ms: i64,
}

pub fn is_council(task_id: Option<&str>) -> bool {
    task_id.is_some_and(|t| t.starts_with(PREFIX))
}

/// The provider family an agent's answer comes from: a council wants different ones. Pure.
pub fn family(agent: &str, hermes_provider: &str) -> String {
    match agent {
        "claude-code" => "anthropic".into(),
        "antigravity" => "google".into(),
        _ => {
            let p = hermes_provider.trim().to_lowercase();
            if p.starts_with("claude") || p.contains("anthropic") {
                "anthropic".into()
            } else if p.contains("antigravity") || p.contains("gemini") || p.contains("google") {
                "google".into()
            } else if p.is_empty() {
                "hermes".into()
            } else {
                p
            }
        }
    }
}

/// Up to three members on different provider families, and the synthesizer. Pure over
/// `available`. A council needs at least two members.
pub fn seat(
    hermes_provider: &str,
    available: impl Fn(&str) -> bool,
) -> Result<(Vec<String>, String)> {
    let mut members: Vec<String> = Vec::new();
    let mut families: Vec<String> = Vec::new();
    for a in ["claude-code", "antigravity", "hermes"] {
        if !available(a) {
            continue;
        }
        let f = family(a, hermes_provider);
        if families.contains(&f) {
            continue;
        }
        families.push(f);
        members.push(a.to_string());
    }
    if members.len() < 2 {
        bail!(
            "a council needs two agents on different providers; only {} available",
            if members.is_empty() {
                "none".to_string()
            } else {
                members.join(", ")
            }
        );
    }
    let synth = if available("hermes") {
        "hermes".to_string()
    } else {
        members[0].clone()
    };
    Ok((members, synth))
}

/// What each member answers. Pure.
pub fn position_prompt(m: &Mission, brief: &str) -> String {
    format!(
        "You are one member of a council of AI agents deciding a design question for the repository {repo}. \
Other agents on other providers answer the same question independently; a synthesizer will weigh every position.\n\n\
## Question\n{goal}\n\n## What a good decision must satisfy\n{acceptance}\n\n## Project brief\n{brief}\n\n\
Give your position in under 300 words: your recommendation, the two or three reasons that decide it, the main risk, \
and what you would refuse. Do not edit any file. Do not use em dashes or en dashes; use hyphens.",
        repo = m.repo,
        goal = m.goal.trim(),
        acceptance = m.acceptance.trim(),
        brief = brief.chars().take(2500).collect::<String>()
    )
}

/// What the synthesizer answers: the ADR body. Pure.
pub fn synth_prompt(m: &Mission, positions: &[Position]) -> String {
    let mut p = format!(
        "You are the synthesizer of a council of AI agents. Write the architecture decision record for this question, \
weighing every position below. Output only the Markdown of the ADR, starting with a level one heading `# <short title>`, \
then the sections `## Status` (Accepted), `## Context`, `## Decision`, `## Positions` (one subsection per agent, \
stating its recommendation faithfully, including where it disagreed), `## Consequences` and `## Rejected alternatives`. \
Do not use em dashes or en dashes; use hyphens.\n\n## Question\n{}\n\n## Acceptance\n{}\n",
        m.goal.trim(),
        m.acceptance.trim()
    );
    for pos in positions {
        p.push_str(&format!(
            "\n## Position of {}\n{}\n",
            pos.agent,
            pos.text.trim()
        ));
    }
    p
}

/// Replace the dashes the constitution bans, and strip a fence a model wrapped the ADR in. Pure.
pub fn clean_adr(text: &str) -> String {
    let t = text.trim();
    let t = t
        .strip_prefix("```markdown")
        .or_else(|| t.strip_prefix("```md"))
        .or_else(|| t.strip_prefix("```"))
        .map(|r| r.trim_end().trim_end_matches("```"))
        .unwrap_or(t);
    let start = t.find("# ").unwrap_or(0);
    t[start..]
        .replace(['\u{2014}', '\u{2013}'], "-")
        .trim()
        .to_string()
        + "\n"
}

/// The ADR title from its first heading. Pure.
pub fn adr_title(adr: &str) -> String {
    adr.lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Council decision".into())
}

/// The next ADR file name in `docs/adr/`: one above the highest `NNNN-` prefix. Pure over names.
pub fn next_adr_name(existing: &[String], title: &str) -> String {
    let n = existing
        .iter()
        .filter_map(|f| f.get(..4).and_then(|p| p.parse::<u32>().ok()))
        .max()
        .unwrap_or(0)
        + 1;
    format!(
        "{n:04}-{}.md",
        crate::vault::slug(title, 48)
            .to_lowercase()
            .replace('_', "-")
    )
}

/// Keep the Hermes session id with the logged text, so cleanup and the timeline can find it. Pure.
pub fn with_session(text: &str, sid: Option<&str>) -> String {
    match sid {
        Some(s) => format!("{text}\n\nHermes session: {s}"),
        None => text.to_string(),
    }
}

fn state_key(id: &str) -> String {
    format!("council.state.{id}")
}

/// `running:<epoch>`, `done` or `failed:<reason>`, set by the council thread.
pub fn state(id: &str) -> Option<String> {
    let c = store::open().ok()?;
    store::kv_get(&c, &state_key(id)).ok().flatten()
}

fn set_state(id: &str, s: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, &state_key(id), s);
    }
}

pub fn clear_state(id: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_delete(&c, &state_key(id));
    }
}

/// What the tick should do with a running council mission. Pure.
#[derive(Debug, PartialEq)]
pub enum Progress {
    Wait,
    Review,
    Fail(String),
}

pub fn progress(state: Option<&str>, now: i64) -> Progress {
    match state {
        Some("done") => Progress::Review,
        Some(s) if s.starts_with("failed:") => Progress::Fail(s["failed:".len()..].trim().into()),
        Some(s) if s.starts_with("running:") => {
            let started = s["running:".len()..].parse::<i64>().unwrap_or(now);
            if now - started > STALE_SECS {
                Progress::Fail(
                    "the council did not finish (the app may have closed while it ran)".into(),
                )
            } else {
                Progress::Wait
            }
        }
        _ => Progress::Fail("the council has no recorded state".into()),
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let exe = crate::procs::find("git").ok_or_else(|| anyhow::anyhow!("git is not installed"))?;
    let out = crate::procs::command(&exe)
        .args(args)
        .current_dir(dir)
        .output()?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Start the council for a queued decision mission: worktree now, the debate on a thread.
pub fn start(v: &Vault, m: &Mission) -> Result<Mission> {
    let dir = crate::review_agent::project_dir(&m.repo)
        .filter(|d| d.is_dir())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}: repository '{}' is not a known project with a folder",
                m.id,
                m.repo
            )
        })?;
    // Fail before the mission moves if no council can sit.
    seat(
        &crate::agents::hermes_provider(),
        crate::bus::headless_available,
    )?;
    crate::worktree::create(&dir, m)?;
    set_state(&m.id, &format!("running:{}", store::now()));
    let started = missions::dispatch(&m.id, &format!("{PREFIX}{}", m.id))?;
    if !cfg!(test) {
        let (v, id) = (v.clone(), m.id.clone());
        std::thread::spawn(move || {
            if let Err(e) = run(&v, &id) {
                set_state(&id, &format!("failed:{e:#}"));
            }
        });
    }
    Ok(started)
}

/// Run the council to its pull request. Blocking; minutes, not seconds.
pub fn run(v: &Vault, id: &str) -> Result<String> {
    let m = missions::get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    let wt = crate::worktree::recorded(id)
        .filter(|p| p.is_dir())
        .ok_or_else(|| anyhow::anyhow!("{id} has no worktree"))?;
    let (members, synth) = seat(
        &crate::agents::hermes_provider(),
        crate::bus::headless_available,
    )?;
    let brief = v.project_brief(&m.repo).unwrap_or_default();
    let prompt = position_prompt(&m, &brief);
    // Positions in parallel: each member answers alone.
    let handles: Vec<_> = members
        .iter()
        .map(|a| {
            let (a, p, dir) = (a.clone(), prompt.clone(), wt.clone());
            std::thread::spawn(move || {
                let t = Instant::now();
                let r = crate::ask::run_turn(&a, &p, &dir, POSITION_TIMEOUT);
                (a, r, t.elapsed().as_millis() as i64)
            })
        })
        .collect();
    let mut positions = Vec::new();
    for h in handles {
        let Ok((agent, r, ms)) = h.join() else {
            continue;
        };
        match r {
            Ok((text, sid, cost)) => {
                let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
                    mission_id: Some(id),
                    kind: "council",
                    from_agent: &agent,
                    to_agent: &synth,
                    summary: &format!("position: {}", text.chars().take(280).collect::<String>()),
                    detail: &with_session(&text, sid.as_deref()),
                    duration_ms: Some(ms),
                    cost_usd: Some(cost),
                });
                if cost > 0.0 {
                    if let Ok(Some(cur)) = missions::get(id) {
                        let _ = missions::set_spent(id, cur.spent_usd + cost);
                    }
                }
                positions.push(Position {
                    agent,
                    text,
                    duration_ms: ms,
                });
            }
            Err(e) => {
                let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
                    mission_id: Some(id),
                    kind: "council",
                    from_agent: &agent,
                    to_agent: &synth,
                    summary: &format!("no position: {e:#}"),
                    duration_ms: Some(ms),
                    ..Default::default()
                });
            }
        }
    }
    if positions.len() < 2 {
        bail!(
            "only {} of {} council members answered",
            positions.len(),
            members.len()
        );
    }
    let t = Instant::now();
    let (adr_raw, synth_sid, cost) =
        crate::ask::run_turn(&synth, &synth_prompt(&m, &positions), &wt, SYNTH_TIMEOUT)?;
    let adr = clean_adr(&adr_raw);
    let title = adr_title(&adr);
    let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
        mission_id: Some(id),
        kind: "council",
        from_agent: &synth,
        to_agent: "",
        summary: &format!("synthesis: {title}"),
        detail: &with_session(&adr, synth_sid.as_deref()),
        duration_ms: Some(t.elapsed().as_millis() as i64),
        cost_usd: Some(cost),
    });
    // The ADR goes through review like code: commit, push, pull request.
    let adr_dir = wt.join("docs").join("adr");
    std::fs::create_dir_all(&adr_dir)?;
    let existing: Vec<String> = std::fs::read_dir(&adr_dir)?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    let name = next_adr_name(&existing, &title);
    std::fs::write(adr_dir.join(&name), &adr)?;
    let rel = format!("docs/adr/{name}");
    git(&wt, &["add", &rel])?;
    let who: Vec<&str> = positions.iter().map(|p| p.agent.as_str()).collect();
    git(
        &wt,
        &[
            "commit",
            "-m",
            &format!("docs(adr): {title}"),
            "-m",
            &format!(
                "Council for mission {id}: {} gave positions, {synth} synthesized.",
                who.join(", ")
            ),
        ],
    )?;
    git(&wt, &["push", "-u", "origin", "HEAD"])?;
    let gh = crate::procs::find("gh").ok_or_else(|| anyhow::anyhow!("gh is not installed"))?;
    let body = format!(
        "Council decision for mission {id}.\n\nPositions from {}; synthesis by {synth}. Each position is in the ADR and on the mission's run timeline.\n\nFile: `{rel}`",
        who.join(", ")
    );
    let out = crate::procs::command(&gh)
        .args([
            "pr",
            "create",
            "--title",
            &format!("docs(adr): {title}"),
            "--body",
            &body,
        ])
        .current_dir(&wt)
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let url = missions::pull_url(&text).ok_or_else(|| {
        anyhow::anyhow!(
            "gh pr create gave no URL: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
    })?;
    missions::set_pr(id, &url)?;
    let mut note = format!(
        "Council decision for mission {id} in {}: {title}.\n\nPull request: {url}\n",
        m.repo
    );
    for p in &positions {
        note.push_str(&format!(
            "\n### Position of {}\n{}\n",
            p.agent,
            p.text.replace(['\u{2014}', '\u{2013}'], "-").trim()
        ));
    }
    let _ = v.write_typed_memory(
        &format!("Council {id} {title}"),
        &note,
        "architecture",
        &crate::vault::MemMeta {
            kind: Some(crate::vault::MemKind::Decision),
            agent: Some(synth.clone()),
            repo: Some(m.repo.clone()),
            ..Default::default()
        },
    );
    set_state(id, "done");
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_council_sits_different_providers_and_needs_two() {
        let all = |_: &str| true;
        // Hermes on the Claude subscription shares Claude Code's provider: it synthesizes instead.
        let (m, s) = seat("claude-subscription-directsdk-experimental", all).unwrap();
        assert_eq!(m, vec!["claude-code", "antigravity"]);
        assert_eq!(s, "hermes");
        let (m, _) = seat("openrouter", all).unwrap();
        assert_eq!(m, vec!["claude-code", "antigravity", "hermes"]);
        let (m, s) = seat("openrouter", |a| a != "antigravity").unwrap();
        assert_eq!(m, vec!["claude-code", "hermes"]);
        assert_eq!(s, "hermes");
        assert!(seat("claude-subscription", |a| a != "antigravity").is_err());
        assert!(seat("", |_| false).is_err());
    }

    #[test]
    fn the_adr_is_cleaned_numbered_and_titled() {
        let raw = "```markdown\n# Use SQLite for the queue \u{2014} not Redis\n\n## Status\nAccepted\n```";
        let adr = clean_adr(raw);
        assert!(adr.starts_with("# Use SQLite for the queue - not Redis"));
        assert!(!adr.contains('\u{2014}') && !adr.contains("```"));
        assert_eq!(adr_title(&adr), "Use SQLite for the queue - not Redis");
        let names = vec!["0001-v0.3-stack.md".to_string(), "README.md".to_string()];
        assert_eq!(next_adr_name(&names, "Use SQLite"), "0002-use-sqlite.md");
        assert_eq!(next_adr_name(&[], "X"), "0001-x.md");
    }

    #[test]
    fn the_tick_waits_reviews_or_fails_a_council() {
        assert_eq!(progress(Some("running:100"), 200), Progress::Wait);
        assert_eq!(progress(Some("done"), 0), Progress::Review);
        assert_eq!(
            progress(Some("failed:no answer"), 0),
            Progress::Fail("no answer".into())
        );
        assert!(matches!(
            progress(Some("running:0"), STALE_SECS + 1),
            Progress::Fail(_)
        ));
        assert!(matches!(progress(None, 0), Progress::Fail(_)));
    }

    #[test]
    fn prompts_carry_the_question_and_every_position() {
        let m = Mission {
            id: "M-009".into(),
            repo: "agentos-sandbox-1".into(),
            goal: "Which test runner?".into(),
            acceptance: "Works on Windows".into(),
            role: "planner".into(),
            agent: "hermes".into(),
            status: "running".into(),
            provider: String::new(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: None,
            reviewer: None,
            review_verdict: None,
            pr_url: None,
            review_note: None,
            created_at: 0,
            updated_at: 0,
            verify: None,
        };
        assert!(position_prompt(&m, "brief").contains("Which test runner?"));
        let s = synth_prompt(
            &m,
            &[
                Position {
                    agent: "claude-code".into(),
                    text: "node:test".into(),
                    duration_ms: 1,
                },
                Position {
                    agent: "antigravity".into(),
                    text: "vitest".into(),
                    duration_ms: 1,
                },
            ],
        );
        assert!(s.contains("## Position of claude-code\nnode:test"));
        assert!(s.contains("## Position of antigravity\nvitest"));
        assert!(s.contains("## Rejected alternatives"));
    }
}
