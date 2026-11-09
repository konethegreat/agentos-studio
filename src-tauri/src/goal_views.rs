//! Obsidian views of a goal: plain Markdown and a JSON Canvas that work without Studio.
//!
//! Next to the goal note (`01_Projects/Goals/G-001.md`) Studio keeps a folder of the same name:
//!
//! - one note per mission (`M-004.md`) linking back to the goal, to the missions it waits for, to
//!   its pull request and to the decisions taken on it;
//! - one note per decision (`E-1.md`, an escalation and how it was answered);
//! - `G-001.canvas`, the goal's mission graph in the open JSON Canvas format: the goal, its
//!   missions in dependency order and the decisions, joined by edges.
//!
//! Every file is regenerated from `studio.db` when the goal note is written, so the views follow
//! the goal without a sync step. Files Studio wrote carry `generated_by: agentos-studio`; one that
//! no longer belongs to the goal (an id handed out again after a cleanup) is removed, and nothing
//! else in the folder is touched.

use anyhow::Result;
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::escalation::Escalation;
use crate::goals::Goal;
use crate::missions::Mission;
use crate::vault::Vault;

pub const GENERATED: &str = "generated_by: agentos-studio";

/// The goal's folder, next to its note.
pub fn folder(goal_id: &str) -> String {
    format!("01_Projects/Goals/{goal_id}")
}

pub fn mission_rel(goal_id: &str, mission_id: &str) -> String {
    format!("{}/{mission_id}.md", folder(goal_id))
}

pub fn decision_rel(goal_id: &str, escalation_id: &str) -> String {
    format!("{}/{escalation_id}.md", folder(goal_id))
}

pub fn canvas_rel(goal_id: &str) -> String {
    format!("{}/{goal_id}.canvas", folder(goal_id))
}

/// A wikilink by full path, so two goals' `M-004` notes never resolve to each other.
fn link(rel: &str, label: &str) -> String {
    format!("[[{}|{label}]]", rel.trim_end_matches(".md"))
}

pub fn goal_link(g: &Goal) -> String {
    link(&crate::goals::note_rel(&g.id), &g.id)
}

pub fn mission_link(goal_id: &str, mission_id: &str) -> String {
    link(&mission_rel(goal_id, mission_id), mission_id)
}

pub fn decision_link(goal_id: &str, escalation_id: &str) -> String {
    link(&decision_rel(goal_id, escalation_id), escalation_id)
}

fn when(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_default()
}

/// One line for a decision: what was asked and how it ended. Pure.
pub fn decision_line(e: &Escalation) -> String {
    match (e.status.as_str(), e.answer.as_deref()) {
        ("open", _) => format!("open, answers: {}", e.options.join(", ")),
        ("expired", a) => format!(
            "{} (no answer by the deadline)",
            a.unwrap_or(&e.safe_answer)
        ),
        (_, Some(a)) => format!(
            "{a} (answered from {})",
            e.answered_by.as_deref().unwrap_or("studio")
        ),
        (s, None) => s.to_string(),
    }
}

/// The extra sections of the goal note: decisions, pull requests and the mission graph. Pure.
pub fn goal_sections(g: &Goal, ms: &[Mission], escs: &[Escalation]) -> String {
    let mut s = String::new();
    s.push_str("\n## Decisions\n\n");
    if escs.is_empty() {
        s.push_str("None yet.\n");
    }
    for e in escs {
        s.push_str(&format!(
            "- {}: {} {}\n",
            decision_link(&g.id, &e.id),
            e.question,
            decision_line(e)
        ));
    }
    let prs: Vec<&Mission> = ms.iter().filter(|m| m.pr_url.is_some()).collect();
    if !prs.is_empty() {
        s.push_str("\n## Pull requests\n\n");
        for m in prs {
            s.push_str(&format!(
                "- {}: {}\n",
                mission_link(&g.id, &m.id),
                m.pr_url.as_deref().unwrap_or("")
            ));
        }
    }
    s.push_str(&format!(
        "\n## Mission graph\n\n{}\n",
        link(&canvas_rel(&g.id), "Open the mission graph")
    ));
    s
}

/// A mission's note. `deps` are the missions it waits for. Pure.
pub fn render_mission(g: &Goal, m: &Mission, deps: &[String], escs: &[Escalation]) -> String {
    let mut s = String::from("---\n");
    s.push_str("type: mission\n");
    s.push_str(&format!("id: {}\n", m.id));
    s.push_str(&format!("goal: \"{}\"\n", goal_link(g)));
    s.push_str(&format!("status: {}\n", m.status));
    s.push_str(&format!("repo: {}\n", m.repo));
    s.push_str(&format!("role: {}\n", m.role));
    s.push_str(&format!("agent: {}\n", m.agent));
    if let Some(r) = &m.reviewer {
        s.push_str(&format!("reviewer: {r}\n"));
    }
    if let Some(u) = &m.pr_url {
        s.push_str(&format!("pr: {u}\n"));
    }
    s.push_str(&format!("spent_usd: {:.2}\n", m.spent_usd));
    s.push_str(GENERATED);
    s.push_str("\n---\n\n");
    s.push_str(&format!("# {}: {}\n\n", m.id, m.goal));
    s.push_str(&format!(
        "Part of {}: {}. Status: **{}**, by {} in {}.\n",
        goal_link(g),
        g.outcome,
        m.status,
        m.agent,
        m.repo
    ));
    if !m.acceptance.trim().is_empty() {
        s.push_str(&format!("\n## Acceptance\n\n{}\n", m.acceptance.trim()));
    }
    if !deps.is_empty() {
        s.push_str("\n## Waits for\n\n");
        for d in deps {
            s.push_str(&format!("- {}\n", mission_link(&g.id, d)));
        }
    }
    if let Some(u) = &m.pr_url {
        s.push_str(&format!("\n## Pull request\n\n- [{u}]({u})\n"));
    }
    if let Some(v) = &m.review_verdict {
        s.push_str(&format!(
            "\n## Review\n\n{}{}\n",
            v,
            m.reviewer
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        ));
    }
    let mine: Vec<&Escalation> = escs
        .iter()
        .filter(|e| e.mission_id.as_deref() == Some(m.id.as_str()))
        .collect();
    if !mine.is_empty() {
        s.push_str("\n## Decisions\n\n");
        for e in mine {
            s.push_str(&format!(
                "- {}: {}\n",
                decision_link(&g.id, &e.id),
                decision_line(e)
            ));
        }
    }
    s
}

/// A decision's note. Pure.
pub fn render_decision(g: &Goal, e: &Escalation) -> String {
    let mut s = String::from("---\n");
    s.push_str("type: decision\n");
    s.push_str(&format!("id: {}\n", e.id));
    s.push_str(&format!("goal: \"{}\"\n", goal_link(g)));
    if let Some(m) = &e.mission_id {
        s.push_str(&format!("mission: \"{}\"\n", mission_link(&g.id, m)));
    }
    s.push_str(&format!("kind: {}\n", e.kind));
    s.push_str(&format!("status: {}\n", e.status));
    if let Some(a) = &e.answer {
        s.push_str(&format!("answer: {a}\n"));
    }
    if let Some(b) = &e.answered_by {
        s.push_str(&format!("answered_by: {b}\n"));
    }
    s.push_str(GENERATED);
    s.push_str("\n---\n\n");
    s.push_str(&format!("# {}: {}\n\n", e.id, e.question));
    let on = e
        .mission_id
        .as_deref()
        .map(|m| format!(" on {}", mission_link(&g.id, m)))
        .unwrap_or_default();
    s.push_str(&format!(
        "Asked for {}{on} at {}.\n\n",
        goal_link(g),
        when(e.created_at)
    ));
    s.push_str(&format!(
        "- Answers: {}\n- Safe answer: {}\n- Deadline: {}\n- Outcome: {}\n",
        e.options.join(", "),
        e.safe_answer,
        when(e.deadline_at),
        decision_line(e)
    ));
    if let Some(t) = e.answered_at {
        s.push_str(&format!("- Decided: {}\n", when(t)));
    }
    s
}

/// Each mission's column in the graph: 0 when it waits for nothing in this goal, otherwise one
/// more than the deepest mission it waits for. A cycle stops at the number of missions. Pure.
pub fn levels(ms: &[Mission], deps: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, usize> {
    let ids: Vec<&str> = ms.iter().map(|m| m.id.as_str()).collect();
    let mut lv: BTreeMap<String, usize> = ids.iter().map(|i| (i.to_string(), 0)).collect();
    for _ in 0..ids.len() {
        let mut changed = false;
        for id in &ids {
            let want = deps
                .get(*id)
                .into_iter()
                .flatten()
                .filter_map(|d| lv.get(d).map(|l| l + 1))
                .max()
                .unwrap_or(0)
                .min(ids.len());
            if lv.get(*id) != Some(&want) {
                lv.insert(id.to_string(), want);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    lv
}

fn color(status: &str) -> Option<&'static str> {
    // JSON Canvas preset colours: 1 red, 2 orange, 3 yellow, 4 green, 5 cyan, 6 purple.
    match status {
        "done" | "answered" => Some("4"),
        "failed" => Some("1"),
        "blocked" | "open" | "paused" => Some("2"),
        "running" | "review" | "verifying" => Some("3"),
        "expired" => Some("6"),
        _ => None,
    }
}

fn file_node(id: &str, file: &str, x: i64, y: i64, w: i64, h: i64, status: &str) -> Value {
    let mut n = json!({
        "id": id, "type": "file", "file": file, "x": x, "y": y, "width": w, "height": h
    });
    if let Some(c) = color(status) {
        n["color"] = json!(c);
    }
    n
}

fn edge(from: &str, to: &str, label: Option<&str>) -> Value {
    let mut e = json!({
        "id": format!("{from}->{to}"), "fromNode": from, "fromSide": "right",
        "toNode": to, "toSide": "left"
    });
    if let Some(l) = label {
        e["label"] = json!(l);
    }
    e
}

/// The goal's mission graph as JSON Canvas 1.0: the goal on the left, missions in dependency
/// columns, decisions in the last column. Deterministic, so an unchanged goal writes the same
/// bytes. Pure.
pub fn render_canvas(
    g: &Goal,
    ms: &[Mission],
    deps: &BTreeMap<String, Vec<String>>,
    escs: &[Escalation],
) -> String {
    const W: i64 = 400;
    const H: i64 = 240;
    const COL: i64 = 520;
    const ROW: i64 = 300;
    let mut nodes = vec![file_node(
        "goal",
        &crate::goals::note_rel(&g.id),
        0,
        0,
        W,
        H,
        &g.status,
    )];
    let mut edges = Vec::new();
    let lv = levels(ms, deps);
    let mut rows: BTreeMap<usize, i64> = BTreeMap::new();
    let ids: Vec<&str> = ms.iter().map(|m| m.id.as_str()).collect();
    for m in ms {
        let l = lv.get(&m.id).copied().unwrap_or(0);
        let row = rows.entry(l).or_insert(0);
        let node = format!("m-{}", m.id);
        nodes.push(file_node(
            &node,
            &mission_rel(&g.id, &m.id),
            COL * (l as i64 + 1),
            ROW * *row,
            W,
            H,
            &m.status,
        ));
        *row += 1;
        let inside: Vec<&String> = deps
            .get(&m.id)
            .into_iter()
            .flatten()
            .filter(|d| ids.contains(&d.as_str()))
            .collect();
        if inside.is_empty() {
            edges.push(edge("goal", &node, None));
        }
        for d in inside {
            edges.push(edge(&format!("m-{d}"), &node, Some("then")));
        }
    }
    let last = lv.values().copied().max().map(|l| l + 2).unwrap_or(1) as i64;
    for (i, e) in escs.iter().enumerate() {
        let node = format!("e-{}", e.id);
        nodes.push(file_node(
            &node,
            &decision_rel(&g.id, &e.id),
            COL * last,
            ROW * i as i64,
            W,
            H,
            &e.status,
        ));
        let from = e
            .mission_id
            .as_deref()
            .filter(|m| ids.contains(m))
            .map(|m| format!("m-{m}"))
            .unwrap_or_else(|| "goal".into());
        edges.push(edge(&from, &node, Some(&e.kind)));
    }
    serde_json::to_string_pretty(&json!({ "nodes": nodes, "edges": edges })).unwrap_or_default()
}

/// Write the goal's folder: mission notes, decision notes and the canvas, then remove generated
/// files that no longer belong to it. Returns the vault-relative paths written.
pub fn write(
    v: &Vault,
    g: &Goal,
    ms: &[Mission],
    deps: &BTreeMap<String, Vec<String>>,
    escs: &[Escalation],
) -> Result<Vec<String>> {
    let mut written = Vec::new();
    for m in ms {
        let rel = mission_rel(&g.id, &m.id);
        let d = deps.get(&m.id).cloned().unwrap_or_default();
        v.write_note(&rel, &render_mission(g, m, &d, escs))?;
        written.push(rel);
    }
    for e in escs {
        let rel = decision_rel(&g.id, &e.id);
        v.write_note(&rel, &render_decision(g, e))?;
        written.push(rel);
    }
    let canvas = canvas_rel(&g.id);
    v.write_note(&canvas, &render_canvas(g, ms, deps, escs))?;
    written.push(canvas);
    let dir = v.resolve_rel(&folder(&g.id))?;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for f in rd.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            let rel = format!("{}/{name}", folder(&g.id));
            if !name.ends_with(".md") || written.contains(&rel) {
                continue;
            }
            let ours = std::fs::read_to_string(f.path())
                .map(|t| t.contains(GENERATED))
                .unwrap_or(false);
            if ours {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
    Ok(written)
}

/// The goal's decisions, oldest first, and each mission's dependencies, read from `studio.db`.
pub fn load(goal_id: &str, ms: &[Mission]) -> (BTreeMap<String, Vec<String>>, Vec<Escalation>) {
    let deps = ms
        .iter()
        .map(|m| (m.id.clone(), crate::planner::depends_on(&m.id)))
        .collect();
    let mut escs: Vec<Escalation> = crate::escalation::list(500)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.goal_id.as_deref() == Some(goal_id))
        .collect();
    escs.sort_by_key(|e| (e.created_at, e.id.clone()));
    (deps, escs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goals::Check;

    fn goal() -> Goal {
        Goal {
            id: "G-001".into(),
            outcome: "Add a probe endpoint".into(),
            repos: vec!["demo".into()],
            checks: vec![Check {
                kind: "file".into(),
                target: "PROBE.md".into(),
                repo: None,
            }],
            budget_usd: 0.0,
            deadline: None,
            autonomy: "ask".into(),
            status: "running".into(),
            note: String::new(),
            report: None,
            created_at: 0,
            updated_at: 0,
        }
    }
    fn mission(id: &str, status: &str, pr: Option<&str>) -> Mission {
        Mission {
            id: id.into(),
            repo: "demo".into(),
            goal: format!("Step {id}"),
            acceptance: "it works".into(),
            role: "implementer".into(),
            agent: "hermes".into(),
            status: status.into(),
            provider: String::new(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: None,
            reviewer: Some("antigravity".into()),
            review_verdict: None,
            pr_url: pr.map(String::from),
            review_note: None,
            created_at: 0,
            updated_at: 0,
            verify: None,
        }
    }
    fn esc(
        id: &str,
        mission: &str,
        status: &str,
        answer: Option<&str>,
        by: Option<&str>,
    ) -> Escalation {
        Escalation {
            id: id.into(),
            goal_id: Some("G-001".into()),
            mission_id: Some(mission.into()),
            kind: "merge".into(),
            question: format!("Merge {mission}?"),
            options: vec!["merge".into(), "skip".into()],
            safe_answer: "skip".into(),
            status: status.into(),
            answer: answer.map(String::from),
            answered_by: by.map(String::from),
            created_at: 100,
            deadline_at: 3700,
            answered_at: answer.map(|_| 200),
        }
    }
    fn deps() -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([
            ("M-001".to_string(), vec![]),
            ("M-002".to_string(), vec!["M-001".to_string()]),
            (
                "M-003".to_string(),
                vec!["M-001".to_string(), "M-002".to_string()],
            ),
        ])
    }

    #[test]
    fn missions_link_back_to_the_goal_their_dependencies_pr_and_decisions() {
        let g = goal();
        let m = mission("M-002", "done", Some("https://github.com/x/demo/pull/7"));
        let e = esc("E-1", "M-002", "answered", Some("merge"), Some("phone"));
        let note = render_mission(&g, &m, &["M-001".to_string()], std::slice::from_ref(&e));
        assert!(note.contains("goal: \"[[01_Projects/Goals/G-001|G-001]]\""));
        assert!(note.contains("[[01_Projects/Goals/G-001/M-001|M-001]]"));
        assert!(note.contains("pull/7"));
        assert!(note.contains("[[01_Projects/Goals/G-001/E-1|E-1]]: merge (answered from phone)"));
        assert!(note.contains(GENERATED));
        let d = render_decision(&g, &e);
        assert!(d.contains("mission: \"[[01_Projects/Goals/G-001/M-002|M-002]]\""));
        assert!(d.contains("answered_by: phone"));
        let sections = goal_sections(&g, &[m], &[e]);
        assert!(sections.contains("## Pull requests"));
        assert!(
            sections.contains("[[01_Projects/Goals/G-001/G-001.canvas|Open the mission graph]]")
        );
        let expired = esc("E-2", "M-003", "expired", Some("skip"), Some("timeout"));
        assert_eq!(decision_line(&expired), "skip (no answer by the deadline)");
    }

    #[test]
    fn the_canvas_lays_missions_out_in_dependency_order() {
        let g = goal();
        let ms = vec![
            mission("M-001", "done", None),
            mission("M-002", "running", None),
            mission("M-003", "queued", None),
        ];
        let lv = levels(&ms, &deps());
        assert_eq!((lv["M-001"], lv["M-002"], lv["M-003"]), (0, 1, 2));
        let escs = vec![esc("E-1", "M-002", "open", None, None)];
        let c: Value = serde_json::from_str(&render_canvas(&g, &ms, &deps(), &escs)).unwrap();
        let nodes = c["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 5, "goal, three missions, one decision");
        let x = |id: &str| {
            nodes.iter().find(|n| n["id"] == id).unwrap()["x"]
                .as_i64()
                .unwrap()
        };
        assert!(
            x("goal") < x("m-M-001") && x("m-M-001") < x("m-M-002") && x("m-M-002") < x("m-M-003")
        );
        assert!(
            x("e-E-1") > x("m-M-003"),
            "decisions sit after the last column"
        );
        assert_eq!(nodes[1]["file"], "01_Projects/Goals/G-001/M-001.md");
        assert_eq!(nodes[1]["color"], "4");
        // JSON Canvas 1.0: every node has an id, a type and a box; file nodes name a file; every
        // edge joins two nodes that exist; ids are unique.
        let mut seen = std::collections::BTreeSet::new();
        for n in nodes {
            assert!(
                seen.insert(n["id"].as_str().unwrap().to_string()),
                "unique id"
            );
            assert_eq!(n["type"], "file");
            assert!(n["file"]
                .as_str()
                .unwrap()
                .starts_with("01_Projects/Goals/"));
            for k in ["x", "y", "width", "height"] {
                assert!(n[k].is_i64(), "{k} is an integer");
            }
        }
        let edges = c["edges"].as_array().unwrap();
        for e in edges {
            assert!(seen.contains(e["fromNode"].as_str().unwrap()));
            assert!(seen.contains(e["toNode"].as_str().unwrap()));
            assert!(
                seen.insert(e["id"].as_str().unwrap().to_string()),
                "unique edge id"
            );
        }
        let has = |a: &str, b: &str| edges.iter().any(|e| e["fromNode"] == a && e["toNode"] == b);
        assert!(has("goal", "m-M-001") && has("m-M-001", "m-M-002") && has("m-M-002", "m-M-003"));
        assert!(has("m-M-002", "e-E-1"));
        assert!(
            !has("goal", "m-M-002"),
            "a mission with a dependency hangs off it, not the goal"
        );
        assert_eq!(
            render_canvas(&g, &ms, &deps(), &escs),
            render_canvas(&g, &ms, &deps(), &escs),
            "deterministic"
        );
        // A cycle does not hang and stays bounded.
        let cyc = BTreeMap::from([
            ("M-001".to_string(), vec!["M-002".to_string()]),
            ("M-002".to_string(), vec!["M-001".to_string()]),
        ]);
        let lv = levels(&ms[..2], &cyc);
        assert!(lv.values().all(|l| *l <= 2));
    }

    #[test]
    fn writing_the_folder_removes_only_stale_generated_files() {
        let tmp = tempfile::tempdir().unwrap();
        let v = Vault::new(tmp.path());
        let g = goal();
        let ms = vec![
            mission("M-001", "done", None),
            mission("M-002", "queued", None),
        ];
        let escs = vec![esc(
            "E-1",
            "M-001",
            "answered",
            Some("merge"),
            Some("notion"),
        )];
        // Left over from an earlier goal with the same id, plus a note the owner wrote.
        v.write_note(
            &mission_rel("G-001", "M-009"),
            &format!("---\n{GENERATED}\n---\nold"),
        )
        .unwrap();
        v.write_note("01_Projects/Goals/G-001/my notes.md", "mine")
            .unwrap();
        let written = write(&v, &g, &ms, &deps(), &escs).unwrap();
        assert_eq!(written.len(), 4, "two missions, one decision, the canvas");
        assert!(v.read_note(&mission_rel("G-001", "M-002")).is_ok());
        assert!(v.read_note(&decision_rel("G-001", "E-1")).is_ok());
        assert!(v.read_note(&canvas_rel("G-001")).is_ok());
        assert!(
            v.read_note(&mission_rel("G-001", "M-009")).is_err(),
            "stale note removed"
        );
        assert_eq!(
            v.read_note("01_Projects/Goals/G-001/my notes.md").unwrap(),
            "mine"
        );
    }
}
