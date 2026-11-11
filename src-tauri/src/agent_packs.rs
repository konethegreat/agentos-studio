//! Agent packs: a tailored agent as an installable capability pack (kind `agent-pack`).
//!
//! A pack carries a persona, the tools it wants, tools it must never get, a model policy, a
//! permission mode, guardrails and optional skills, in `agent.json` next to its `capability.json`.
//! It is installed through the trust pipeline like any other pack, and the host enforces its
//! ceiling: the tools a worker gets are the pack's request intersected with the capabilities the
//! owner granted at install, the scoped tool policy for that agent and repository, and the
//! repository's own allowlist. A pack can only narrow what a worker could already do; it cannot
//! enable a denied tool, and it runs unattended only when it says so and the unattended profile
//! is enforced.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const SPEC: &str = "agent.json";

/// Studio's agent-neutral tool vocabulary, plus `mcp:<server>:<tool>`.
pub const TOOLS: [&str; 9] = [
    "read", "edit", "shell", "web", "browser", "memory", "skills", "todo", "delegate",
];
pub const MODES: [&str; 3] = ["plan", "ask", "edit"];
pub const AGENTS: [&str; 2] = ["claude-code", "hermes"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    /// The persona file, relative to the pack.
    pub persona: String,
    pub tools: Vec<String>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    /// Preference list, tried in order: `repo-routing` (the repository's `.agentos/routing.json`
    /// for the mission's role), `router` (Studio's model router when it is allowed and
    /// confident) and `agent-default` (whatever the agent is configured with). No model ids.
    pub model_policy: Vec<String>,
    /// `plan` (read and propose only), `ask` (no edit is auto-accepted) or `edit`.
    pub permission_mode: String,
    #[serde(default)]
    pub guardrails: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    /// Agents the pack is written for.
    pub agents: Vec<String>,
    #[serde(default)]
    pub unattended: bool,
}

pub const MODEL_ENTRIES: [&str; 3] = ["repo-routing", "router", "agent-default"];

fn valid_tool(t: &str) -> bool {
    TOOLS.contains(&t)
        || t.strip_prefix("mcp:")
            .and_then(|r| r.split_once(':'))
            .is_some_and(|(s, n)| !s.is_empty() && !n.is_empty())
}

/// Parse and validate `agent.json`. Pure.
pub fn parse_spec(text: &str) -> Result<AgentSpec> {
    let s: AgentSpec = serde_json::from_str(text).context(SPEC)?;
    if !crate::marketplace::safe_rel(&s.persona) {
        bail!("persona '{}' must be a file inside the pack", s.persona);
    }
    for t in s.tools.iter().chain(&s.disallowed_tools) {
        if !valid_tool(t) {
            bail!(
                "tool '{t}' is not one of {} or mcp:<server>:<tool>",
                TOOLS.join(", ")
            );
        }
    }
    if s.model_policy.is_empty() {
        bail!("model_policy is empty");
    }
    for m in &s.model_policy {
        if !MODEL_ENTRIES.contains(&m.as_str()) {
            bail!(
                "model_policy entry '{m}' must be one of {}; packs never name a model id",
                MODEL_ENTRIES.join(", ")
            );
        }
    }
    if !MODES.contains(&s.permission_mode.as_str()) {
        bail!("permission_mode must be one of {}", MODES.join(", "));
    }
    if s.agents.is_empty() || s.agents.iter().any(|a| !AGENTS.contains(&a.as_str())) {
        bail!("agents must list one or more of {}", AGENTS.join(", "));
    }
    Ok(s)
}

/// The capability a tool needs from the pack's install grant. Pure.
pub fn tool_capability(tool: &str) -> Option<&'static str> {
    match tool {
        "read" => Some("fs.read"),
        "edit" => Some("fs.write"),
        "shell" => Some("shell.exec"),
        "web" | "browser" => Some("net.hosts"),
        t if t.starts_with("mcp:") => Some("mcp.tools"),
        _ => None,
    }
}

/// The Studio tool a Claude Code allowlist entry belongs to. Pure.
pub fn claude_family(entry: &str) -> Option<String> {
    let base = entry.split('(').next().unwrap_or(entry).trim();
    Some(match base {
        "Read" | "Glob" | "Grep" | "LS" => "read".into(),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => "edit".into(),
        "Bash" | "PowerShell" => "shell".into(),
        "WebFetch" | "WebSearch" => "web".into(),
        "TodoWrite" => "todo".into(),
        "Task" | "Agent" => "delegate".into(),
        b if b.starts_with("mcp__") => {
            let (server, tool) = b.trim_start_matches("mcp__").split_once("__")?;
            format!("mcp:{server}:{tool}")
        }
        _ => return None,
    })
}

/// Claude Code tools denied outright when the pack's ceiling does not include their family.
const CLAUDE_FAMILY_TOOLS: [(&str, &[&str]); 6] = [
    ("read", &["Read", "Glob", "Grep", "LS"]),
    ("edit", &["Edit", "Write", "MultiEdit", "NotebookEdit"]),
    ("shell", &["Bash", "PowerShell"]),
    ("web", &["WebFetch", "WebSearch"]),
    ("todo", &["TodoWrite"]),
    ("delegate", &["Task", "Agent"]),
];

/// Hermes toolsets per Studio tool. Hermes' `file` toolset reads and writes, so it needs both.
fn hermes_toolsets(tools: &BTreeSet<String>) -> Vec<String> {
    let mut out = BTreeSet::new();
    if tools.contains("read") && tools.contains("edit") {
        out.insert("file");
    }
    if tools.contains("shell") {
        out.insert("terminal");
        out.insert("code_execution");
    }
    for (t, ts) in [
        ("web", "web"),
        ("browser", "browser"),
        ("memory", "memory"),
        ("skills", "skills"),
        ("todo", "todo"),
        ("delegate", "delegation"),
    ] {
        if tools.contains(t) {
            out.insert(ts);
        }
    }
    out.into_iter().map(String::from).collect()
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Dropped {
    pub tool: String,
    pub reason: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Ceiling {
    pub agent: String,
    /// Studio tools the worker gets.
    pub tools: Vec<String>,
    pub dropped: Vec<Dropped>,
    /// Claude Code: `--allowedTools`, `--disallowedTools` and `--permission-mode`.
    pub claude_allowed: Vec<String>,
    pub claude_disallowed: Vec<String>,
    pub claude_mode: String,
    /// Hermes: `-t` toolsets.
    pub hermes_toolsets: Vec<String>,
    pub unattended_ok: bool,
    pub unattended_reason: String,
}

/// The host-enforced ceiling for one pack on one agent. `granted` are the capabilities the owner
/// granted at install; `base_allowed` is the Claude Code allowlist the worker would get without a
/// pack; `policy(server, tool)` is the scoped tool policy for this agent and repository;
/// `profile_enforced` says whether the unattended profile's guarantees are all enforced. Pure.
pub fn ceiling(
    spec: &AgentSpec,
    agent: &str,
    granted: &[String],
    base_allowed: &[String],
    policy: &dyn Fn(&str, &str) -> String,
    profile_enforced: bool,
) -> Ceiling {
    let disallowed: BTreeSet<&str> = spec.disallowed_tools.iter().map(String::as_str).collect();
    // A base `mcp__<server>` wildcard survives unless the pack excludes the whole server.
    let server_excluded = |server: &str| {
        disallowed.contains("mcp") || disallowed.contains(format!("mcp:{server}").as_str())
    };
    let kept_server = |server: &str| {
        !server_excluded(server) && base_allowed.iter().any(|e| e == &format!("mcp__{server}"))
    };
    let mut tools = BTreeSet::new();
    let mut dropped = Vec::new();
    for t in &spec.tools {
        let mcp = t.strip_prefix("mcp:").and_then(|r| r.split_once(':'));
        let why = if disallowed.contains(t.as_str()) {
            Some("the pack disallows it".to_string())
        } else if let Some(cap) = tool_capability(t).filter(|c| !granted.iter().any(|g| g == c)) {
            Some(format!("{cap} was not granted when the pack was installed"))
        } else if let Some((s, n)) = mcp {
            if policy(s, n) == "deny" {
                Some("your tool policy denies it".to_string())
            } else if agent == "claude-code"
                && !kept_server(s)
                && !base_allowed.iter().any(|e| e == &format!("mcp__{s}__{n}"))
            {
                // A pack never adds an MCP rule the repository allowlist does not already have.
                Some(format!(
                    "the repository allowlist does not cover the {s} server"
                ))
            } else {
                None
            }
        } else {
            None
        };
        match why {
            Some(reason) => dropped.push(Dropped {
                tool: t.clone(),
                reason,
            }),
            None => {
                tools.insert(t.clone());
            }
        }
    }
    // Claude Code: keep only allowlist entries whose family survived, plus base server
    // wildcards the pack does not exclude (the hub stays reachable); deny the rest outright.
    let mut claude_allowed = Vec::new();
    for e in base_allowed {
        if let Some(server) = e
            .strip_prefix("mcp__")
            .filter(|r| !r.contains("__") && !r.is_empty())
        {
            if !server_excluded(server) {
                claude_allowed.push(e.clone());
            }
        } else if let Some(f) = claude_family(e) {
            if tools.contains(&f) {
                claude_allowed.push(e.clone());
            }
        }
    }
    let mut claude_disallowed = Vec::new();
    for (fam, names) in CLAUDE_FAMILY_TOOLS {
        if !tools.contains(fam) {
            claude_disallowed.extend(names.iter().map(|s| s.to_string()));
        }
    }
    // A kept server wildcard must not readmit a tool the pack or the policy dropped.
    for d in &dropped {
        if let Some((s, n)) = d.tool.strip_prefix("mcp:").and_then(|r| r.split_once(':')) {
            let e = format!("mcp__{s}__{n}");
            if !claude_disallowed.contains(&e) {
                claude_disallowed.push(e);
            }
        }
    }
    for server in disallowed
        .iter()
        .filter_map(|d| d.strip_prefix("mcp:"))
        .filter(|r| !r.contains(':'))
    {
        claude_disallowed.push(format!("mcp__{server}"));
    }
    let edit = tools.contains("edit");
    let claude_mode = match spec.permission_mode.as_str() {
        "plan" => "plan",
        "edit" if edit => "acceptEdits",
        _ => "default",
    }
    .to_string();
    let (unattended_ok, unattended_reason) = if !spec.unattended {
        (
            false,
            "the pack is not written to run unattended".to_string(),
        )
    } else if !profile_enforced {
        (
            false,
            "the unattended profile is not fully enforced".to_string(),
        )
    } else {
        (
            true,
            "allowed: the pack opts in and the unattended profile is enforced".to_string(),
        )
    };
    Ceiling {
        agent: agent.to_string(),
        hermes_toolsets: hermes_toolsets(&tools),
        tools: tools.into_iter().collect(),
        dropped,
        claude_allowed,
        claude_disallowed,
        claude_mode,
        unattended_ok,
        unattended_reason,
    }
}

/// The first model policy entry that resolves, and what it resolved to. `agent-default` always
/// resolves, to no pin. Pure over `resolve`.
pub fn resolve_model(
    policy: &[String],
    resolve: &dyn Fn(&str) -> Option<(String, String)>,
) -> (String, String, String) {
    for entry in policy {
        if entry == "agent-default" {
            return (entry.clone(), String::new(), String::new());
        }
        if let Some((provider, model)) = resolve(entry) {
            return (entry.clone(), provider, model);
        }
    }
    ("agent-default".into(), String::new(), String::new())
}

/// Every guarantee of the unattended profile is enforced.
pub fn profile_enforced() -> bool {
    crate::permissions::unattended_profile()
        .guarantees
        .iter()
        .filter(|g| matches!(g.id.as_str(), "worktree" | "environ" | "inside"))
        .all(|g| g.status == "enforced")
}

#[derive(Serialize, Clone, Debug)]
pub struct InstalledAgent {
    pub entry: crate::marketplace::LockEntry,
    pub spec: AgentSpec,
    pub persona: String,
    pub dir: String,
    /// The ceiling on each agent the pack supports, with no repository scope.
    pub ceilings: Vec<Ceiling>,
    pub status: String,
}

fn load_from(
    entry: &crate::marketplace::LockEntry,
    store: &crate::marketplace::Store,
) -> Result<(AgentSpec, String, PathBuf)> {
    crate::marketplace::verify(&entry.id, store)?;
    let dir = store.pack_dir(&entry.kind, &entry.id).join(&entry.hash);
    let spec = parse_spec(
        &fs::read_to_string(dir.join(SPEC))
            .with_context(|| format!("{} has no {SPEC}", entry.id))?,
    )?;
    let persona: String = fs::read_to_string(dir.join(&spec.persona))
        .with_context(|| format!("persona {}", spec.persona))?
        .chars()
        .take(crate::marketplace::PROSE_CAP_BYTES)
        .collect();
    Ok((spec, persona, dir))
}

/// Load an installed, verified agent pack.
pub fn load(id: &str) -> Result<(crate::marketplace::LockEntry, AgentSpec, String)> {
    let store = crate::marketplace::Store::live();
    let lock = crate::marketplace::read_lock(&store)?;
    let e = lock
        .packs
        .get(id)
        .cloned()
        .ok_or_else(|| anyhow!("agent pack {id} is not installed"))?;
    if e.kind != "agent-pack" {
        bail!("{id} is a {}, not an agent pack", e.kind);
    }
    let (spec, persona, _) = load_from(&e, &store)?;
    Ok((e, spec, persona))
}

pub fn installed() -> Result<Vec<InstalledAgent>> {
    let store = crate::marketplace::Store::live();
    let lock = crate::marketplace::read_lock(&store)?;
    let base = crate::routing::claude_allowed_tools(&[]);
    let enforced = profile_enforced();
    let policy = |s: &str, t: &str| crate::permissions::decide_scoped(None, None, s, t).policy;
    Ok(lock
        .packs
        .values()
        .filter(|e| e.kind == "agent-pack")
        .map(|e| match load_from(e, &store) {
            Ok((spec, persona, dir)) => InstalledAgent {
                ceilings: spec
                    .agents
                    .iter()
                    .map(|a| ceiling(&spec, a, &e.granted, &base, &policy, enforced))
                    .collect(),
                entry: e.clone(),
                spec,
                persona,
                dir: dir.display().to_string(),
                status: "ok".into(),
            },
            Err(err) => InstalledAgent {
                entry: e.clone(),
                spec: AgentSpec {
                    persona: String::new(),
                    tools: vec![],
                    disallowed_tools: vec![],
                    model_policy: vec!["agent-default".into()],
                    permission_mode: "plan".into(),
                    guardrails: vec![],
                    skills: vec![],
                    agents: vec![],
                    unattended: false,
                },
                persona: String::new(),
                dir: String::new(),
                ceilings: vec![],
                status: err.to_string(),
            },
        })
        .collect())
}

/// Resolve a new mission's model through the pack's policy, in the pack's order. Returns the
/// entry that decided and the provider and model to pin (both empty for `agent-default`).
pub fn resolve_for_mission(
    spec: &AgentSpec,
    repo: &str,
    role: &str,
    agent: &str,
) -> (String, String, String) {
    let resolve = |entry: &str| -> Option<(String, String)> {
        match entry {
            "repo-routing" => crate::review_agent::project_dir(repo)
                .and_then(|d| crate::routing::defaults_for(&d, role))
                .filter(|c| !c.provider.trim().is_empty())
                .map(|c| (c.provider, c.model)),
            "router" if agent == "hermes" => {
                crate::router::allowed_pick(role).map(|s| (s.provider, s.model))
            }
            _ => None,
        }
    };
    resolve_model(&spec.model_policy, &resolve)
}

/// The kv key holding a mission's agent pack.
pub fn mission_key(mission_id: &str) -> String {
    format!("mission.pack.{mission_id}")
}

pub fn set_for_mission(mission_id: &str, pack: &str) -> Result<()> {
    let c = crate::store::open()?;
    crate::store::kv_set(&c, &mission_key(mission_id), pack)
}

pub fn for_mission(mission_id: &str) -> Option<String> {
    let c = crate::store::open().ok()?;
    crate::store::kv_get(&c, &mission_key(mission_id))
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
}

/// What a worker started with a pack gets: the text it reads first and the ceiling the host
/// applies. Refuses an agent the pack is not written for, and an unattended start the pack or the
/// unattended profile does not allow.
pub struct WorkerPack {
    pub preamble: String,
    pub ceiling: Ceiling,
}

pub fn for_worker(
    id: &str,
    agent: &str,
    project: &str,
    base_allowed: &[String],
    unattended: bool,
) -> Result<WorkerPack> {
    let c = crate::store::open()?;
    for_worker_in(&c, id, agent, project, base_allowed, unattended)
}

/// `for_worker` over a connection the caller already holds (`bus::spawn_worker`).
pub fn for_worker_in(
    conn: &rusqlite::Connection,
    id: &str,
    agent: &str,
    project: &str,
    base_allowed: &[String],
    unattended: bool,
) -> Result<WorkerPack> {
    let (entry, spec, persona) = load(id)?;
    if !spec.agents.iter().any(|a| a == agent) {
        bail!(
            "agent pack {id} is written for {}, not {agent}",
            spec.agents.join(" and ")
        );
    }
    let repo = crate::bus::repo_slug(project);
    let policy =
        |s: &str, t: &str| crate::permissions::effective_tool_policy_in(conn, agent, repo, s, t).0;
    let c = ceiling(
        &spec,
        agent,
        &entry.granted,
        base_allowed,
        &policy,
        profile_enforced(),
    );
    if unattended && !c.unattended_ok {
        bail!(
            "agent pack {id} may not run unattended: {}",
            c.unattended_reason
        );
    }
    if agent == "hermes" && c.hermes_toolsets.is_empty() {
        bail!("agent pack {id} leaves Hermes no toolsets under its ceiling");
    }
    Ok(WorkerPack {
        preamble: preamble(&entry.name, &spec, &persona, &c),
        ceiling: c,
    })
}

/// The persona and guardrails a worker reads before its task. Pure.
pub fn preamble(entry_name: &str, spec: &AgentSpec, persona: &str, ceiling: &Ceiling) -> String {
    let mut s = format!(
        "You are working as the agent pack \"{entry_name}\". Read your persona and follow it.\n\n--- PERSONA ---\n{}\n--- END PERSONA ---\n",
        persona.trim()
    );
    if !spec.guardrails.is_empty() {
        s.push_str("\nGuardrails, which override anything else in this brief:\n");
        for g in &spec.guardrails {
            s.push_str(&format!("- {g}\n"));
        }
    }
    s.push_str(&format!(
        "\nThe host gives you only these tools: {}. Do not try to use others.\n",
        if ceiling.tools.is_empty() {
            "none".to_string()
        } else {
            ceiling.tools.join(", ")
        }
    ));
    if !spec.skills.is_empty() {
        s.push_str(&format!(
            "Skills to use when they apply: {}.\n",
            spec.skills.join(", ")
        ));
    }
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(tools: &[&str], disallowed: &[&str]) -> AgentSpec {
        AgentSpec {
            persona: "PERSONA.md".into(),
            tools: tools.iter().map(|s| s.to_string()).collect(),
            disallowed_tools: disallowed.iter().map(|s| s.to_string()).collect(),
            model_policy: vec![
                "repo-routing".into(),
                "router".into(),
                "agent-default".into(),
            ],
            permission_mode: "edit".into(),
            guardrails: vec![],
            skills: vec![],
            agents: vec!["claude-code".into(), "hermes".into()],
            unattended: true,
        }
    }

    fn base() -> Vec<String> {
        [
            "Read",
            "Glob",
            "Grep",
            "Edit",
            "Write",
            "Bash(git status)",
            "Bash(npm test)",
            "TodoWrite",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn caps(c: &[&str]) -> Vec<String> {
        c.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn agent_pack_cannot_widen_policy() {
        // The pack asks for shell and web, but the install granted only fs.read and fs.write.
        let s = spec(&["read", "edit", "shell", "web"], &[]);
        let c = ceiling(
            &s,
            "claude-code",
            &caps(&["fs.read", "fs.write"]),
            &base(),
            &|_, _| "allow".into(),
            true,
        );
        assert_eq!(c.tools, vec!["edit".to_string(), "read".to_string()]);
        assert!(c
            .dropped
            .iter()
            .any(|d| d.tool == "shell" && d.reason.contains("shell.exec")));
        assert!(!c.claude_allowed.iter().any(|a| a.starts_with("Bash")));
        assert!(c.claude_disallowed.contains(&"Bash".to_string()));
        assert!(c.claude_disallowed.contains(&"WebFetch".to_string()));
        // The base allowlist never grows: nothing outside it appears.
        assert!(c.claude_allowed.iter().all(|a| base().contains(a)));
        // With every capability granted, the repo allowlist still bounds shell to its entries.
        let all = ceiling(
            &s,
            "claude-code",
            &caps(&["fs.read", "fs.write", "shell.exec", "net.hosts"]),
            &base(),
            &|_, _| "allow".into(),
            true,
        );
        let bash: Vec<&String> = all
            .claude_allowed
            .iter()
            .filter(|a| a.starts_with("Bash"))
            .collect();
        assert_eq!(bash.len(), 2);
        // Unattended needs the pack to opt in and the profile to be enforced.
        assert!(!ceiling(&s, "hermes", &[], &base(), &|_, _| "allow".into(), false).unattended_ok);
        let mut no = s.clone();
        no.unattended = false;
        assert!(!ceiling(&no, "hermes", &[], &base(), &|_, _| "allow".into(), true).unattended_ok);
    }

    #[test]
    fn agent_pack_denied_tool_absent_from_list() {
        let s = spec(
            &[
                "read",
                "mcp:github:merge_pull_request",
                "mcp:github:list_issues",
                "todo",
            ],
            &["todo"],
        );
        let policy = |srv: &str, t: &str| {
            if srv == "github" && t == "merge_pull_request" {
                "deny".to_string()
            } else {
                "ask".to_string()
            }
        };
        let mut with_github = base();
        with_github.push("mcp__github".into());
        let c = ceiling(
            &s,
            "claude-code",
            &caps(&["fs.read", "mcp.tools"]),
            &with_github,
            &policy,
            true,
        );
        assert!(!c
            .tools
            .contains(&"mcp:github:merge_pull_request".to_string()));
        assert!(!c
            .claude_allowed
            .contains(&"mcp__github__merge_pull_request".to_string()));
        assert!(
            c.claude_disallowed
                .contains(&"mcp__github__merge_pull_request".to_string()),
            "the kept server wildcard must not readmit a denied tool"
        );
        assert!(c.tools.contains(&"mcp:github:list_issues".to_string()));
        assert!(c.claude_allowed.contains(&"mcp__github".to_string()));
        assert!(c.claude_allowed.iter().all(|a| with_github.contains(a)));
        assert!(
            !c.tools.contains(&"todo".to_string()),
            "a disallowed tool stays out"
        );
        assert!(c.claude_disallowed.contains(&"TodoWrite".to_string()));
        assert!(
            c.dropped
                .iter()
                .any(|d| d.tool == "mcp:github:merge_pull_request"
                    && d.reason.contains("tool policy"))
        );
        // Hermes cannot split read from write, so read alone gives no file toolset.
        let h = ceiling(
            &s,
            "hermes",
            &caps(&["fs.read", "mcp.tools"]),
            &[],
            &policy,
            true,
        );
        assert!(!h.hermes_toolsets.contains(&"file".to_string()));
        assert_eq!(c.claude_mode, "default", "edit mode needs the edit tool");
    }

    #[test]
    fn pack_workers_keep_the_hub_and_add_no_uncovered_mcp_rules() {
        let mut b = base();
        b.push("mcp__agentos-hub".into());
        let s = spec(&["read", "mcp:slack:post_message"], &[]);
        let c = ceiling(
            &s,
            "claude-code",
            &caps(&["fs.read", "mcp.tools"]),
            &b,
            &|_, _| "allow".into(),
            true,
        );
        assert!(c.claude_allowed.contains(&"mcp__agentos-hub".to_string()));
        assert!(!c.claude_allowed.iter().any(|a| a.contains("slack")));
        assert!(c
            .dropped
            .iter()
            .any(|d| d.tool == "mcp:slack:post_message" && d.reason.contains("allowlist")));
        assert!(c.claude_allowed.iter().all(|a| b.contains(a)));
        // A pack that excludes the hub server loses the wildcard and denies it outright.
        let no_hub = spec(&["read"], &["mcp:agentos-hub"]);
        let c = ceiling(
            &no_hub,
            "claude-code",
            &caps(&["fs.read"]),
            &b,
            &|_, _| "allow".into(),
            true,
        );
        assert!(!c.claude_allowed.contains(&"mcp__agentos-hub".to_string()));
        assert!(c
            .claude_disallowed
            .contains(&"mcp__agentos-hub".to_string()));
    }

    #[test]
    fn agent_pack_model_falls_back_in_order() {
        let policy: Vec<String> = ["repo-routing", "router", "agent-default"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let none = |_: &str| None;
        assert_eq!(
            resolve_model(&policy, &none),
            ("agent-default".into(), String::new(), String::new())
        );
        let router_only = |e: &str| {
            (e == "router").then(|| {
                (
                    "claude-subscription".to_string(),
                    "routed-model".to_string(),
                )
            })
        };
        assert_eq!(resolve_model(&policy, &router_only).0, "router");
        let both = |e: &str| Some((format!("p-{e}"), format!("m-{e}")));
        assert_eq!(
            resolve_model(&policy, &both),
            (
                "repo-routing".into(),
                "p-repo-routing".into(),
                "m-repo-routing".into()
            )
        );
        let only_default: Vec<String> = vec!["agent-default".into(), "router".into()];
        assert_eq!(
            resolve_model(&only_default, &both).0,
            "agent-default",
            "order is the pack's"
        );
    }

    #[test]
    fn specs_are_strict() {
        let ok = serde_json::to_string(&spec(&["read"], &[])).unwrap();
        assert!(parse_spec(&ok).is_ok());
        let mut v: serde_json::Value = serde_json::from_str(&ok).unwrap();
        v["model_policy"] = serde_json::json!(["claude-opus-5-5"]);
        assert!(parse_spec(&v.to_string())
            .unwrap_err()
            .to_string()
            .contains("never name a model id"));
        let mut w: serde_json::Value = serde_json::from_str(&ok).unwrap();
        w["sudo"] = serde_json::json!(true);
        assert!(parse_spec(&w.to_string()).is_err());
        let mut x: serde_json::Value = serde_json::from_str(&ok).unwrap();
        x["tools"] = serde_json::json!(["everything"]);
        assert!(parse_spec(&x.to_string()).is_err());
        assert_eq!(
            claude_family("mcp__agentos-hub__recall").as_deref(),
            Some("mcp:agentos-hub:recall")
        );
        assert_eq!(claude_family("Bash(git *)").as_deref(), Some("shell"));
    }

    #[test]
    fn the_five_example_packs_parse_and_verify() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("examples")
            .join("capabilities")
            .join("agents");
        let names = [
            "planner",
            "code-reviewer",
            "security-auditor",
            "docs-writer",
            "researcher",
        ];
        for n in names {
            let dir = root.join(n);
            let s = parse_spec(&fs::read_to_string(dir.join(SPEC)).unwrap())
                .unwrap_or_else(|e| panic!("{n}: {e:#}"));
            let persona = fs::read_to_string(dir.join(&s.persona)).unwrap();
            for section in ["## Inputs", "## Output", "## Refuse when"] {
                assert!(persona.contains(section), "{n} persona lacks {section}");
            }
            let t = tempfile::tempdir().unwrap();
            let store = crate::marketplace::Store {
                root: t.path().join("c"),
                lock: t.path().join("l.json"),
            };
            let policy = |_: &str, _: &str| "ask".to_string();
            let audit = |_: &str, _: &str, _: &str, _: &str| {};
            let ctx = crate::marketplace::Ctx {
                trusted_keys: Default::default(),
                owner_ceiling: Default::default(),
                tool_policy: &policy,
                audit: &audit,
            };
            let p = crate::marketplace::propose(&dir.display().to_string(), None, &store, &ctx)
                .unwrap();
            assert_eq!(p.tier, "official", "{n}: {:?}", p.refusal);
            assert!(!p.blocked, "{n}: {:?} {:?}", p.refusal, p.findings);
            assert_eq!(p.manifest.kind, "agent-pack");
        }
    }
}
