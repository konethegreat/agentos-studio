//! Vault store: the Agent Bus task queue and shared memory, kept as Markdown files.
//!
//! File layout and formats are compatible with the AgentOS Python server, so both can
//! operate on the same vault concurrently.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const AGENTS: [&str; 5] = ["hermes", "claude-code", "qoder", "antigravity", "human"];
pub const PRIORITIES: [&str; 4] = ["low", "medium", "high", "critical"];
const DASH_START: &str = "<!-- AGENTOS:TASKS:START -->";
const DASH_END: &str = "<!-- AGENTOS:TASKS:END -->";
const RESERVED_MEMORY: [&str; 2] = ["system_context", "decision_log"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    InProgress,
    Completed,
}

impl State {
    pub fn dir(self) -> &'static str {
        match self {
            State::Pending => "Pending",
            State::InProgress => "In_Progress",
            State::Completed => "Completed",
        }
    }
    pub fn status(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::InProgress => "in_progress",
            State::Completed => "completed",
        }
    }
    pub fn parse(s: &str) -> Result<State> {
        match s.trim().to_lowercase().replace(['-', ' '], "_").as_str() {
            "pending" => Ok(State::Pending),
            "in_progress" | "inprogress" => Ok(State::InProgress),
            "completed" | "done" => Ok(State::Completed),
            other => bail!("Unknown state '{other}'. Use pending, in_progress or completed."),
        }
    }
    pub const ALL: [State; 3] = [State::Pending, State::InProgress, State::Completed];
}

/// Ordered YAML frontmatter value.
#[derive(Debug, Clone, PartialEq)]
pub enum Fm {
    Str(String),
    List(Vec<String>),
}

#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub meta: Vec<(String, Fm)>,
    pub body: String,
}

impl Doc {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.meta
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| match v {
                Fm::Str(s) => Some(s.as_str()),
                Fm::List(_) => None,
            })
    }
    pub fn list(&self, key: &str) -> Vec<String> {
        self.meta
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| match v {
                Fm::List(l) => l.clone(),
                Fm::Str(s) if !s.is_empty() => vec![s.clone()],
                Fm::Str(_) => vec![],
            })
            .unwrap_or_default()
    }
    pub fn set(&mut self, key: &str, val: Fm) {
        if let Some(slot) = self.meta.iter_mut().find(|(k, _)| k == key) {
            slot.1 = val;
        } else {
            self.meta.push((key.to_string(), val));
        }
    }
    pub fn set_str(&mut self, key: &str, val: impl Into<String>) {
        self.set(key, Fm::Str(val.into()));
    }

    pub fn parse(raw: &str) -> Doc {
        let raw = raw.replace("\r\n", "\n");
        if let Some(rest) = raw.strip_prefix("---") {
            if let Some(end) = rest.find("\n---") {
                let fm = &rest[..end];
                let after = &rest[end + 4..];
                let body = after.trim_start_matches('\n');
                let mut meta = Vec::new();
                if let Ok(serde_yaml::Value::Mapping(map)) =
                    serde_yaml::from_str::<serde_yaml::Value>(fm)
                {
                    for (k, v) in map {
                        let key = scalar(&k);
                        let val = match v {
                            serde_yaml::Value::Sequence(items) => {
                                Fm::List(items.iter().map(scalar).collect())
                            }
                            other => Fm::Str(scalar(&other)),
                        };
                        meta.push((key, val));
                    }
                }
                return Doc {
                    meta,
                    body: body.to_string(),
                };
            }
        }
        Doc {
            meta: Vec::new(),
            body: raw,
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::from("---\n");
        for (k, v) in &self.meta {
            match v {
                Fm::Str(s) => out.push_str(&format!("{k}: {}\n", yaml_scalar(s))),
                Fm::List(items) if items.is_empty() => out.push_str(&format!("{k}: []\n")),
                Fm::List(items) => {
                    out.push_str(&format!("{k}:\n"));
                    for i in items {
                        out.push_str(&format!("- {}\n", yaml_scalar(i)));
                    }
                }
            }
        }
        out.push_str("---\n\n");
        out.push_str(self.body.trim_end());
        out.push('\n');
        out
    }
}

fn scalar(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::Null => String::new(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::String(s) => s.clone(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

/// Emit a YAML scalar that PyYAML and serde_yaml both read back as the same string.
fn yaml_scalar(s: &str) -> String {
    let plain =
        Regex::new(r"^[A-Za-z][A-Za-z0-9_/.\-]*( [A-Za-z0-9_/.,()\-]+)*$").expect("static regex");
    let reserved = ["true", "false", "yes", "no", "on", "off", "null", "y", "n"];
    if plain.is_match(s) && !reserved.contains(&s.to_lowercase().as_str()) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "''"))
    }
}

pub fn now() -> String {
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

pub fn slug(text: &str, max: usize) -> String {
    let re = Regex::new(r"[^A-Za-z0-9]+").expect("static regex");
    let s = re.replace_all(text, "_");
    let s = s.trim_matches('_');
    let cut: String = s.chars().take(max).collect();
    let cut = cut.trim_end_matches('_').to_string();
    if cut.is_empty() {
        "untitled".into()
    } else {
        cut
    }
}

pub fn norm_agent(name: &str) -> Result<String> {
    let n = name.trim().to_lowercase().replace(['_', ' '], "-");
    let n = match n.as_str() {
        "claude" | "claudecode" => "claude-code".to_string(),
        "user" => "human".to_string(),
        _ => n,
    };
    if AGENTS.contains(&n.as_str()) {
        Ok(n)
    } else {
        bail!("Invalid agent '{name}'. Valid: {}", AGENTS.join(", "))
    }
}

pub fn norm_task_id(id: &str) -> Result<String> {
    let t = id.trim().to_uppercase();
    if Regex::new(r"^TASK-\d{3,}$")
        .expect("static regex")
        .is_match(&t)
    {
        Ok(t)
    } else {
        bail!("Invalid task_id '{id}'. Expected format TASK-001.")
    }
}

fn atomic_write(path: &Path, text: &str) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("no parent for {}", path.display()))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".tmp_{}.md", uuid::Uuid::new_v4().simple()));
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Cross-process lock for bus mutations (GUI and MCP processes share one vault).
struct BusLock(PathBuf);

impl BusLock {
    fn acquire(dir: &Path) -> Result<BusLock> {
        fs::create_dir_all(dir)?;
        let path = dir.join(".agentos.lock");
        for _ in 0..200 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(BusLock(path)),
                Err(_) => {
                    let stale = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .map(|t| {
                            SystemTime::now().duration_since(t).unwrap_or_default()
                                > Duration::from_secs(15)
                        })
                        .unwrap_or(false);
                    if stale {
                        let _ = fs::remove_file(&path);
                    } else {
                        sleep(Duration::from_millis(25));
                    }
                }
            }
        }
        bail!(
            "Agent Bus is locked by another process ({})",
            path.display()
        )
    }
}

impl Drop for BusLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: String,
    pub state: State,
    pub assigned_to: String,
    pub creator: String,
    pub project: String,
    pub priority: String,
    pub timestamp: String,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<String>,
    pub completed_by: Option<String>,
    pub completed_at: Option<String>,
    pub context_files: Vec<String>,
    pub path: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Board {
    pub pending: Vec<Task>,
    pub in_progress: Vec<Task>,
    pub completed: Vec<Task>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub relative_path: String,
    pub score: usize,
    pub matched_terms: Vec<String>,
    pub matches: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NoteInfo {
    pub relative_path: String,
    pub title: String,
    pub modified: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DelegateArgs {
    pub title: String,
    pub assigned_to: String,
    #[serde(default)]
    pub creator: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub context_files: Vec<String>,
    pub instructions: String,
    #[serde(default = "default_priority")]
    pub priority: String,
}

fn default_priority() -> String {
    "high".into()
}

fn prio_rank(p: &str) -> u8 {
    match p {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 9,
    }
}

#[derive(Debug, Clone)]
pub struct Vault {
    pub root: PathBuf,
}

/// What a memory note is for. Old notes have no type and read as `Fact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemKind {
    Decision,
    Fact,
    Convention,
    Episode,
    Gotcha,
}

impl MemKind {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "decision" => Self::Decision,
            "convention" => Self::Convention,
            "episode" => Self::Episode,
            "gotcha" => Self::Gotcha,
            _ => Self::Fact,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Fact => "fact",
            Self::Convention => "convention",
            Self::Episode => "episode",
            Self::Gotcha => "gotcha",
        }
    }
}

/// Metadata a typed write can carry. All optional except the kind.
#[derive(Debug, Clone, Default)]
pub struct MemMeta {
    pub kind: Option<MemKind>,
    pub supersedes: Option<String>,
    pub source_session: Option<String>,
    pub agent: Option<String>,
    pub repo: Option<String>,
}

impl Vault {
    pub fn new(root: impl Into<PathBuf>) -> Vault {
        Vault { root: root.into() }
    }
    pub fn bus(&self) -> PathBuf {
        self.root.join("02_Agent_Bus")
    }
    pub fn state_dir(&self, s: State) -> PathBuf {
        self.bus().join(s.dir())
    }
    pub fn memory(&self) -> PathBuf {
        self.root.join("03_Memory")
    }
    pub fn projects(&self) -> PathBuf {
        self.root.join("01_Projects")
    }
    pub fn decision_log(&self) -> PathBuf {
        self.memory().join("DECISION_LOG.md")
    }
    pub fn dashboard(&self) -> PathBuf {
        self.root.join("DASHBOARD.md")
    }
    pub fn worker_logs(&self) -> PathBuf {
        self.root.join("04_Agent_Engine").join("worker_logs")
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        for s in State::ALL {
            fs::create_dir_all(self.state_dir(s))?;
        }
        fs::create_dir_all(self.memory())?;
        fs::create_dir_all(self.projects())?;
        Ok(())
    }

    /// Create the full vault tree and seed files. Existing files are never overwritten.
    pub fn scaffold(&self) -> Result<Vec<String>> {
        let mut created = Vec::new();
        for d in [
            "00_Inbox",
            "01_Projects",
            "02_Agent_Bus/Pending",
            "02_Agent_Bus/In_Progress",
            "02_Agent_Bus/Completed",
            "03_Memory",
            "04_Agent_Engine/templates",
        ] {
            let p = self.root.join(d);
            if !p.exists() {
                fs::create_dir_all(&p)?;
                created.push(format!("{d}/"));
            }
        }
        let raw = crate::constitution::default_for(&self.root);
        let seeds: Vec<(&str, String)> = vec![
            ("00_Inbox/README.md", "# 00_Inbox\n\nQuick dumps, raw prompts, and unsorted agent outputs.\n".into()),
            ("04_Agent_Engine/templates/CONSTITUTION.md", raw.clone()),
            ("03_Memory/SYSTEM_CONTEXT.md", format!(
                "---\ntitle: SYSTEM_CONTEXT\ntype: system-context\nversion: 2\ntags: [memory, system, agentos]\n---\n\n# SYSTEM CONTEXT - read this first\n\n> [!important] Every agent reads this note before doing any work. Interface: the `agentos-hub` MCP server.\n\n## Constitution\n\n{}\n\n## Vault map\n- `00_Inbox/` - raw dumps\n- `01_Projects/` - one note per project\n- `02_Agent_Bus/` - `Pending/` -> `In_Progress/` -> `Completed/`\n- `03_Memory/` - this file, [[DECISION_LOG]], topic notes\n- `04_Agent_Engine/` - templates and worker logs\n- [[DASHBOARD]] - live overview\n",
                crate::constitution::managed_block(&raw, crate::constitution::Kind::System))),
            ("03_Memory/DECISION_LOG.md", format!(
                "---\ntitle: DECISION_LOG\ntype: decision-log\ntags: [memory, decisions, agentos]\n---\n\n# Decision Log\n\nChronological record of decisions. Newest entries at the bottom.\n\n### {} - [architecture] Vault initialized by AgentOS Studio\n- **By:** agentos-studio\n- Vault scaffolded as the canonical shared memory and task bus.\n", now())),
            ("DASHBOARD.md", format!(
                "---\ntitle: DASHBOARD\ntype: dashboard\ntags: [dashboard, agentos]\n---\n\n# AgentOS Dashboard\n\n> [!abstract] Local-first Agent Bus + Shared Memory. Read [[03_Memory/SYSTEM_CONTEXT|SYSTEM_CONTEXT]] first.\n\n## Agent Bus\n\n{DASH_START}\n{DASH_END}\n")),
        ];
        for (rel, content) in seeds {
            let p = self.root.join(rel);
            if !p.exists() {
                atomic_write(&p, &content)?;
                created.push(rel.to_string());
            }
        }
        self.refresh_dashboard()?;
        Ok(created)
    }

    pub fn structure_ok(&self) -> (bool, Vec<String>) {
        let missing: Vec<String> = [
            "00_Inbox",
            "01_Projects",
            "02_Agent_Bus/Pending",
            "02_Agent_Bus/In_Progress",
            "02_Agent_Bus/Completed",
            "03_Memory",
            "04_Agent_Engine",
        ]
        .iter()
        .filter(|d| !self.root.join(d).is_dir())
        .map(|d| d.to_string())
        .collect();
        (missing.is_empty(), missing)
    }

    /// The body of a task, wherever it currently lives.
    pub fn task_body(&self, id: &str) -> Option<String> {
        let (path, _) = self.find_task(id, &State::ALL)?;
        fs::read_to_string(path).ok()
    }

    /// Where a task currently sits: pending, in progress or completed.
    pub fn task_state(&self, id: &str) -> Option<State> {
        self.find_task(id, &State::ALL).map(|(_, s)| s)
    }

    fn find_task(&self, id: &str, states: &[State]) -> Option<(PathBuf, State)> {
        for s in states {
            if let Ok(rd) = fs::read_dir(self.state_dir(*s)) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name == format!("{id}.md")
                        || (name.starts_with(&format!("{id}_")) && name.ends_with(".md"))
                    {
                        return Some((e.path(), *s));
                    }
                }
            }
        }
        None
    }

    fn next_id(&self) -> String {
        let re = Regex::new(r"^TASK-(\d+)").expect("static regex");
        let mut hi = 0u64;
        for s in State::ALL {
            if let Ok(rd) = fs::read_dir(self.state_dir(s)) {
                for e in rd.flatten() {
                    if let Some(c) = re.captures(&e.file_name().to_string_lossy()) {
                        hi = hi.max(c[1].parse().unwrap_or(0));
                    }
                }
            }
        }
        format!("TASK-{:03}", hi + 1)
    }

    fn task_from(&self, path: &Path, state: State) -> Result<Task> {
        let doc = Doc::parse(&fs::read_to_string(path)?);
        let opt = |k: &str| doc.get(k).filter(|s| !s.is_empty()).map(str::to_string);
        Ok(Task {
            id: doc.get("id").unwrap_or_default().to_string(),
            title: doc.get("title").unwrap_or_default().to_string(),
            status: doc.get("status").unwrap_or(state.status()).to_string(),
            state,
            assigned_to: doc.get("assigned_to").unwrap_or_default().to_string(),
            creator: doc.get("creator").unwrap_or_default().to_string(),
            project: doc.get("project").unwrap_or_default().to_string(),
            priority: doc.get("priority").unwrap_or_default().to_string(),
            timestamp: doc.get("timestamp").unwrap_or_default().to_string(),
            claimed_by: opt("claimed_by"),
            claimed_at: opt("claimed_at"),
            completed_by: opt("completed_by"),
            completed_at: opt("completed_at"),
            context_files: doc.list("context_files"),
            path: path.display().to_string(),
            body: doc.body.clone(),
        })
    }

    fn tasks_in(&self, s: State) -> Vec<Task> {
        let mut out: Vec<Task> = fs::read_dir(self.state_dir(s))
            .map(|rd| {
                rd.flatten()
                    .filter(|e| {
                        let n = e.file_name().to_string_lossy().to_string();
                        n.starts_with("TASK-") && n.ends_with(".md")
                    })
                    .filter_map(|e| self.task_from(&e.path(), s).ok())
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| {
            prio_rank(&a.priority)
                .cmp(&prio_rank(&b.priority))
                .then(a.id.cmp(&b.id))
        });
        out
    }

    pub fn board(&self) -> Board {
        let mut completed = self.tasks_in(State::Completed);
        completed.sort_by(|a, b| b.completed_at.cmp(&a.completed_at).then(b.id.cmp(&a.id)));
        Board {
            pending: self.tasks_in(State::Pending),
            in_progress: self.tasks_in(State::InProgress),
            completed,
        }
    }

    pub fn get_task(&self, id: &str) -> Result<(Task, String)> {
        let tid = norm_task_id(id)?;
        let (path, s) = self
            .find_task(&tid, &State::ALL)
            .ok_or_else(|| anyhow!("{tid} not found"))?;
        let raw = fs::read_to_string(&path)?;
        Ok((self.task_from(&path, s)?, raw))
    }

    pub fn delegate_task(&self, a: DelegateArgs) -> Result<Value> {
        let assignee = norm_agent(&a.assigned_to)?;
        let prio = a.priority.trim().to_lowercase();
        if !PRIORITIES.contains(&prio.as_str()) {
            bail!(
                "Invalid priority '{}'. Valid: {}",
                a.priority,
                PRIORITIES.join(", ")
            );
        }
        if a.title.trim().is_empty() {
            bail!("title must not be empty");
        }
        if a.instructions.trim().is_empty() {
            bail!("instructions must not be empty");
        }
        self.ensure_dirs()?;
        let _lock = BusLock::acquire(&self.bus())?;
        let id = self.next_id();
        let ts = now();
        let creator = if a.creator.trim().is_empty() {
            "unknown".to_string()
        } else {
            a.creator.trim().to_string()
        };
        let project = if a.project.trim().is_empty() {
            "general".to_string()
        } else {
            a.project.trim().to_string()
        };
        let mut doc = Doc::default();
        doc.set_str("id", &id);
        doc.set_str("title", a.title.trim());
        doc.set_str("status", "pending");
        doc.set_str("assigned_to", &assignee);
        doc.set_str("creator", &creator);
        doc.set_str("project", &project);
        doc.set_str("priority", &prio);
        doc.set_str("timestamp", &ts);
        doc.set("context_files", Fm::List(a.context_files.clone()));
        doc.set(
            "tags",
            Fm::List(vec!["agent-bus".into(), format!("agent/{assignee}")]),
        );
        let ctx = if a.context_files.is_empty() {
            "- _none_".to_string()
        } else {
            a.context_files
                .iter()
                .map(|c| format!("- `{c}`"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        doc.body = format!(
            "# {id}: {}\n\n> [!task] Assigned to **{assignee}** by **{creator}** - priority **{prio}**\n\n## Context files\n{ctx}\n\n## Instructions\n{}\n\n## Activity log\n- {ts} - created by {creator}\n",
            a.title.trim(),
            a.instructions.trim()
        );
        let path = self
            .state_dir(State::Pending)
            .join(format!("{id}_{}.md", slug(&a.title, 60)));
        atomic_write(&path, &doc.render())?;
        drop(_lock);
        self.refresh_dashboard()?;
        Ok(
            json!({"id": id, "status": "pending", "path": path.display().to_string(), "assigned_to": assignee, "priority": prio}),
        )
    }

    pub fn fetch_pending_tasks(&self, agent: &str) -> Result<Vec<Task>> {
        let who = agent.trim().to_lowercase();
        let who = if who == "all" || who.is_empty() {
            "all".to_string()
        } else {
            norm_agent(&who)?
        };
        self.ensure_dirs()?;
        let mut tasks = self.tasks_in(State::Pending);
        tasks.extend(self.tasks_in(State::InProgress));
        if who != "all" {
            tasks.retain(|t| t.assigned_to == who);
        }
        tasks.sort_by(|a, b| {
            prio_rank(&a.priority)
                .cmp(&prio_rank(&b.priority))
                .then(a.id.cmp(&b.id))
        });
        Ok(tasks)
    }

    pub fn claim_task(&self, id: &str, agent: &str) -> Result<String> {
        let tid = norm_task_id(id)?;
        let who = norm_agent(agent)?;
        let _lock = BusLock::acquire(&self.bus())?;
        let (src, _) = match self.find_task(&tid, &[State::Pending]) {
            Some(x) => x,
            None => {
                if self.find_task(&tid, &[State::InProgress]).is_some() {
                    bail!("{tid} is already in progress.");
                }
                if self.find_task(&tid, &[State::Completed]).is_some() {
                    bail!("{tid} is already completed.");
                }
                bail!("{tid} not found in Pending/.");
            }
        };
        let mut doc = Doc::parse(&fs::read_to_string(&src)?);
        let ts = now();
        doc.set_str("status", "in_progress");
        doc.set_str("claimed_by", &who);
        doc.set_str("claimed_at", &ts);
        if doc.get("assigned_to") != Some(who.as_str()) {
            let prev = doc.get("assigned_to").unwrap_or_default().to_string();
            doc.set_str("reassigned_from", prev);
            doc.set_str("assigned_to", &who);
        }
        doc.body = format!("{}\n- {ts} - claimed by {who}\n", doc.body.trim_end());
        let dst = self
            .state_dir(State::InProgress)
            .join(src.file_name().unwrap_or_default());
        atomic_write(&dst, &doc.render())?;
        fs::remove_file(&src)?;
        drop(_lock);
        self.refresh_dashboard()?;
        Ok(format!("{tid} claimed by {who} -> {}", dst.display()))
    }

    pub fn complete_task(&self, id: &str, agent: &str, summary: &str) -> Result<String> {
        let tid = norm_task_id(id)?;
        let who = norm_agent(agent)?;
        if summary.trim().is_empty() {
            bail!("summary_of_work must not be empty");
        }
        let _lock = BusLock::acquire(&self.bus())?;
        let (src, _) = match self.find_task(&tid, &[State::InProgress]) {
            Some(x) => x,
            None => {
                if self.find_task(&tid, &[State::Pending]).is_some() {
                    bail!("{tid} is still pending - call claim_task first.");
                }
                if self.find_task(&tid, &[State::Completed]).is_some() {
                    bail!("{tid} is already completed.");
                }
                bail!("{tid} not found in In_Progress/.");
            }
        };
        let mut doc = Doc::parse(&fs::read_to_string(&src)?);
        let ts = now();
        doc.set_str("status", "completed");
        doc.set_str("completed_by", &who);
        doc.set_str("completed_at", &ts);
        doc.body = format!(
            "{}\n- {ts} - completed by {who}\n\n## Completion summary\n> [!success] Completed by **{who}** at {ts}\n\n{}\n",
            doc.body.trim_end(),
            summary.trim()
        );
        let dst = self
            .state_dir(State::Completed)
            .join(src.file_name().unwrap_or_default());
        atomic_write(&dst, &doc.render())?;
        fs::remove_file(&src)?;
        drop(_lock);
        self.refresh_dashboard()?;
        Ok(format!("{tid} completed by {who} -> {}", dst.display()))
    }

    /// Move a task to any state (used by the Kanban board), recording who moved it.
    pub fn move_task(&self, id: &str, to: State, actor: &str) -> Result<String> {
        let tid = norm_task_id(id)?;
        let (task, _) = self.get_task(&tid)?;
        if task.state == to {
            return Ok(format!("{tid} is already {}", to.status()));
        }
        match (task.state, to) {
            (State::Pending, State::InProgress) => {
                let agent = norm_agent(&task.assigned_to).unwrap_or_else(|_| "human".into());
                return self.claim_task(&tid, &agent);
            }
            (State::InProgress, State::Completed) => {
                let agent = task
                    .claimed_by
                    .clone()
                    .unwrap_or_else(|| task.assigned_to.clone());
                let agent = norm_agent(&agent).unwrap_or_else(|_| "human".into());
                return self.complete_task(
                    &tid,
                    &agent,
                    &format!("Marked complete from AgentOS Studio by {actor}."),
                );
            }
            _ => {}
        }
        let _lock = BusLock::acquire(&self.bus())?;
        let src = PathBuf::from(&task.path);
        let mut doc = Doc::parse(&fs::read_to_string(&src)?);
        let ts = now();
        doc.set_str("status", to.status());
        if to == State::Pending {
            doc.meta.retain(|(k, _)| {
                !matches!(
                    k.as_str(),
                    "claimed_by" | "claimed_at" | "completed_by" | "completed_at"
                )
            });
        }
        if to == State::Completed {
            doc.set_str("completed_by", "human");
            doc.set_str("completed_at", &ts);
        }
        doc.body = format!(
            "{}\n- {ts} - moved to {} by {actor}\n",
            doc.body.trim_end(),
            to.status()
        );
        let dst = self.state_dir(to).join(src.file_name().unwrap_or_default());
        atomic_write(&dst, &doc.render())?;
        fs::remove_file(&src)?;
        drop(_lock);
        self.refresh_dashboard()?;
        Ok(format!("{tid} moved to {}", to.status()))
    }

    /// Append a line to a task's activity log without changing its state.
    pub fn log_activity(&self, id: &str, line: &str) -> Result<()> {
        let tid = norm_task_id(id)?;
        let _lock = BusLock::acquire(&self.bus())?;
        let (path, _) = self
            .find_task(&tid, &State::ALL)
            .ok_or_else(|| anyhow!("{tid} not found"))?;
        let mut doc = Doc::parse(&fs::read_to_string(&path)?);
        doc.body = format!("{}\n- {} - {line}\n", doc.body.trim_end(), now());
        atomic_write(&path, &doc.render())
    }

    fn append_decision(&self, title: &str, detail: &str, author: &str) -> Result<()> {
        let log = self.decision_log();
        if !log.exists() {
            atomic_write(
                &log,
                "# Decision Log\n\nChronological record of decisions.\n",
            )?;
        }
        let mut f = fs::OpenOptions::new().append(true).open(&log)?;
        write!(
            f,
            "\n### {} - {title}\n- **By:** {author}\n- {detail}\n",
            now()
        )?;
        Ok(())
    }

    pub fn write_shared_memory(
        &self,
        topic: &str,
        content: &str,
        category: &str,
    ) -> Result<String> {
        if topic.trim().is_empty() || content.trim().is_empty() {
            bail!("topic and content must not be empty");
        }
        let cat = slug(&category.to_lowercase(), 30).to_lowercase();
        let file_slug = slug(topic, 60);
        if RESERVED_MEMORY.contains(&file_slug.to_lowercase().as_str()) {
            bail!("SYSTEM_CONTEXT and DECISION_LOG are reserved; choose another topic.");
        }
        self.ensure_dirs()?;
        let path = self.memory().join(format!("{file_slug}.md"));
        let ts = now();
        let section = format!("## {ts} ({cat})\n\n{}\n", content.trim());
        let action = if path.exists() {
            let mut doc = Doc::parse(&fs::read_to_string(&path)?);
            doc.set_str("updated", &ts);
            let mut cats = doc.list("categories");
            if !cats.contains(&cat) {
                cats.push(cat.clone());
            }
            cats.sort();
            doc.set("categories", Fm::List(cats));
            doc.body = format!("{}\n\n{section}", doc.body.trim_end());
            atomic_write(&path, &doc.render())?;
            "appended"
        } else {
            let mut doc = Doc::default();
            doc.set_str("topic", topic.trim());
            doc.set("categories", Fm::List(vec![cat.clone()]));
            doc.set_str("created", &ts);
            doc.set_str("updated", &ts);
            doc.set(
                "tags",
                Fm::List(vec!["memory".into(), format!("memory/{cat}")]),
            );
            doc.body = format!(
                "# {}\n\nSee also: [[SYSTEM_CONTEXT]] - [[DECISION_LOG]]\n\n{section}",
                topic.trim()
            );
            atomic_write(&path, &doc.render())?;
            "created"
        };
        let first: String = content
            .trim()
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(160)
            .collect();
        self.append_decision(
            &format!("[{cat}] {}", topic.trim()),
            &format!("Memory {action}: [[{file_slug}]] - {first}"),
            "agentos-hub",
        )?;
        Ok(format!("Memory note {action}: {}", path.display()))
    }

    /// Typed memory write. Only `decision` notes touch DECISION_LOG; everything else stays
    /// in its own note so the log keeps meaning what it says.
    pub fn write_typed_memory(
        &self,
        topic: &str,
        content: &str,
        category: &str,
        meta: &MemMeta,
    ) -> Result<String> {
        if topic.trim().is_empty() || content.trim().is_empty() {
            bail!("topic and content must not be empty");
        }
        let cat = slug(&category.to_lowercase(), 30).to_lowercase();
        let file_slug = slug(topic, 60);
        if RESERVED_MEMORY.contains(&file_slug.to_lowercase().as_str()) {
            bail!("SYSTEM_CONTEXT and DECISION_LOG are reserved; choose another topic.");
        }
        self.ensure_dirs()?;
        let kind = meta.kind.unwrap_or(MemKind::Fact);
        let path = self.memory().join(format!("{file_slug}.md"));
        let ts = now();
        let section = format!("## {ts} ({cat})\n\n{}\n", content.trim());
        let action = if path.exists() {
            let mut doc = Doc::parse(&fs::read_to_string(&path)?);
            doc.set_str("updated", &ts);
            doc.set_str("type", kind.as_str());
            doc.set_str("status", "active");
            let mut cats = doc.list("categories");
            if !cats.contains(&cat) {
                cats.push(cat.clone());
            }
            cats.sort();
            doc.set("categories", Fm::List(cats));
            doc.body = format!("{}\n\n{section}", doc.body.trim_end());
            atomic_write(&path, &doc.render())?;
            "appended"
        } else {
            let mut doc = Doc::default();
            doc.set_str("type", kind.as_str());
            doc.set_str("status", "active");
            doc.set_str("topic", topic.trim());
            doc.set("categories", Fm::List(vec![cat.clone()]));
            doc.set_str("created", &ts);
            doc.set_str("updated", &ts);
            if let Some(s) = meta.supersedes.as_deref().filter(|s| !s.trim().is_empty()) {
                doc.set_str("supersedes", slug(s, 60));
            }
            if let Some(s) = meta
                .source_session
                .as_deref()
                .filter(|s| !s.trim().is_empty())
            {
                doc.set_str("source_session", s.trim());
            }
            if let Some(a) = meta.agent.as_deref().filter(|a| !a.trim().is_empty()) {
                doc.set_str("agent", a.trim());
            }
            if let Some(r) = meta.repo.as_deref().filter(|r| !r.trim().is_empty()) {
                doc.set_str("repo", r.trim());
            }
            doc.set(
                "tags",
                Fm::List(vec!["memory".into(), format!("memory/{cat}")]),
            );
            doc.body = format!(
                "# {}\n\nSee also: [[SYSTEM_CONTEXT]] - [[DECISION_LOG]]\n\n{section}",
                topic.trim()
            );
            atomic_write(&path, &doc.render())?;
            "created"
        };

        if let Some(old) = meta.supersedes.as_deref().filter(|s| !s.trim().is_empty()) {
            let old_slug = slug(old, 60);
            let old_path = self.memory().join(format!("{old_slug}.md"));
            if old_path.exists() {
                let mut doc = Doc::parse(&fs::read_to_string(&old_path)?);
                doc.set_str("status", "superseded");
                doc.set_str("superseded_by", &file_slug);
                doc.set_str("updated", &ts);
                atomic_write(&old_path, &doc.render())?;
            }
        }

        if kind == MemKind::Decision {
            let first: String = content
                .trim()
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(160)
                .collect();
            self.append_decision(
                &format!("[{cat}] {}", topic.trim()),
                &format!("Memory {action}: [[{file_slug}]] - {first}"),
                meta.agent.as_deref().unwrap_or("agentos-hub"),
            )?;
        }

        Ok(format!("Memory note {action}: {}", path.display()))
    }

    /// The `01_Projects` note for a repository, tolerant of hyphens, underscores and case.
    fn resolve_project_note(&self, repo: &str) -> Option<PathBuf> {
        fn norm(s: &str) -> String {
            s.to_lowercase().replace(['-', '_', ' '], "")
        }
        let want = norm(repo);
        let direct = self.projects().join(format!("{}.md", slug(repo, 60)));
        if direct.exists() {
            return Some(direct);
        }
        let entries = fs::read_dir(self.projects()).ok()?;
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if norm(&stem) == want {
                return Some(p);
            }
        }
        None
    }

    /// One page an agent can read instead of every note: purpose, stack, commands, open tasks,
    /// the last few decisions and the known gotchas.
    pub fn project_brief(&self, repo: &str) -> Result<String> {
        let repo = repo.trim();
        if repo.is_empty() {
            bail!("repo must not be empty");
        }
        let slugish = slug(repo, 60);
        let note = self.resolve_project_note(repo);

        fn norm(s: &str) -> String {
            s.to_lowercase().replace(['-', '_', ' '], "")
        }
        let note_label = note
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("{slugish}.md"));
        let mut out = String::new();
        out.push_str(&format!("# Brief: {repo}\n\n"));

        let mut root_dir: Option<PathBuf> = None;
        if let Some(note) = note.as_ref().filter(|p| p.exists()) {
            let raw = fs::read_to_string(note)?;
            let doc = Doc::parse(&raw);
            // Prose, not the frontmatter table or a callout: the first real sentence.
            let purpose = doc
                .body
                .lines()
                .map(str::trim)
                .find(|l| {
                    l.chars().count() > 20
                        && !l.starts_with('#')
                        && !l.starts_with('>')
                        && !l.starts_with('|')
                        && !l.starts_with('-')
                        && !l.starts_with('[')
                        && !l.starts_with("```")
                })
                .unwrap_or("No purpose recorded.")
                .chars()
                .take(400)
                .collect::<String>();
            out.push_str(&format!("## Purpose\n{purpose}\n\n"));
            for key in ["path", "root", "repo_path"] {
                if let Some(p) = doc.get(key) {
                    if Path::new(p).is_dir() {
                        root_dir = Some(PathBuf::from(p));
                    }
                }
            }
        } else {
            out.push_str(&format!(
                "## Purpose\nNo project note at 01_Projects/{note_label} yet.\n\n"
            ));
        }

        // Hermes already records which folders a project owns; use it when the note has no path.
        if root_dir.is_none() {
            if let Ok(list) = crate::projects::list() {
                if let Some(p) = list
                    .iter()
                    .find(|p| norm(&p.slug) == norm(repo) || norm(&p.name) == norm(repo))
                {
                    root_dir = p
                        .primary_path
                        .clone()
                        .or_else(|| p.folders.first().map(|f| f.path.clone()))
                        .map(PathBuf::from);
                }
            }
        }

        let mut stack: Vec<String> = Vec::new();
        if let Some(dir) = root_dir.as_deref() {
            for (file, label) in [
                ("Cargo.toml", "Rust"),
                ("src-tauri/Cargo.toml", "Rust (Tauri backend)"),
                ("package.json", "Node / TypeScript"),
                ("pyproject.toml", "Python"),
                ("go.mod", "Go"),
                ("src-tauri", "Tauri desktop"),
            ] {
                if dir.join(file).exists() {
                    stack.push(label.to_string());
                }
            }
            let readme = dir.join("README.md");
            if let Ok(text) = fs::read_to_string(readme) {
                let mut in_commands = false;
                let mut cmds: Vec<String> = Vec::new();
                for line in text.lines() {
                    let low = line.to_lowercase();
                    if line.starts_with("## ") {
                        in_commands = low.contains("command")
                            || low.contains("development")
                            || low.contains("usage");
                    } else if in_commands && line.trim_start().starts_with("```") {
                        continue;
                    } else if in_commands && !line.trim().is_empty() && cmds.len() < 6 {
                        cmds.push(line.trim().to_string());
                    }
                }
                if !cmds.is_empty() {
                    out.push_str("## Commands\n");
                    for c in cmds {
                        out.push_str(&format!("- `{c}`\n"));
                    }
                    out.push('\n');
                }
            }
        }
        out.push_str(&format!(
            "## Stack\n{}\n\n",
            if stack.is_empty() {
                "Not detected. Add `path:` to the project note.".to_string()
            } else {
                stack.join(", ")
            }
        ));

        let open: Vec<crate::vault::Task> = self
            .fetch_pending_tasks("all")?
            .into_iter()
            .filter(|t| {
                let p = t.project.to_lowercase();
                let r = repo.to_lowercase();
                p == r || p.contains(&r) || r.contains(&p)
            })
            .collect();
        out.push_str("## Open tasks\n");
        if open.is_empty() {
            out.push_str("- None on the bus for this project.\n\n");
        } else {
            for t in open {
                out.push_str(&format!(
                    "- {} [{}] {} ({})\n",
                    t.id, t.status, t.title, t.assigned_to
                ));
            }
            out.push('\n');
        }

        let mut decisions: Vec<String> = Vec::new();
        if let Ok(log) = fs::read_to_string(self.decision_log()) {
            let mut block = String::new();
            for line in log.lines() {
                if line.starts_with("### ") {
                    if !block.is_empty() && block.to_lowercase().contains(&repo.to_lowercase()) {
                        decisions.push(block.trim().to_string());
                    }
                    block = line.to_string();
                } else {
                    block.push('\n');
                    block.push_str(line);
                }
            }
            if !block.is_empty() && block.to_lowercase().contains(&repo.to_lowercase()) {
                decisions.push(block.trim().to_string());
            }
        }
        out.push_str("## Recent decisions\n");
        if decisions.is_empty() {
            out.push_str("- None recorded for this project.\n\n");
        } else {
            for d in decisions.iter().rev().take(5) {
                let head = d
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim_start_matches("### ")
                    .to_string();
                out.push_str(&format!("- {head}\n"));
            }
            out.push('\n');
        }

        out.push_str("## Gotchas\n");
        let mut gotchas = 0usize;
        if let Ok(entries) = fs::read_dir(self.memory()) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("md") {
                    continue;
                }
                let Ok(raw) = fs::read_to_string(&p) else {
                    continue;
                };
                let doc = Doc::parse(&raw);
                if MemKind::parse(doc.get("type").unwrap_or("")) != MemKind::Gotcha {
                    continue;
                }
                let repo_match = doc
                    .get("repo")
                    .map(|r| r.to_lowercase().contains(&repo.to_lowercase()))
                    .unwrap_or(true);
                if !repo_match {
                    continue;
                }
                let first = doc
                    .body
                    .lines()
                    .rev()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with('#'))
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect::<String>();
                let stem = p
                    .file_stem()
                    .map(|x| x.to_string_lossy().to_string())
                    .unwrap_or_default();
                out.push_str(&format!("- [[{stem}]]: {first}\n"));
                gotchas += 1;
                if gotchas >= 5 {
                    break;
                }
            }
        }
        if gotchas == 0 {
            out.push_str("- None recorded.\n");
        }
        Ok(out)
    }

    pub fn search_shared_memory(&self, query: &str) -> Result<Vec<SearchHit>> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if terms.is_empty() {
            bail!("query must not be empty");
        }
        let mut hits = Vec::new();
        for root in [self.projects(), self.memory()] {
            for entry in walkdir::WalkDir::new(&root).into_iter().flatten() {
                let p = entry.path();
                if !entry.file_type().is_file()
                    || p.extension().and_then(|e| e.to_str()) != Some("md")
                {
                    continue;
                }
                let Ok(text) = fs::read_to_string(p) else {
                    continue;
                };
                let mut hit_terms: Vec<String> = Vec::new();
                let mut matches = Vec::new();
                for (i, line) in text.lines().enumerate() {
                    let low = line.to_lowercase();
                    let found: Vec<&String> =
                        terms.iter().filter(|t| low.contains(t.as_str())).collect();
                    if found.is_empty() {
                        continue;
                    }
                    for f in &found {
                        if !hit_terms.contains(f) {
                            hit_terms.push((*f).clone());
                        }
                    }
                    let mut snippet = line.trim().to_string();
                    if snippet.chars().count() > 240 {
                        let idx = low.find(found[0].as_str()).unwrap_or(0);
                        let start = idx.saturating_sub(100);
                        let start = (0..=start)
                            .rev()
                            .find(|i| snippet.is_char_boundary(*i))
                            .unwrap_or(0);
                        let tail: String = snippet[start..].chars().take(240).collect();
                        snippet = format!("{}{tail}...", if start > 0 { "..." } else { "" });
                    }
                    matches.push(json!({"line": i + 1, "snippet": snippet}));
                }
                let stem = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_lowercase();
                for t in &terms {
                    if stem.contains(t.as_str()) && !hit_terms.contains(t) {
                        hit_terms.push(t.clone());
                    }
                }
                if !hit_terms.is_empty() {
                    hit_terms.sort();
                    let rel = p
                        .strip_prefix(&self.root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .replace('\\', "/");
                    hits.push(SearchHit {
                        path: p.display().to_string(),
                        relative_path: rel,
                        score: hit_terms.len() * 100 + matches.len(),
                        matched_terms: hit_terms,
                        matches: matches.into_iter().take(8).collect(),
                    });
                }
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(a.relative_path.cmp(&b.relative_path))
        });
        hits.truncate(25);
        Ok(hits)
    }

    pub fn list_memory_notes(&self) -> Vec<NoteInfo> {
        let mut notes: Vec<NoteInfo> = walkdir::WalkDir::new(self.memory())
            .into_iter()
            .flatten()
            .filter(|e| {
                e.file_type().is_file()
                    && e.path().extension().and_then(|x| x.to_str()) == Some("md")
            })
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                let modified: chrono::DateTime<chrono::Local> = meta.modified().ok()?.into();
                Some(NoteInfo {
                    relative_path: e
                        .path()
                        .strip_prefix(&self.root)
                        .ok()?
                        .to_string_lossy()
                        .replace('\\', "/"),
                    title: e.path().file_stem()?.to_string_lossy().replace('_', " "),
                    modified: modified.format("%Y-%m-%dT%H:%M:%S%:z").to_string(),
                    size: meta.len(),
                })
            })
            .collect();
        notes.sort_by(|a, b| b.modified.cmp(&a.modified));
        notes
    }

    /// Resolve a vault-relative path, rejecting anything that escapes the vault.
    pub fn resolve_rel(&self, rel: &str) -> Result<PathBuf> {
        let p = Path::new(rel);
        if p.is_absolute()
            || p.components()
                .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
        {
            bail!("Path must be relative to the vault: {rel}");
        }
        Ok(self.root.join(p))
    }

    pub fn read_note(&self, rel: &str) -> Result<String> {
        let p = self.resolve_rel(rel)?;
        fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))
    }

    pub fn write_note(&self, rel: &str, content: &str) -> Result<()> {
        let p = self.resolve_rel(rel)?;
        atomic_write(&p, content)
    }

    /// Rewrite the task section of DASHBOARD.md between the AgentOS markers.
    pub fn refresh_dashboard(&self) -> Result<()> {
        let mut rows = Vec::new();
        let mut counts = [0usize; 3];
        for (i, (label, s)) in [
            ("Pending", State::Pending),
            ("In Progress", State::InProgress),
        ]
        .iter()
        .enumerate()
        {
            let tasks = self.tasks_in(*s);
            counts[i] = tasks.len();
            for t in tasks {
                let link = Path::new(&t.path)
                    .strip_prefix(&self.root)
                    .map(|p| p.with_extension("").to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                rows.push(format!(
                    "| [[{link}\\|{}]] | {} | {label} | {} | {} | {} |",
                    t.id, t.title, t.assigned_to, t.priority, t.project
                ));
            }
        }
        let completed = self.board().completed;
        counts[2] = completed.len();
        let mut lines = vec![
            DASH_START.to_string(),
            format!("_Auto-refreshed by AgentOS-Hub at {}_", now()),
            String::new(),
            "| Pending | In Progress | Completed |".into(),
            "|:---:|:---:|:---:|".into(),
            format!("| {} | {} | {} |", counts[0], counts[1], counts[2]),
            String::new(),
            "#### Active tasks".into(),
            String::new(),
            "| ID | Title | Status | Assigned | Priority | Project |".into(),
            "|---|---|---|---|---|---|".into(),
        ];
        if rows.is_empty() {
            lines.push("| - | _No active tasks_ | - | - | - | - |".into());
        } else {
            lines.extend(rows);
        }
        lines.extend(["".into(), "#### Recently completed".into(), "".into()]);
        if completed.is_empty() {
            lines.push("_Nothing completed yet._".into());
        } else {
            lines.push("| ID | Title | Completed by | Completed at |".into());
            lines.push("|---|---|---|---|".into());
            for t in completed.iter().take(10) {
                let link = Path::new(&t.path)
                    .strip_prefix(&self.root)
                    .map(|p| p.with_extension("").to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                lines.push(format!(
                    "| [[{link}\\|{}]] | {} | {} | {} |",
                    t.id,
                    t.title,
                    t.completed_by.clone().unwrap_or_default(),
                    t.completed_at.clone().unwrap_or_default()
                ));
            }
        }
        lines.push(DASH_END.into());
        let block = lines.join("\n");
        let dash = self.dashboard();
        let text = fs::read_to_string(&dash).unwrap_or_else(|_| "# AgentOS Dashboard\n".into());
        let new = match (text.find(DASH_START), text.find(DASH_END)) {
            (Some(s), Some(e)) if e > s => {
                format!("{}{block}{}", &text[..s], &text[e + DASH_END.len()..])
            }
            _ => format!("{}\n\n## Agent Bus\n\n{block}\n", text.trim_end()),
        };
        atomic_write(&dash, &new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::new(dir.path());
        v.scaffold().unwrap();
        (dir, v)
    }

    fn args(title: &str, to: &str) -> DelegateArgs {
        DelegateArgs {
            title: title.into(),
            assigned_to: to.into(),
            creator: "hermes".into(),
            project: "AgentOS".into(),
            context_files: vec!["03_Memory/SYSTEM_CONTEXT.md".into()],
            instructions: "Do the thing.".into(),
            priority: "medium".into(),
        }
    }

    #[test]
    fn full_lifecycle() {
        let (_d, v) = vault();
        let t = v.delegate_task(args("First task", "qoder")).unwrap();
        assert_eq!(t["id"], "TASK-001");
        let t2 = v
            .delegate_task(args("Second: with colon", "hermes"))
            .unwrap();
        assert_eq!(t2["id"], "TASK-002");
        assert_eq!(v.fetch_pending_tasks("qoder").unwrap().len(), 1);
        v.claim_task("task-001", "qoder").unwrap();
        assert!(v
            .claim_task("TASK-001", "qoder")
            .unwrap_err()
            .to_string()
            .contains("already in progress"));
        v.complete_task("TASK-001", "qoder", "Done.").unwrap();
        let b = v.board();
        assert_eq!(
            (b.pending.len(), b.in_progress.len(), b.completed.len()),
            (1, 0, 1)
        );
        assert_eq!(b.pending[0].title, "Second: with colon");
        assert_eq!(b.completed[0].completed_by.as_deref(), Some("qoder"));
        let dash = fs::read_to_string(v.dashboard()).unwrap();
        assert!(dash.contains("| 1 | 0 | 1 |"));
    }

    #[test]
    fn rejects_bad_input() {
        let (_d, v) = vault();
        assert!(v.delegate_task(args("x", "nobody")).is_err());
        let mut a = args("x", "hermes");
        a.priority = "urgent".into();
        assert!(v.delegate_task(a).is_err());
        assert!(v.claim_task("../etc", "hermes").is_err());
        assert!(v.read_note("../outside.md").is_err());
    }

    #[test]
    fn move_task_any_direction() {
        let (_d, v) = vault();
        v.delegate_task(args("Movable", "hermes")).unwrap();
        v.move_task("TASK-001", State::Completed, "human").unwrap();
        assert_eq!(v.board().completed.len(), 1);
        v.move_task("TASK-001", State::Pending, "human").unwrap();
        let b = v.board();
        assert_eq!(b.pending.len(), 1);
        assert!(b.pending[0].completed_by.is_none());
    }

    #[test]
    fn typed_memory_logs_only_decisions() {
        let (_d, v) = vault();
        let fact = MemMeta {
            kind: Some(MemKind::Fact),
            agent: Some("hermes".into()),
            repo: Some("agentos-studio".into()),
            ..Default::default()
        };
        v.write_typed_memory(
            "Port Number",
            "The single instance guard uses 47823.",
            "ops",
            &fact,
        )
        .unwrap();
        let doc = Doc::parse(&v.read_note("03_Memory/Port_Number.md").unwrap());
        assert_eq!(doc.get("type"), Some("fact"));
        assert_eq!(doc.get("status"), Some("active"));
        assert_eq!(doc.get("agent"), Some("hermes"));
        assert_eq!(doc.get("repo"), Some("agentos-studio"));
        let log = v.read_note("03_Memory/DECISION_LOG.md").unwrap();
        assert!(
            !log.contains("Port Number"),
            "a fact must not reach the decision log"
        );

        let decision = MemMeta {
            kind: Some(MemKind::Decision),
            source_session: Some("sess-1".into()),
            ..Default::default()
        };
        v.write_typed_memory(
            "FTS Index",
            "Rank by bm25 plus recency.",
            "architecture",
            &decision,
        )
        .unwrap();
        let log = v.read_note("03_Memory/DECISION_LOG.md").unwrap();
        assert!(log.contains("FTS Index"));
        assert!(log.contains("[[FTS_Index]]"));
    }

    #[test]
    fn superseding_marks_the_old_note_and_untyped_reads_as_fact() {
        let (_d, v) = vault();
        let old = MemMeta {
            kind: Some(MemKind::Fact),
            ..Default::default()
        };
        v.write_typed_memory("Old Guard", "Port 8080.", "ops", &old)
            .unwrap();

        // A note written before types existed carries no type field at all.
        v.write_note(
            "03_Memory/Legacy_Note.md",
            "---\ntitle: Legacy\nstatus: active\n---\n\nbody\n",
        )
        .unwrap();

        let new = MemMeta {
            kind: Some(MemKind::Gotcha),
            supersedes: Some("Old Guard".into()),
            ..Default::default()
        };
        v.write_typed_memory("New Guard", "Port 47823, port 8080 is taken.", "ops", &new)
            .unwrap();

        let old_doc = Doc::parse(&v.read_note("03_Memory/Old_Guard.md").unwrap());
        assert_eq!(old_doc.get("status"), Some("superseded"));
        assert_eq!(old_doc.get("superseded_by"), Some("New_Guard"));

        let legacy = Doc::parse(&v.read_note("03_Memory/Legacy_Note.md").unwrap());
        assert_eq!(legacy.get("type"), None);
        assert_eq!(
            MemKind::parse(legacy.get("type").unwrap_or("")),
            MemKind::Fact
        );
        // Read time only: the file keeps its original bytes apart from nothing.
        assert!(v
            .read_note("03_Memory/Legacy_Note.md")
            .unwrap()
            .contains("status: active"));
    }

    #[test]
    fn memory_kind_parses_and_defaults() {
        assert_eq!(MemKind::parse("decision"), MemKind::Decision);
        assert_eq!(MemKind::parse(" GOTCHA "), MemKind::Gotcha);
        assert_eq!(MemKind::parse("episode"), MemKind::Episode);
        assert_eq!(MemKind::parse("convention"), MemKind::Convention);
        assert_eq!(MemKind::parse(""), MemKind::Fact);
        assert_eq!(MemKind::parse("nonsense"), MemKind::Fact);
        assert_eq!(MemKind::Decision.as_str(), "decision");
    }

    #[test]
    fn memory_write_and_search() {
        let (_d, v) = vault();
        v.write_shared_memory("Zebra Canary", "zebracanary marker", "test")
            .unwrap();
        v.write_shared_memory("Zebra Canary", "second entry", "architecture")
            .unwrap();
        let hits = v.search_shared_memory("zebracanary").unwrap();
        assert!(hits
            .iter()
            .any(|h| h.relative_path == "03_Memory/Zebra_Canary.md"));
        let doc = Doc::parse(&v.read_note("03_Memory/Zebra_Canary.md").unwrap());
        assert_eq!(
            doc.list("categories"),
            vec!["architecture".to_string(), "test".to_string()]
        );
        assert!(v.write_shared_memory("SYSTEM_CONTEXT", "x", "a").is_err());
    }

    #[test]
    fn frontmatter_roundtrip_quotes_ambiguous_values() {
        let mut d = Doc::default();
        d.set_str("timestamp", "2026-11-05T21:52:07+02:00");
        d.set_str("title", "it's: tricky");
        d.set_str("status", "pending");
        d.set_str("flag", "yes");
        d.body = "# Body".into();
        let r = d.render();
        assert!(r.contains("timestamp: '2026-11-05T21:52:07+02:00'"));
        assert!(r.contains("status: pending"));
        assert!(r.contains("flag: 'yes'"));
        let back = Doc::parse(&r);
        assert_eq!(back.get("title"), Some("it's: tricky"));
        assert_eq!(back.body.trim(), "# Body");
    }
}
