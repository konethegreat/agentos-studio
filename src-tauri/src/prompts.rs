//! The prompt workbench (docs/PROMPTS.md): saved prompts that know their project, written from
//! what Studio already knows about the work, checked before they leave, and handed to another
//! agent or app.
//!
//! A prompt is stored in `studio.db` (schema 12) with every version kept, and mirrored to the
//! vault at `05_Prompts/<project>/P-<id>.md` so agents find it through recall. The live project
//! facts sit between `<!-- agentos:context -->` markers; "Refresh context" rewrites only that
//! block, so a prompt saved last week goes out with today's commits, tasks and missions.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::procs;
use crate::vault::Vault;

pub const CTX_START: &str = "<!-- agentos:context -->";
pub const CTX_END: &str = "<!-- /agentos:context -->";
pub const KINDS: [&str; 5] = ["continue", "new_session", "review", "fix", "custom"];
/// The app or agent a prompt is written for, and roughly how many tokens it should stay under.
pub const TARGETS: [(&str, &str, i64); 7] = [
    ("claude-code", "Claude Code", 6000),
    ("antigravity", "Antigravity", 6000),
    ("qoder", "Qoder", 6000),
    ("codex", "Codex CLI", 6000),
    ("cursor", "Cursor", 4000),
    ("claude-desktop", "Claude Desktop", 8000),
    ("chat", "A chat model (no tools)", 3000),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Prompt {
    pub id: i64,
    pub title: String,
    /// Hermes project slug, or a folder path.
    pub project: String,
    pub kind: String,
    pub target: String,
    pub body: String,
    pub version: i64,
    pub uses: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub version: i64,
    pub body: String,
    pub note: String,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    pub id: i64,
    pub prompt_id: i64,
    pub version: i64,
    pub via: String,
    pub detail: String,
    pub at: i64,
}

/// What Studio knows about a project at this moment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectContext {
    pub name: String,
    pub path: Option<String>,
    pub branch: String,
    pub remote: String,
    pub stack: Vec<String>,
    pub commits: Vec<String>,
    pub dirty: usize,
    pub rules: String,
    pub phases: String,
    pub tasks: String,
    pub note: String,
    pub missions: Vec<String>,
    pub open_prs: Vec<String>,
    pub checks: Vec<String>,
    /// Files an agent should read first, repo-relative, that exist.
    #[serde(default)]
    pub files: Vec<String>,
}

/// A Hermes session to carry forward into a new one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionBrief {
    pub id: String,
    pub title: String,
    pub last_request: String,
    pub last_reply: String,
    pub summary: String,
    pub written: Vec<String>,
    pub decisions: Vec<String>,
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub messages: i64,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Finding {
    /// error (fix before sending), warn, info
    pub level: String,
    pub text: String,
}

// ---------------------------------------------------------------------------------------------
// Store

fn row(r: &rusqlite::Row) -> rusqlite::Result<Prompt> {
    Ok(Prompt {
        id: r.get(0)?,
        title: r.get(1)?,
        project: r.get(2)?,
        kind: r.get(3)?,
        target: r.get(4)?,
        body: r.get(5)?,
        version: r.get(6)?,
        uses: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

const COLS: &str = "id, title, project, kind, target, body, version, uses, created_at, updated_at";

pub fn validate(title: &str, kind: &str, target: &str, body: &str) -> Result<()> {
    if title.trim().is_empty() {
        bail!("a prompt needs a title");
    }
    if body.trim().is_empty() {
        bail!("a prompt needs a body");
    }
    if !KINDS.contains(&kind) {
        bail!("unknown kind '{kind}'");
    }
    if !TARGETS.iter().any(|t| t.0 == target) {
        bail!("unknown target '{target}'");
    }
    Ok(())
}

/// Save a new prompt (id 0) or a new version of an existing one. A save with the same body keeps
/// the version number; a changed body adds a version, so nothing written is ever lost.
pub fn save_in(c: &Connection, p: &Prompt, note: &str) -> Result<Prompt> {
    validate(&p.title, &p.kind, &p.target, &p.body)?;
    let now = crate::store::now();
    if p.id == 0 {
        c.execute(
            "INSERT INTO prompts (title, project, kind, target, body, version, uses, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, 0, ?6, ?6)",
            params![p.title.trim(), p.project.trim(), p.kind, p.target, p.body, now],
        )?;
        let id = c.last_insert_rowid();
        c.execute(
            "INSERT INTO prompt_versions (prompt_id, version, body, note, at) VALUES (?1, 1, ?2, ?3, ?4)",
            params![id, p.body, note, now],
        )?;
        return get_in(c, id)?.ok_or_else(|| anyhow!("prompt {id} vanished"));
    }
    let old = get_in(c, p.id)?.ok_or_else(|| anyhow!("no prompt {}", p.id))?;
    let version = if old.body == p.body {
        old.version
    } else {
        old.version + 1
    };
    c.execute(
        "UPDATE prompts SET title = ?2, project = ?3, kind = ?4, target = ?5, body = ?6, version = ?7, updated_at = ?8 WHERE id = ?1",
        params![p.id, p.title.trim(), p.project.trim(), p.kind, p.target, p.body, version, now],
    )?;
    if version != old.version {
        c.execute(
            "INSERT INTO prompt_versions (prompt_id, version, body, note, at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![p.id, version, p.body, note, now],
        )?;
    }
    get_in(c, p.id)?.ok_or_else(|| anyhow!("prompt {} vanished", p.id))
}

pub fn get_in(c: &Connection, id: i64) -> Result<Option<Prompt>> {
    Ok(c.query_row(
        &format!("SELECT {COLS} FROM prompts WHERE id = ?1"),
        [id],
        row,
    )
    .optional()?)
}

/// Every prompt, newest first, optionally for one project and matching a search over title and body.
pub fn list_in(c: &Connection, project: Option<&str>, q: Option<&str>) -> Result<Vec<Prompt>> {
    let mut st = c.prepare(&format!(
        "SELECT {COLS} FROM prompts
         WHERE (?1 IS NULL OR project = ?1)
           AND (?2 IS NULL OR title LIKE '%' || ?2 || '%' OR body LIKE '%' || ?2 || '%')
         ORDER BY updated_at DESC, id DESC"
    ))?;
    let out = st
        .query_map(
            params![
                project.filter(|s| !s.is_empty()),
                q.filter(|s| !s.is_empty())
            ],
            row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

pub fn versions_in(c: &Connection, id: i64) -> Result<Vec<Version>> {
    let mut st = c.prepare(
        "SELECT version, body, note, at FROM prompt_versions WHERE prompt_id = ?1 ORDER BY version DESC",
    )?;
    let out = st
        .query_map([id], |r| {
            Ok(Version {
                version: r.get(0)?,
                body: r.get(1)?,
                note: r.get(2)?,
                at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

pub fn handoffs_in(c: &Connection, id: i64) -> Result<Vec<Handoff>> {
    let mut st = c.prepare(
        "SELECT id, prompt_id, version, via, detail, at FROM prompt_handoffs WHERE prompt_id = ?1 ORDER BY id DESC LIMIT 50",
    )?;
    let out = st
        .query_map([id], |r| {
            Ok(Handoff {
                id: r.get(0)?,
                prompt_id: r.get(1)?,
                version: r.get(2)?,
                via: r.get(3)?,
                detail: r.get(4)?,
                at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

/// Record that a prompt left Studio (copied, written into a repo, put on the bus).
pub fn record_use_in(c: &Connection, id: i64, via: &str, detail: &str) -> Result<()> {
    let p = get_in(c, id)?.ok_or_else(|| anyhow!("no prompt {id}"))?;
    c.execute(
        "INSERT INTO prompt_handoffs (prompt_id, version, via, detail, at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, p.version, via, detail, crate::store::now()],
    )?;
    c.execute("UPDATE prompts SET uses = uses + 1 WHERE id = ?1", [id])?;
    Ok(())
}

pub fn delete_in(c: &Connection, id: i64) -> Result<()> {
    c.execute("DELETE FROM prompt_handoffs WHERE prompt_id = ?1", [id])?;
    c.execute("DELETE FROM prompt_versions WHERE prompt_id = ?1", [id])?;
    c.execute("DELETE FROM prompts WHERE id = ?1", [id])?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Vault mirror

fn slug(s: &str) -> String {
    let s: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() {
        "general".into()
    } else {
        s.chars().take(60).collect()
    }
}

pub fn vault_rel(p: &Prompt) -> String {
    let project = Path::new(&p.project)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.project.clone());
    format!("05_Prompts/{}/P-{:03}.md", slug(&project), p.id)
}

/// The vault copy, so agents recall saved prompts like any other note. Pure.
pub fn vault_note(p: &Prompt) -> String {
    format!(
        "---\ngenerated_by: agentos-studio\ntype: prompt\nprompt_id: {}\ntitle: \"{}\"\nproject: \"{}\"\nkind: {}\ntarget: {}\nversion: {}\n---\n\n# {}\n\n{}\n",
        p.id,
        p.title.replace('"', "'"),
        p.project.replace('"', "'"),
        p.kind,
        p.target,
        p.version,
        p.title,
        p.body.trim_end()
    )
}

pub fn mirror(v: &Vault, p: &Prompt) -> Result<()> {
    v.write_note(&vault_rel(p), &vault_note(p))
}

pub fn unmirror(v: &Vault, p: &Prompt) {
    let path = v.root.join(vault_rel(p));
    let marked = std::fs::read_to_string(&path)
        .map(|s| s.contains("generated_by: agentos-studio"))
        .unwrap_or(false);
    if marked {
        let _ = std::fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------------------------
// Project context

/// The folder a project lives in: a Hermes project slug, or an existing absolute folder.
pub fn project_path(project: &str) -> Option<PathBuf> {
    let p = Path::new(project.trim());
    if p.is_absolute() && p.is_dir() {
        return Some(p.to_path_buf());
    }
    crate::review_agent::project_dir(project.trim())
}

fn head(text: &str, max_lines: usize, max_chars: usize) -> String {
    let s: String = text.lines().take(max_lines).collect::<Vec<_>>().join("\n");
    if s.chars().count() > max_chars {
        let cut: String = s.chars().take(max_chars).collect();
        format!("{cut}\n(truncated)")
    } else {
        s
    }
}

/// Files worth reading before any change, when the repository has them.
const READ_FIRST: [&str; 8] = [
    "AGENTS.md",
    "README.md",
    "CONTRIBUTING.md",
    "docs/agentos/PHASES.md",
    "docs/agentos/TASKS.md",
    "docs/ROADMAP.md",
    "docs/ARCHITECTURE.md",
    "PHASES.md",
];

/// The unfinished part of a TASKS.md: open checkboxes, and the headings above them. Pure.
pub fn open_tasks(md: &str, max: usize) -> String {
    let mut out = Vec::new();
    let mut heading = "";
    let mut printed = "";
    for l in md.lines() {
        let t = l.trim_start();
        if t.starts_with('#') {
            heading = l;
        } else if t.starts_with("- [ ]") {
            if heading != printed && !heading.is_empty() {
                out.push(heading.to_string());
                printed = heading;
            }
            out.push(l.to_string());
            if out.len() >= max {
                break;
            }
        }
    }
    out.join("\n")
}

/// Checks that tell an agent the work is right: the package.json scripts that exist, and the
/// usual commands of the other stacks. Pure.
pub fn checks_for(stack: &[String], has: impl Fn(&str) -> bool, package_json: &str) -> Vec<String> {
    let mut out = Vec::new();
    let s = |k: &str| stack.iter().any(|x| x.eq_ignore_ascii_case(k));
    if has("package.json") {
        let v: serde_json::Value = serde_json::from_str(package_json).unwrap_or_default();
        let scripts = v.get("scripts").and_then(|x| x.as_object());
        for name in ["build", "lint", "test"] {
            if scripts.is_some_and(|m| m.contains_key(name)) {
                out.push(if name == "test" {
                    "npm test".into()
                } else {
                    format!("npm run {name}")
                });
            }
        }
    }
    if has("Cargo.toml") || has("src-tauri/Cargo.toml") {
        let pre = if has("Cargo.toml") {
            ""
        } else {
            "cd src-tauri && "
        };
        out.push(format!("{pre}cargo fmt --check"));
        out.push(format!("{pre}cargo clippy --all-targets -- -D warnings"));
        out.push(format!("{pre}cargo test"));
    }
    if has("pyproject.toml") || s("python") {
        out.push("python -m pytest".into());
    }
    if has("go.mod") {
        out.push("go test ./...".into());
    }
    out
}

/// Read what Studio knows about a project now. Every source is optional; a missing one is left
/// empty rather than failing the whole gather.
pub fn gather(v: &Vault, project: &str) -> ProjectContext {
    let path = project_path(project);
    let name = path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| project.trim().to_string());
    let mut ctx = ProjectContext {
        name: name.clone(),
        path: path.as_ref().map(|p| p.display().to_string()),
        ..Default::default()
    };
    if let Some(dir) = path.as_deref() {
        let git = |args: &[&str]| crate::fleet::git(dir, args);
        ctx.branch = git(&["branch", "--show-current"]).trim().to_string();
        ctx.remote = git(&["remote", "get-url", "origin"]).trim().to_string();
        ctx.commits = git(&["log", "--oneline", "-12"])
            .lines()
            .map(String::from)
            .collect();
        ctx.dirty = git(&["status", "--porcelain"]).lines().count();
        ctx.stack = crate::fleet::detect_stack(dir);
        let read = |rel: &str| std::fs::read_to_string(dir.join(rel)).unwrap_or_default();
        ctx.rules = head(&read("AGENTS.md"), 60, 3500);
        let phases = ["docs/agentos/PHASES.md", "PHASES.md", "docs/PHASES.md"]
            .iter()
            .map(|r| read(r))
            .find(|s| !s.trim().is_empty())
            .unwrap_or_default();
        ctx.phases = head(&phases, 80, 4000);
        let tasks = ["docs/agentos/TASKS.md", "TASKS.md", "docs/TASKS.md"]
            .iter()
            .map(|r| read(r))
            .find(|s| !s.trim().is_empty())
            .unwrap_or_default();
        ctx.tasks = open_tasks(&tasks, 40);
        ctx.checks = checks_for(&ctx.stack, |f| dir.join(f).exists(), &read("package.json"));
        ctx.files = READ_FIRST
            .iter()
            .filter(|f| dir.join(f).is_file())
            .map(|f| f.to_string())
            .collect();
        if let Ok((_, out, _)) = procs::run_with_timeout(
            "gh",
            &[
                "pr",
                "list",
                "--state",
                "open",
                "--limit",
                "10",
                "--json",
                "number,title,headRefName",
            ],
            None,
            Some(dir),
            std::time::Duration::from_secs(20),
        ) {
            if let Ok(serde_json::Value::Array(a)) = serde_json::from_str::<serde_json::Value>(&out)
            {
                ctx.open_prs = a
                    .iter()
                    .map(|p| {
                        format!(
                            "#{} {} ({})",
                            p["number"],
                            p["title"].as_str().unwrap_or(""),
                            p["headRefName"].as_str().unwrap_or("")
                        )
                    })
                    .collect();
            }
        }
    }
    ctx.note = v
        .read_note(&format!("01_Projects/{name}.md"))
        .map(|s| head(&s, 40, 2500))
        .unwrap_or_default();
    if let Ok(ms) = crate::missions::list(200) {
        ctx.missions = ms
            .iter()
            .filter(|m| {
                m.repo.eq_ignore_ascii_case(&name) || m.repo.eq_ignore_ascii_case(project.trim())
            })
            .take(10)
            .map(|m| {
                format!(
                    "{} [{}] {}{}",
                    m.id,
                    m.status,
                    m.goal.lines().next().unwrap_or(""),
                    m.pr_url
                        .as_deref()
                        .map(|u| format!(" ({u})"))
                        .unwrap_or_default()
                )
            })
            .collect();
    }
    ctx
}

fn section(out: &mut String, title: &str, body: &str) {
    if !body.trim().is_empty() {
        out.push_str(&format!("### {title}\n\n{}\n\n", body.trim_end()));
    }
}

fn bullets(items: &[String]) -> String {
    items
        .iter()
        .map(|i| format!("- {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The context block: facts only, between the markers. Pure.
pub fn context_block(ctx: &ProjectContext, session: Option<&SessionBrief>) -> String {
    let mut s = format!("{CTX_START}\n## Project context (gathered by AgentOS Studio)\n\n");
    let mut facts = vec![format!("Project: {}", ctx.name)];
    if let Some(p) = &ctx.path {
        facts.push(format!("Folder: {p}"));
    }
    if !ctx.remote.is_empty() {
        facts.push(format!("Remote: {}", ctx.remote));
    }
    if !ctx.branch.is_empty() {
        facts.push(format!(
            "Branch: {}{}",
            ctx.branch,
            if ctx.dirty > 0 {
                format!(" ({} uncommitted files)", ctx.dirty)
            } else {
                String::new()
            }
        ));
    }
    if !ctx.stack.is_empty() {
        facts.push(format!("Stack: {}", ctx.stack.join(", ")));
    }
    s.push_str(&bullets(&facts));
    s.push_str("\n\n");
    let mut read = ctx.files.clone();
    if let Some(b) = session {
        read.extend(b.written.iter().filter(|w| !ctx.files.contains(w)).cloned());
    }
    section(&mut s, "Files to read first", &bullets(&read));
    section(&mut s, "Recent commits", &bullets(&ctx.commits));
    section(&mut s, "Open pull requests", &bullets(&ctx.open_prs));
    section(&mut s, "Missions", &bullets(&ctx.missions));
    section(&mut s, "Phases", &ctx.phases);
    section(&mut s, "Open tasks", &ctx.tasks);
    section(&mut s, "Project rules (AGENTS.md)", &ctx.rules);
    section(&mut s, "Project note", &ctx.note);
    if let Some(b) = session {
        let mut t = format!("Session: {} ({})\n", b.title, b.id);
        if !b.summary.is_empty() {
            t.push_str(&format!(
                "\nWhere it stood (its last compaction summary):\n{}\n",
                b.summary
            ));
        }
        if !b.last_request.is_empty() {
            t.push_str(&format!("\nLast request: {}\n", b.last_request));
        }
        if !b.last_reply.is_empty() {
            t.push_str(&format!("\nLast reply: {}\n", b.last_reply));
        }
        if !b.written.is_empty() {
            t.push_str(&format!("\nFiles it changed:\n{}\n", bullets(&b.written)));
        }
        if !b.decisions.is_empty() {
            t.push_str(&format!("\nDecisions:\n{}\n", bullets(&b.decisions)));
        }
        if !b.ids.is_empty() {
            t.push_str(&format!("\nReferences: {}\n", b.ids.join(", ")));
        }
        section(&mut s, "Previous session", &t);
    }
    s.push_str(CTX_END);
    s
}

/// Replace the context block in a saved prompt, or append one when it has none. Pure.
pub fn refresh_block(body: &str, block: &str) -> String {
    match (body.find(CTX_START), body.find(CTX_END)) {
        (Some(a), Some(b)) if b > a => {
            format!("{}{}{}", &body[..a], block, &body[b + CTX_END.len()..])
        }
        _ => format!("{}\n\n{}\n", body.trim_end(), block),
    }
}

/// A short title: the owner's words when given, else what the prompt is for. Cut at a word. Pure.
pub fn title_for(kind: &str, ask: &str, project: &str) -> String {
    let base = match (kind, ask.trim()) {
        (_, a) if !a.is_empty() => a.lines().next().unwrap_or(a).to_string(),
        ("continue", _) => format!("Continue {project}"),
        ("new_session", _) => format!("Take over the {project} session"),
        ("review", _) => format!("Review the {project} change"),
        ("fix", _) => format!("Fix a problem in {project}"),
        _ => format!("Work on {project}"),
    };
    if base.chars().count() <= 80 {
        return base;
    }
    let cut: String = base.chars().take(80).collect();
    match cut.rfind(' ') {
        Some(i) if i > 40 => cut[..i].to_string(),
        _ => cut,
    }
}

/// Drop Hermes' compaction preamble ("[CONTEXT COMPACTION ...] treat as reference") so only the
/// summary itself is handed on. Pure.
pub fn strip_compaction_preamble(summary: &str) -> String {
    let t = summary.trim_start();
    if t.starts_with("[CONTEXT COMPACTION") {
        if let Some(i) = t.find("\n## ") {
            return t[i + 1..].trim().to_string();
        }
    }
    t.trim().to_string()
}

fn target_style(target: &str) -> &'static str {
    match target {
        "chat" => "You have no tools in this chat. Work from the context below, say plainly what you would need to run or read, and give me changes as complete files or exact patches.",
        "claude-desktop" => "You may not have this repository open. Use the context below; if you have the agentos-hub tools, read the project note and shared memory before you start.",
        "cursor" => "You are working in this repository in Cursor. Keep changes focused and run the checks below in the terminal.",
        _ => "You are working in this repository with a shell. Read the files named below before you change anything, and run the checks before you say it is done.",
    }
}

/// A complete first draft of a prompt from the facts: role, goal, context, task, constraints,
/// finish line and how to report back. Pure.
pub fn compose(
    kind: &str,
    target: &str,
    ask: &str,
    ctx: &ProjectContext,
    session: Option<&SessionBrief>,
) -> String {
    let ask = ask.trim();
    let goal = match kind {
        "continue" => format!(
            "Continue the work on {}. {}",
            ctx.name,
            if ask.is_empty() {
                "Pick up the first open task below and finish it."
            } else {
                ask
            }
        ),
        "new_session" => format!(
            "Take over from the previous session on {} and carry its work to the finish. {}",
            ctx.name, ask
        ),
        "review" => format!(
            "Review the change on {} as a strict second engineer. {}",
            ctx.name,
            if ask.is_empty() {
                "Find bugs, missing tests and unsafe behaviour; say what blocks a merge."
            } else {
                ask
            }
        ),
        "fix" => format!(
            "Fix this problem in {}: {}",
            ctx.name,
            if ask.is_empty() { "(describe it)" } else { ask }
        ),
        _ => {
            if ask.is_empty() {
                format!("Work on {}.", ctx.name)
            } else {
                ask.to_string()
            }
        }
    };
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", title_for(kind, ask, &ctx.name)));
    s.push_str(&format!("## Role\n\n{}\n\n", target_style(target)));
    s.push_str(&format!("## Goal\n\n{}\n\n", goal.trim()));
    s.push_str(&context_block(ctx, session));
    s.push_str("\n\n## Task\n\n");
    s.push_str(match kind {
        "continue" => "1. Read the project rules and the open tasks above.\n2. Take the first unfinished task, on its own branch.\n3. Implement it with tests, run the checks, open a pull request.\n4. Tick the task off in TASKS.md in the same pull request.\n",
        "new_session" => "1. Read the previous session's state above and confirm what is finished from the commits, not from the summary.\n2. Finish what it left open, in the order it planned.\n3. Run the checks and open a pull request for each finished piece.\n",
        "review" => "1. Read the diff and the files it touches.\n2. Run the checks.\n3. List findings by severity with file and line, then a verdict: approve or request changes.\n",
        "fix" => "1. Reproduce the problem and find the root cause before changing code.\n2. Add a test that fails without the fix.\n3. Fix it, run the checks, open a pull request.\n",
        _ => "1. Read the context above.\n2. Do the work, run the checks, and report.\n",
    });
    s.push_str("\n## Constraints\n\n- Follow the project rules above; they override this prompt where they conflict.\n- Work on a branch and open a pull request; never push to main.\n- Do not print, log or commit secrets.\n- No em dashes or en dashes in code, comments, commits or docs: use hyphens, colons or periods.\n\n");
    if !ctx.checks.is_empty() {
        s.push_str(&format!("## Done when\n\n- These pass:\n{}\n- Every pull request says what changed and how it was verified.\n\n", ctx.checks.iter().map(|c| format!("  - `{c}`")).collect::<Vec<_>>().join("\n")));
    } else {
        s.push_str("## Done when\n\n- The change works and is tested, and every pull request says how it was verified.\n\n");
    }
    s.push_str("## Report back\n\nEnd with: what changed, every pull request link, what you verified and how, and anything left open. If something in this prompt is unclear or wrong, say so before you start rather than guessing.\n");
    s
}

// ---------------------------------------------------------------------------------------------
// Previous sessions

fn state_db() -> PathBuf {
    procs::hermes_home().join("state.db")
}

fn open_state(path: &Path) -> Result<Connection> {
    let uri = format!(
        "file:{}?mode=ro",
        path.display().to_string().replace('\\', "/")
    );
    Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open {} read-only", path.display()))
}

/// Recent named Hermes sessions with real work in them, newest first.
pub fn sessions_at(path: &Path, limit: usize) -> Result<Vec<SessionRow>> {
    let c = open_state(path)?;
    let mut st = c.prepare(
        "SELECT id, COALESCE(title, ''), COALESCE(message_count, 0), CAST(COALESCE(last_activity_at, started_at, 0) AS INTEGER)
         FROM sessions
         WHERE COALESCE(message_count, 0) > 4 AND COALESCE(hidden, 0) = 0
           AND source NOT IN ('oneshot', 'subagent', 'kanban')
         ORDER BY COALESCE(last_activity_at, started_at) DESC LIMIT ?1",
    )?;
    let out = st
        .query_map([limit as i64], |r| {
            Ok(SessionRow {
                id: r.get(0)?,
                title: r.get(1)?,
                messages: r.get(2)?,
                at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

pub fn sessions(limit: usize) -> Result<Vec<SessionRow>> {
    sessions_at(&state_db(), limit)
}

fn clip(s: &str, n: usize) -> String {
    let t = s.trim();
    if t.chars().count() > n {
        format!("{}...", t.chars().take(n).collect::<String>())
    } else {
        t.to_string()
    }
}

/// What a session was doing: its last compaction summary, last request and reply, and the
/// files, decisions and references the context ledger finds in its last messages. Read-only.
pub fn session_brief_at(path: &Path, id: &str) -> Result<SessionBrief> {
    let c = open_state(path)?;
    let title: String = c
        .query_row(
            "SELECT COALESCE(title, '') FROM sessions WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("no Hermes session '{id}'"))?;
    let summary: String = c
        .query_row(
            "SELECT COALESCE(content, '') FROM messages WHERE session_id = ?1 AND _compressed_summary = 1 ORDER BY id DESC LIMIT 1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None)
        .unwrap_or_default();
    let mut st = c.prepare(
        "SELECT id, role, COALESCE(content, ''), tool_name, tool_calls, COALESCE(timestamp, 0)
         FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT 400",
    )?;
    let mut msgs: Vec<crate::context_ledger::Msg> = st
        .query_map([id], |r| {
            Ok(crate::context_ledger::Msg {
                id: r.get(0)?,
                role: r.get(1)?,
                content: r.get(2)?,
                tool_name: r.get(3)?,
                tool_calls: crate::context_ledger::parse_calls(r.get(4)?),
                at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    msgs.reverse();
    let a = crate::context_ledger::anchors(&msgs);
    let last_reply = msgs
        .iter()
        .rev()
        .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
        .map(|m| clip(&m.content, 1500))
        .unwrap_or_default();
    let redact = crate::constitution_gate::redact;
    Ok(SessionBrief {
        id: id.into(),
        title,
        last_request: redact(&clip(&a.last_request, 1500)),
        last_reply: redact(&last_reply),
        summary: redact(&clip(&strip_compaction_preamble(&summary), 3000)),
        written: a.written.into_iter().take(25).collect(),
        decisions: a
            .decisions
            .into_iter()
            .take(12)
            .map(|d| redact(&clip(&d, 240)))
            .collect(),
        ids: a.ids.into_iter().take(20).collect(),
    })
}

pub fn session_brief(id: &str) -> Result<SessionBrief> {
    session_brief_at(&state_db(), id)
}

// ---------------------------------------------------------------------------------------------
// Checks before it leaves

/// Rough token count, four characters per token.
pub fn tokens(text: &str) -> i64 {
    (text.chars().count() as i64 + 3) / 4
}

/// Rule checks: a goal, a finish line, constraints, secrets, dashes, the target's size, and
/// paths that do not exist in the project. `exists` answers for a repo-relative path. Pure.
pub fn lint(body: &str, target: &str, exists: impl Fn(&str) -> bool) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut push = |level: &str, text: String| {
        out.push(Finding {
            level: level.into(),
            text,
        })
    };
    let lower = body.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if !has(&["## goal", "goal:", "# "]) {
        push(
            "warn",
            "No goal: say in one sentence what the outcome is.".into(),
        );
    }
    if !has(&[
        "done when",
        "acceptance",
        "finished when",
        "definition of done",
    ]) {
        push(
            "warn",
            "No finish line: add a \"Done when\" list so the agent knows when to stop.".into(),
        );
    }
    if !has(&["constraint", "never", "do not", "don't", "must"]) {
        push(
            "info",
            "No constraints: name what the agent must not do (push to main, touch other files)."
                .into(),
        );
    }
    if !has(&["report", "reply with", "end with"]) {
        push(
            "info",
            "No report format: say what the agent should hand back.".into(),
        );
    }
    if crate::constitution_gate::redact(body) != body {
        push("error", "Looks like it contains a secret (a key or token). Send fixes this by blanking it, but check the prompt.".into());
    }
    if body.contains('\u{2014}') || body.contains('\u{2013}') {
        push(
            "warn",
            "Contains em or en dashes (Law 2). Fix replaces them with hyphens.".into(),
        );
    }
    let budget = TARGETS
        .iter()
        .find(|t| t.0 == target)
        .map(|t| t.2)
        .unwrap_or(6000);
    let n = tokens(body);
    if n > budget {
        push("warn", format!("About {n} tokens, more than the {budget} that suit this target; trim the context or split the task."));
    }
    // Paths are checked in what the owner wrote; the context block holds gathered facts, which may
    // name files outside the repository (agent rule files in the home folder).
    let own = match (body.find(CTX_START), body.find(CTX_END)) {
        (Some(a), Some(b)) if b > a => format!("{}{}", &body[..a], &body[b..]),
        _ => body.to_string(),
    };
    let mut missing = Vec::new();
    for w in own.split(|c: char| c.is_whitespace() || c == '`' || c == '(' || c == ')') {
        let w = w.trim_matches(|c: char| matches!(c, ',' | '.' | ':' | ';' | '"' | '\''));
        let looks_like_path = w.contains('/')
            && !w.contains("://")
            && !w.starts_with('/')
            && !w.contains(':')
            && w.rsplit('/')
                .next()
                .is_some_and(|f| f.contains('.') && f.len() > 2);
        if looks_like_path && !exists(w) && !missing.contains(&w.to_string()) {
            missing.push(w.to_string());
        }
    }
    if !missing.is_empty() {
        push(
            "warn",
            format!(
                "Paths not found in the project: {}",
                missing.into_iter().take(6).collect::<Vec<_>>().join(", ")
            ),
        );
    }
    out
}

/// What Send does to the text before it leaves: secrets blanked, dashes made hyphens. Pure.
pub fn clean(body: &str) -> String {
    crate::constitution_gate::redact(body).replace(['\u{2014}', '\u{2013}'], "-")
}

// ---------------------------------------------------------------------------------------------
// Sharpen and second reading (Hermes)

/// The instruction for a Hermes turn over a prompt. Pure.
pub fn ai_instruction(mode: &str, body: &str, target: &str) -> String {
    let label = TARGETS
        .iter()
        .find(|t| t.0 == target)
        .map(|t| t.1)
        .unwrap_or(target);
    match mode {
        "questions" => format!(
            "You are about to receive the prompt below as {label}. Do not do the work. List, as short bullets, every question you would have to ask before you could finish it without guessing, and every instruction that is ambiguous or contradicts another. If nothing is unclear, reply exactly: Nothing is unclear.\n\n--- PROMPT ---\n{body}\n--- END ---"
        ),
        _ => format!(
            "Rewrite the prompt below so {label} can finish the work without follow-up questions. Keep every fact, path, command and constraint; keep the block between {CTX_START} and {CTX_END} exactly as it is; keep the sections Role, Goal, Task, Constraints, Done when, Report back. Make the goal and the finish line concrete, cut repetition, and never use em dashes or en dashes. Reply with the rewritten prompt only, no preamble.\n\n--- PROMPT ---\n{body}\n--- END ---"
        ),
    }
}

/// Strip a model's wrapping (code fences, a leading "Here is") from a rewrite. Pure.
pub fn unwrap_reply(text: &str) -> String {
    let t = text.trim();
    let t = t
        .strip_prefix("```markdown")
        .or_else(|| t.strip_prefix("```md"))
        .or_else(|| t.strip_prefix("```"))
        .map(|r| r.trim_end().trim_end_matches("```"))
        .unwrap_or(t);
    t.trim().to_string()
}

/// One quiet Hermes turn (it stays out of the chat list) over the prompt.
pub fn ai_pass(mode: &str, body: &str, target: &str, dir: Option<&Path>) -> Result<String> {
    let q = std::env::temp_dir().join(format!("agentos-prompt-{}.md", std::process::id()));
    std::fs::write(&q, ai_instruction(mode, body, target))?;
    let home = dirs::home_dir().unwrap_or_else(std::env::temp_dir);
    let cwd = dir.map(Path::to_path_buf).unwrap_or(home);
    let qs = q.display().to_string();
    let cs = cwd.display().to_string();
    let out = procs::run_with_timeout(
        "hermes",
        &["chat", "-Q", "--oneshot", "--query-file", &qs, "--in", &cs],
        None,
        Some(&cwd),
        std::time::Duration::from_secs(300),
    );
    let _ = std::fs::remove_file(&q);
    let (_, text, err) = out?;
    let text = unwrap_reply(&text);
    if text.is_empty() {
        bail!("Hermes returned nothing: {}", clip(&err, 300));
    }
    Ok(if mode == "questions" {
        text
    } else {
        clean(&text)
    })
}

// ---------------------------------------------------------------------------------------------
// Delivery

/// The repo-relative file a "write into the project" handoff creates. Pure.
pub fn handoff_rel(p: &Prompt) -> String {
    format!(".agentos/handoff/P-{:03}-{}.md", p.id, slug(&p.title))
}

/// Write the prompt into the project, where an agent opened in that folder finds it.
pub fn write_into_project(p: &Prompt) -> Result<PathBuf> {
    let dir = project_path(&p.project)
        .ok_or_else(|| anyhow!("'{}' is not a project folder Studio knows", p.project))?;
    let path = dir.join(handoff_rel(p));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, clean(&p.body))?;
    Ok(path)
}

/// Put the prompt on the bus as a task for one agent; its watcher picks it up like any task.
pub fn put_on_bus(v: &Vault, p: &Prompt, agent: &str) -> Result<String> {
    let mut body = clean(&p.body);
    if let Some(dir) = project_path(&p.project) {
        body = format!("Repository: {}\n\n{body}", dir.display());
    }
    let r = v.delegate_task(crate::vault::DelegateArgs {
        title: p.title.clone(),
        assigned_to: agent.into(),
        creator: "owner".into(),
        project: Path::new(&p.project)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.project.clone()),
        context_files: vec![vault_rel(p)],
        instructions: body,
        priority: "medium".into(),
    })?;
    Ok(r.get("id")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_db<T>(f: impl FnOnce(&Connection) -> T) -> T {
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let c = crate::store::open().unwrap();
        let out = f(&c);
        crate::store::set_test_db(None);
        out
    }

    fn prompt(body: &str) -> Prompt {
        Prompt {
            id: 0,
            title: "Continue phase 2".into(),
            project: "agentos-studio".into(),
            kind: "continue".into(),
            target: "claude-code".into(),
            body: body.into(),
            version: 0,
            uses: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn ctx() -> ProjectContext {
        ProjectContext {
            name: "shop".into(),
            path: Some("C:/r/shop".into()),
            branch: "main".into(),
            remote: "https://github.com/o/shop".into(),
            stack: vec!["node".into()],
            commits: vec!["abc1234 feat: cart".into()],
            tasks: "## Phase 2\n- [ ] Add checkout".into(),
            checks: vec!["npm test".into()],
            ..Default::default()
        }
    }

    #[test]
    fn saving_a_changed_body_keeps_every_version() {
        with_db(|c| {
            let p = save_in(c, &prompt("first"), "created").unwrap();
            assert_eq!((p.id > 0, p.version), (true, 1));
            let same = save_in(c, &p, "no change").unwrap();
            assert_eq!(same.version, 1, "an unchanged body is not a new version");
            let mut next = p.clone();
            next.body = "second".into();
            let p2 = save_in(c, &next, "sharpened").unwrap();
            assert_eq!(p2.version, 2);
            let vs = versions_in(c, p.id).unwrap();
            assert_eq!(
                vs.iter().map(|v| v.body.as_str()).collect::<Vec<_>>(),
                ["second", "first"]
            );
            record_use_in(c, p.id, "clipboard", "").unwrap();
            assert_eq!(get_in(c, p.id).unwrap().unwrap().uses, 1);
            assert_eq!(handoffs_in(c, p.id).unwrap()[0].version, 2);
            assert_eq!(
                list_in(c, Some("agentos-studio"), Some("second"))
                    .unwrap()
                    .len(),
                1
            );
            assert!(list_in(c, Some("other"), None).unwrap().is_empty());
            delete_in(c, p.id).unwrap();
            assert!(get_in(c, p.id).unwrap().is_none());
            assert!(versions_in(c, p.id).unwrap().is_empty());
            assert!(save_in(
                c,
                &Prompt {
                    kind: "nope".into(),
                    ..prompt("x")
                },
                ""
            )
            .is_err());
        });
    }

    #[test]
    fn composed_prompt_carries_the_project_and_refreshes_in_place() {
        let body = compose("continue", "claude-code", "", &ctx(), None);
        for want in [
            "## Goal",
            "## Constraints",
            "## Done when",
            "`npm test`",
            "- [ ] Add checkout",
            "abc1234",
            CTX_START,
            CTX_END,
        ] {
            assert!(body.contains(want), "missing {want}");
        }
        let mut later = ctx();
        later.commits = vec!["def5678 feat: checkout".into()];
        let edited = body.replace("## Report back", "## Report back\n\nOwner note kept.");
        let fresh = refresh_block(&edited, &context_block(&later, None));
        assert!(fresh.contains("def5678") && !fresh.contains("abc1234"));
        assert!(
            fresh.contains("Owner note kept."),
            "text outside the block survives"
        );
        assert_eq!(fresh.matches(CTX_START).count(), 1);
        // A prompt without a block gets one appended.
        assert!(refresh_block("Just do it", &context_block(&later, None)).contains("def5678"));
    }

    #[test]
    fn a_session_handoff_lists_what_the_session_did() {
        let b = SessionBrief {
            id: "local-1".into(),
            title: "Build phase C".into(),
            last_request: "release it".into(),
            written: vec!["src/a.rs".into()],
            decisions: vec!["chose squash merges".into()],
            ..Default::default()
        };
        let body = compose("new_session", "antigravity", "", &ctx(), Some(&b));
        for want in [
            "Previous session",
            "Build phase C",
            "release it",
            "src/a.rs",
            "chose squash merges",
        ] {
            assert!(body.contains(want), "missing {want}");
        }
    }

    #[test]
    fn lint_finds_gaps_secrets_dashes_size_and_missing_paths() {
        let good = compose("continue", "claude-code", "", &ctx(), None);
        let f = lint(&good, "claude-code", |_| true);
        assert!(f.iter().all(|x| x.level != "error"), "{f:?}");
        assert!(f.iter().all(|x| !x.text.starts_with("No ")), "{f:?}");
        let key = format!("sk-or-v1-{}", "a".repeat(40));
        let bad = format!("Fix it with {key} and read src/missing.rs \u{2014} quickly");
        let f = lint(&bad, "chat", |p| p != "src/missing.rs");
        let text = f
            .iter()
            .map(|x| x.text.as_str())
            .collect::<Vec<_>>()
            .join("|");
        assert!(f.iter().any(|x| x.level == "error"), "secret is an error");
        assert!(
            text.contains("No finish line")
                && text.contains("dashes")
                && text.contains("src/missing.rs")
        );
        assert!(lint(&"word ".repeat(4000), "chat", |_| true)
            .iter()
            .any(|x| x.text.contains("tokens")));
        let cleaned = clean(&bad);
        assert!(!cleaned.contains(&key) && !cleaned.contains('\u{2014}'));
        assert!(!lint("see https://x.io/a/b.html", "chat", |_| false)
            .iter()
            .any(|x| x.text.starts_with("Paths")));
    }

    #[test]
    fn open_tasks_and_checks_come_from_the_repo() {
        let md =
            "# Tasks\n## Phase 1\n- [x] done\n## Phase 2\n- [ ] a\n- [ ] b\n## Phase 3\n- [x] c\n";
        assert_eq!(open_tasks(md, 10), "## Phase 2\n- [ ] a\n- [ ] b");
        let pkg = r#"{"scripts": {"build": "vite build", "dev": "vite"}}"#;
        let c = checks_for(
            &[],
            |f| f == "package.json" || f == "src-tauri/Cargo.toml",
            pkg,
        );
        assert!(c.contains(&"npm run build".to_string()));
        assert!(
            !c.contains(&"npm test".to_string()),
            "no test script, no npm test"
        );
        assert_eq!(title_for("continue", "", "shop"), "Continue shop");
        let long = "word ".repeat(30);
        let t = title_for("fix", &long, "shop");
        assert!(t.len() <= 80 && t.ends_with("word"));
        assert_eq!(
            strip_compaction_preamble("[CONTEXT COMPACTION x] boring\n## Goal\nShip"),
            "## Goal\nShip"
        );
        assert_eq!(strip_compaction_preamble("Plain summary"), "Plain summary");
        assert!(c.contains(&"cd src-tauri && cargo test".to_string()));
    }

    #[test]
    fn ai_reply_unwraps_and_instructions_keep_the_block() {
        assert_eq!(unwrap_reply("```markdown\n# A\n```"), "# A");
        assert_eq!(unwrap_reply("# B"), "# B");
        assert!(ai_instruction("sharpen", "x", "cursor").contains(CTX_START));
        assert!(ai_instruction("questions", "x", "chat").contains("Nothing is unclear"));
    }

    #[test]
    fn vault_copy_and_handoff_paths_are_stable() {
        let p = Prompt {
            id: 7,
            project: "C:/repos/My Shop".into(),
            ..prompt("body")
        };
        assert_eq!(vault_rel(&p), "05_Prompts/my-shop/P-007.md");
        assert_eq!(
            handoff_rel(&p),
            ".agentos/handoff/P-007-continue-phase-2.md"
        );
        let n = vault_note(&p);
        assert!(n.starts_with("---\ngenerated_by: agentos-studio\n") && n.contains("prompt_id: 7"));
    }

    /// Live: draft from the real agentos-studio project and the newest real Hermes session, check
    /// it, and (with PROMPTS_LIVE_AI=1) ask Hermes what is unclear. Writes nothing.
    /// `cargo test --lib -- --ignored prompts_live --nocapture`
    #[test]
    #[ignore]
    fn prompts_live() {
        let v = Vault::new(crate::settings::vault_path());
        let ctx = gather(&v, "agentos-studio");
        println!(
            "path={:?} branch={} commits={} prs={} missions={} checks={:?}",
            ctx.path,
            ctx.branch,
            ctx.commits.len(),
            ctx.open_prs.len(),
            ctx.missions.len(),
            ctx.checks
        );
        assert!(
            ctx.path.is_some() && !ctx.commits.is_empty(),
            "the real project resolves"
        );
        let s = sessions(5).unwrap();
        println!(
            "sessions: {:?}",
            s.iter().map(|r| (&r.title, r.messages)).collect::<Vec<_>>()
        );
        let b = s.first().map(|r| session_brief(&r.id).unwrap());
        if let Some(b) = &b {
            println!(
                "brief: written={} decisions={} summary={} chars",
                b.written.len(),
                b.decisions.len(),
                b.summary.len()
            );
        }
        let body = compose(
            "new_session",
            "claude-code",
            "Finish the open roadmap item",
            &ctx,
            b.as_ref(),
        );
        let dir = ctx.path.as_ref().map(PathBuf::from).unwrap();
        let f = lint(&body, "claude-code", |r| dir.join(r).exists());
        println!("tokens={} findings={f:?}", tokens(&body));
        println!("{}", body.chars().take(1800).collect::<String>());
        assert!(f.iter().all(|x| x.level != "error"));
        if std::env::var("PROMPTS_LIVE_AI").is_ok() {
            let q = ai_pass("questions", &body, "claude-code", Some(&dir)).unwrap();
            println!("--- questions ---\n{q}");
            assert!(!q.is_empty());
        }
    }

    #[test]
    fn session_brief_reads_a_hermes_state_db_read_only() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.db");
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE sessions (id TEXT, title TEXT, message_count INT, last_activity_at REAL, started_at REAL, hidden INT, source TEXT);
             CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, tool_name TEXT, tool_calls TEXT, timestamp REAL, _compressed_summary INT DEFAULT 0);
             INSERT INTO sessions VALUES ('s1', 'Ship it', 9, 10, 1, 0, 'local'), ('s2', 'Quick', 9, 20, 1, 0, 'oneshot');
             INSERT INTO messages (session_id, role, content, tool_calls, timestamp, _compressed_summary) VALUES
               ('s1', 'user', 'Please release version two now', NULL, 1, 0),
               ('s1', 'assistant', 'Summary: phase 1 merged', NULL, 2, 1),
               ('s1', 'assistant', '', '[{\"function\":{\"name\":\"write_file\",\"arguments\":\"{\\\"path\\\":\\\"src/lib.rs\\\"}\"}}]', 3, 0),
               ('s1', 'assistant', 'Released and we decided to keep squash merges.', NULL, 4, 0);",
        )
        .unwrap();
        drop(c);
        let rows = sessions_at(&path, 10).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["s1"],
            "oneshot sessions are hidden"
        );
        let b = session_brief_at(&path, "s1").unwrap();
        assert_eq!(b.title, "Ship it");
        assert!(b.summary.contains("phase 1 merged"));
        assert!(b.last_reply.contains("Released"));
        assert_eq!(b.written, ["src/lib.rs"]);
        assert!(session_brief_at(&path, "nope").is_err());
    }
}
