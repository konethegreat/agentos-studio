//! Per-action permission policies and the audit log.
//!
//! Every sensitive command calls [`guard`] before doing anything. The policy decides:
//! `deny` refuses, `ask` requires the UI to re-invoke with `approved = true` after the
//! user confirmed in the approval modal, `allow` runs straight away. The check lives in
//! Rust so a UI bug cannot skip it. Every guarded action is written to the audit log.

use anyhow::{anyhow, bail, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::store;

/// Prefix of the error string the UI recognises as "show the approval modal".
pub const APPROVAL_PREFIX: &str = "APPROVAL_REQUIRED";

#[derive(Serialize, Clone, Debug)]
pub struct ActionDef {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub risk: &'static str,
    pub default_policy: &'static str,
    pub reversible: bool,
}

const fn def(
    id: &'static str,
    group: &'static str,
    label: &'static str,
    risk: &'static str,
    default_policy: &'static str,
    reversible: bool,
) -> ActionDef {
    ActionDef {
        id,
        group,
        label,
        risk,
        default_policy,
        reversible,
    }
}

pub const ACTIONS: &[ActionDef] = &[
    def(
        "hermes.journey.delete",
        "Hermes",
        "Delete a Hermes memory or archive a learned skill (hermes journey delete)",
        "high",
        "ask",
        false,
    ),
    def(
        "hermes.computer_use.enable",
        "Hermes",
        "Let Hermes see the screen and control the mouse and keyboard",
        "high",
        "ask",
        false,
    ),
    def(
        "hermes.credential.remove",
        "Hermes",
        "Remove a pooled credential from Hermes (hermes auth remove)",
        "high",
        "ask",
        false,
    ),
    def(
        "hermes.fallback.change",
        "Hermes",
        "Change Hermes' fallback chain (fallback models may be metered)",
        "medium",
        "allow",
        true,
    ),
    def(
        "hermes.cron.change",
        "Hermes",
        "Create, pause, resume, run or remove a Hermes cron job",
        "medium",
        "ask",
        true,
    ),
    def(
        "github.pr.merge",
        "GitHub",
        "Merge a pull request",
        "high",
        "ask",
        false,
    ),
    def(
        "github.pr.review",
        "GitHub",
        "Submit a pull request review",
        "medium",
        "ask",
        false,
    ),
    def(
        "github.pr.create",
        "GitHub",
        "Open a pull request",
        "medium",
        "ask",
        true,
    ),
    def(
        "github.issue.create",
        "GitHub",
        "Open an issue",
        "low",
        "allow",
        true,
    ),
    def(
        "github.issue.comment",
        "GitHub",
        "Comment on an issue or pull request",
        "low",
        "allow",
        false,
    ),
    def(
        "github.issue.close",
        "GitHub",
        "Close an issue",
        "medium",
        "ask",
        true,
    ),
    def(
        "github.run.rerun",
        "GitHub",
        "Re-run a GitHub Actions workflow",
        "low",
        "allow",
        false,
    ),
    def(
        "update.apply",
        "Updates",
        "Install an update",
        "high",
        "ask",
        true,
    ),
    def(
        "update.rollback",
        "Updates",
        "Roll back to a backup",
        "high",
        "ask",
        true,
    ),
    def(
        "org.delete",
        "Workspace",
        "Delete a workspace or folder (chats are kept)",
        "medium",
        "ask",
        false,
    ),
    def(
        "org.link_project",
        "Workspace",
        "Create a Hermes project for a folder",
        "low",
        "allow",
        true,
    ),
    def(
        "session.delete",
        "Chats",
        "Delete a Hermes session permanently",
        "high",
        "ask",
        false,
    ),
    def(
        "hermes.skills.create",
        "Hermes",
        "Create a Hermes skill from a Studio proposal (one Hermes turn)",
        "high",
        "ask",
        false,
    ),
    def(
        "eval.run",
        "Missions",
        "Replay a finished mission on another agent or model (spends agent time)",
        "medium",
        "ask",
        true,
    ),
];

pub fn action(id: &str) -> Option<&'static ActionDef> {
    ACTIONS.iter().find(|a| a.id == id)
}

fn valid_policy(p: &str) -> bool {
    matches!(p, "allow" | "ask" | "deny")
}

pub fn policy_in(c: &Connection, id: &str) -> Result<String> {
    let def = action(id).ok_or_else(|| anyhow!("Unknown action {id}"))?;
    let stored: Option<String> = c
        .query_row(
            "SELECT policy FROM permissions WHERE action = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(stored
        .filter(|p| valid_policy(p))
        .unwrap_or_else(|| def.default_policy.to_string()))
}

pub fn set_policy_in(c: &Connection, id: &str, policy: &str) -> Result<()> {
    if action(id).is_none() {
        bail!("Unknown action {id}");
    }
    if !valid_policy(policy) {
        bail!("Policy must be allow, ask or deny");
    }
    c.execute(
        "INSERT INTO permissions (action, policy) VALUES (?1, ?2) ON CONFLICT(action) DO UPDATE SET policy = excluded.policy",
        [id, policy],
    )?;
    audit_in(
        c,
        "permissions.set",
        id,
        &format!("policy = {policy}"),
        "ok",
    )?;
    Ok(())
}

#[derive(Serialize, Clone, Debug)]
pub struct PolicyView {
    #[serde(flatten)]
    pub def: ActionDef,
    pub policy: String,
}

pub fn list_policies_in(c: &Connection) -> Result<Vec<PolicyView>> {
    ACTIONS
        .iter()
        .map(|d| {
            Ok(PolicyView {
                def: d.clone(),
                policy: policy_in(c, d.id)?,
            })
        })
        .collect()
}

pub fn audit_in(
    c: &Connection,
    action: &str,
    target: &str,
    detail: &str,
    outcome: &str,
) -> Result<()> {
    let detail: String = detail.chars().take(4000).collect();
    c.execute(
        "INSERT INTO audit (at, action, target, detail, outcome) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![store::now(), action, target, detail, outcome],
    )?;
    Ok(())
}

/// Best-effort audit write against the default store; audit failures never block work.
pub fn audit(action: &str, target: &str, detail: &str, outcome: &str) {
    if let Ok(c) = store::open() {
        if let Err(e) = audit_in(&c, action, target, detail, outcome) {
            eprintln!("audit write failed: {e:#}");
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct AuditEntry {
    pub id: i64,
    pub at: i64,
    pub action: String,
    pub target: String,
    pub detail: String,
    pub outcome: String,
}

pub fn list_audit_in(c: &Connection, limit: usize, query: Option<&str>) -> Result<Vec<AuditEntry>> {
    let like = format!("%{}%", query.unwrap_or("").trim());
    let mut stmt = c.prepare(
        "SELECT id, at, action, target, detail, outcome FROM audit \
         WHERE action LIKE ?1 OR target LIKE ?1 OR detail LIKE ?1 \
         ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![like, limit as i64], |r| {
            Ok(AuditEntry {
                id: r.get(0)?,
                at: r.get(1)?,
                action: r.get(2)?,
                target: r.get(3)?,
                detail: r.get(4)?,
                outcome: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Decide whether `action` may run. `summary` is shown in the approval modal.
pub fn guard_in(
    c: &Connection,
    id: &str,
    target: &str,
    summary: &str,
    approved: bool,
) -> Result<()> {
    let policy = policy_in(c, id)?;
    match policy.as_str() {
        "deny" => {
            audit_in(c, id, target, summary, "denied")?;
            bail!(
                "Blocked by your permission policy for \"{}\". Change it in Safety.",
                action(id).map(|a| a.label).unwrap_or(id)
            )
        }
        "ask" if !approved => bail!("{APPROVAL_PREFIX}|{id}|{summary}"),
        _ => Ok(()),
    }
}

pub fn guard(id: &str, target: &str, summary: &str, approved: bool) -> Result<()> {
    let c = store::open()?;
    guard_in(&c, id, target, summary, approved)
}

/// Run a guarded action and audit its outcome.
pub fn run_guarded<T>(
    id: &str,
    target: &str,
    summary: &str,
    approved: bool,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    guard(id, target, summary, approved)?;
    let out = f();
    match &out {
        Ok(_) => audit(id, target, summary, "ok"),
        Err(e) => audit(id, target, &format!("{summary}\n{e:#}"), "error"),
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Scoped tool permissions: the same allow, ask, deny idea as `hermes tools`, but for the
// agentos-hub MCP tools, and per agent and per project on top of the global policy.
//
// Precedence when an agent calls a tool through the hub: repo override, then agent override,
// then the global tool policy (the table Safety already edits), then allow. Overrides live in
// `studio.db` kv under the `perm.` prefix, so no schema change is needed.
// ---------------------------------------------------------------------------------------------

/// The scopes an override can be set on. A repo is the project slug a mission records.
pub const TOOL_SCOPES: [&str; 2] = ["agent", "repo"];

/// The variable a hub session uses to declare which project it works in. Studio sets it for
/// mission workers; an IDE session can add it to its agentos-hub entry to opt in.
pub const REPO_ENV: &str = "AGENTOS_REPO";

const SCOPED_INDEX: &str = "perm.tool.scopes";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ScopeRef {
    pub scope: String,
    pub key: String,
}

/// One stored override, flattened for the UI.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ScopedToolPolicy {
    pub scope: String,
    pub key: String,
    pub server: String,
    pub tool: String,
    pub policy: String,
}

/// Every override, plus the agents and tools the UI needs, in one read.
#[derive(Serialize, Clone, Debug)]
pub struct ToolPolicyModel {
    pub tools: Vec<String>,
    pub agents: Vec<String>,
    pub repos: Vec<String>,
    pub global: Vec<crate::policy::ToolPolicy>,
    pub scoped: Vec<ScopedToolPolicy>,
}

/// Which policy applies to a call, and where it came from.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ToolDecision {
    pub policy: String,
    pub source: String,
}

fn scoped_key(scope: &str, key: &str) -> String {
    format!("perm.tool.{scope}.{key}")
}

fn validate_scope(scope: &str, key: &str) -> Result<()> {
    if !TOOL_SCOPES.contains(&scope) {
        bail!("Scope must be agent or repo");
    }
    if key.trim().is_empty() {
        bail!("A {scope} override needs a name");
    }
    if scope == "agent" && !crate::hub_identity::AGENTS.contains(&key) {
        bail!("Unknown agent '{key}'");
    }
    Ok(())
}

fn read_scopes(c: &Connection) -> Vec<ScopeRef> {
    store::kv_get(c, SCOPED_INDEX)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<ScopeRef>>(&s).ok())
        .unwrap_or_default()
}

fn write_scopes(c: &Connection, refs: &[ScopeRef]) -> Result<()> {
    if refs.is_empty() {
        store::kv_delete(c, SCOPED_INDEX)?;
    } else {
        store::kv_set(c, SCOPED_INDEX, &serde_json::to_string(refs)?)?;
    }
    Ok(())
}

type PolicyMap = BTreeMap<String, BTreeMap<String, String>>;

fn read_map(c: &Connection, scope: &str, key: &str) -> PolicyMap {
    store::kv_get(c, &scoped_key(scope, key))
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<PolicyMap>(&s).ok())
        .unwrap_or_default()
}

fn policy_of(map: &PolicyMap, server: &str, tool: &str) -> Option<String> {
    map.get(server)
        .and_then(|tools| tools.get(tool))
        .filter(|p| valid_policy(p))
        .cloned()
}

pub fn scoped_policy_in(
    c: &Connection,
    scope: &str,
    key: &str,
    server: &str,
    tool: &str,
) -> Option<String> {
    policy_of(&read_map(c, scope, key), server, tool)
}

/// Set one override, or clear it when `policy` is None. Clearing the last override for a scope
/// removes the scope from the index, so the UI never lists an empty tab.
pub fn set_scoped_in(
    c: &Connection,
    scope: &str,
    key: &str,
    server: &str,
    tool: &str,
    policy: Option<&str>,
) -> Result<()> {
    validate_scope(scope, key)?;
    if server.trim().is_empty() || tool.trim().is_empty() {
        bail!("A tool override needs a server and a tool");
    }
    if let Some(p) = policy {
        if !valid_policy(p) {
            bail!("Policy must be allow, ask or deny");
        }
    }
    let key = key.trim();
    let mut map = read_map(c, scope, key);
    match policy {
        Some(p) => {
            map.entry(server.to_string())
                .or_default()
                .insert(tool.to_string(), p.to_string());
        }
        None => {
            if let Some(tools) = map.get_mut(server) {
                tools.remove(tool);
                if tools.is_empty() {
                    map.remove(server);
                }
            }
        }
    }
    if map.is_empty() {
        store::kv_delete(c, &scoped_key(scope, key))?;
    } else {
        store::kv_set(c, &scoped_key(scope, key), &serde_json::to_string(&map)?)?;
    }
    let mut refs = read_scopes(c);
    let present = refs
        .iter()
        .any(|r| r.scope == scope && r.key.eq_ignore_ascii_case(key));
    if map.is_empty() && present {
        refs.retain(|r| !(r.scope == scope && r.key.eq_ignore_ascii_case(key)));
    } else if !map.is_empty() && !present {
        refs.push(ScopeRef {
            scope: scope.to_string(),
            key: key.to_string(),
        });
    }
    write_scopes(c, &refs)?;
    audit_in(
        c,
        "tool_policy.set",
        &format!("{scope}:{key}:{server}/{tool}"),
        &format!("policy = {}", policy.unwrap_or("inherit")),
        "ok",
    )?;
    Ok(())
}

pub fn set_scoped(
    scope: &str,
    key: &str,
    server: &str,
    tool: &str,
    policy: Option<&str>,
) -> Result<()> {
    let c = store::open()?;
    set_scoped_in(&c, scope, key, server, tool, policy)
}

/// Every stored override, in a stable order: repos first (the narrower scope), then agents.
pub fn scoped_list_in(c: &Connection) -> Vec<ScopedToolPolicy> {
    let mut out = Vec::new();
    let mut sorted = read_scopes(c);
    sorted.sort_by(|a, b| {
        (a.scope != "repo")
            .cmp(&(b.scope != "repo"))
            .then_with(|| a.key.cmp(&b.key))
    });
    for r in sorted {
        for (server, tools) in read_map(c, &r.scope, &r.key) {
            for (tool, policy) in tools {
                if valid_policy(&policy) {
                    out.push(ScopedToolPolicy {
                        scope: r.scope.clone(),
                        key: r.key.clone(),
                        server: server.clone(),
                        tool,
                        policy,
                    });
                }
            }
        }
    }
    out
}

pub fn scoped_list() -> Vec<ScopedToolPolicy> {
    match store::open() {
        Ok(c) => scoped_list_in(&c),
        Err(_) => Vec::new(),
    }
}

fn decide_in(
    c: &Connection,
    agent: Option<&str>,
    repo: Option<&str>,
    server: &str,
    tool: &str,
    global: impl FnOnce() -> String,
) -> ToolDecision {
    if let Some(r) = repo.map(str::trim).filter(|r| !r.is_empty()) {
        if let Some(p) = scoped_policy_in(c, "repo", r, server, tool) {
            return ToolDecision {
                policy: p,
                source: format!("repo:{r}"),
            };
        }
    }
    if let Some(a) = agent.map(str::trim).filter(|a| !a.is_empty()) {
        if let Some(p) = scoped_policy_in(c, "agent", a, server, tool) {
            return ToolDecision {
                policy: p,
                source: format!("agent:{a}"),
            };
        }
    }
    ToolDecision {
        policy: global(),
        source: "global".into(),
    }
}

/// The policy that applies to a hub call from `agent` (None for an unknown caller) in `repo`.
pub fn decide_scoped(
    agent: Option<&str>,
    repo: Option<&str>,
    server: &str,
    tool: &str,
) -> ToolDecision {
    match store::open() {
        Ok(c) => decide_in(&c, agent, repo, server, tool, || {
            crate::policy::decide(server, tool)
        }),
        Err(_) => ToolDecision {
            policy: crate::policy::decide(server, tool),
            source: "global".into(),
        },
    }
}

fn source_text(decision: &ToolDecision) -> String {
    match decision.source.split_once(':') {
        Some(("repo", r)) => format!("the policy for {r}"),
        Some(("agent", a)) => format!("the policy for {a}"),
        _ => "the global tool policy".to_string(),
    }
}

/// The gate the hub calls for a verified caller. Same outcomes as the global policy, with the
/// deciding scope in the message so an owner knows where to change it.
pub fn check_scoped(
    agent: Option<&str>,
    repo: Option<&str>,
    server: &str,
    tool: &str,
) -> Result<(), String> {
    let d = decide_scoped(agent, repo, server, tool);
    match d.policy.as_str() {
        "deny" => Err(format!(
            "Tool '{tool}' is blocked by {}. Change it in Safety > Tool permissions if this is intended.",
            source_text(&d)
        )),
        "ask" => Err(format!(
            "Tool '{tool}' requires approval under {}, and this is a headless MCP session with nobody to ask. Set it to allow in Safety > Tool permissions, or run it from a Studio chat.",
            source_text(&d)
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_requires_approval_and_deny_blocks() {
        let c = store::test_conn();
        let e = guard_in(&c, "github.pr.merge", "o/r#1", "merge", false).unwrap_err();
        assert!(e
            .to_string()
            .starts_with("APPROVAL_REQUIRED|github.pr.merge|"));
        guard_in(&c, "github.pr.merge", "o/r#1", "merge", true).unwrap();
        set_policy_in(&c, "github.pr.merge", "deny").unwrap();
        assert!(guard_in(&c, "github.pr.merge", "o/r#1", "merge", true).is_err());
        set_policy_in(&c, "github.pr.merge", "allow").unwrap();
        guard_in(&c, "github.pr.merge", "o/r#1", "merge", false).unwrap();
        let log = list_audit_in(&c, 10, None).unwrap();
        assert!(log.iter().any(|a| a.outcome == "denied"));
        assert!(log.iter().any(|a| a.action == "permissions.set"));
    }

    #[test]
    fn rejects_unknown_actions_and_policies() {
        let c = store::test_conn();
        assert!(set_policy_in(&c, "nope", "allow").is_err());
        assert!(set_policy_in(&c, "update.apply", "sometimes").is_err());
        assert!(guard_in(&c, "nope", "", "", true).is_err());
    }

    #[test]
    fn every_action_has_a_valid_default() {
        for a in ACTIONS {
            assert!(valid_policy(a.default_policy), "{}", a.id);
        }
    }

    #[test]
    fn scoped_overrides_win_in_repo_then_agent_then_global_order() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));

        let base = decide_scoped(
            Some("qoder"),
            Some("agentos-studio"),
            "agentos-hub",
            "recall",
        );
        assert_eq!(
            (base.policy.as_str(), base.source.as_str()),
            ("allow", "global")
        );

        set_scoped("agent", "qoder", "agentos-hub", "recall", Some("deny")).unwrap();
        let agent = decide_scoped(
            Some("qoder"),
            Some("agentos-studio"),
            "agentos-hub",
            "recall",
        );
        assert_eq!(
            (agent.policy.as_str(), agent.source.as_str()),
            ("deny", "agent:qoder")
        );
        let other = decide_scoped(
            Some("hermes"),
            Some("agentos-studio"),
            "agentos-hub",
            "recall",
        );
        assert_eq!(other.policy, "allow", "another agent is unaffected");

        set_scoped(
            "repo",
            "agentos-studio",
            "agentos-hub",
            "recall",
            Some("ask"),
        )
        .unwrap();
        let repo = decide_scoped(
            Some("qoder"),
            Some("agentos-studio"),
            "agentos-hub",
            "recall",
        );
        assert_eq!(
            (repo.policy.as_str(), repo.source.as_str()),
            ("ask", "repo:agentos-studio")
        );
        let no_repo = decide_scoped(Some("qoder"), None, "agentos-hub", "recall");
        assert_eq!(
            no_repo.policy, "deny",
            "without a repo the agent override applies"
        );

        set_scoped("repo", "agentos-studio", "agentos-hub", "recall", None).unwrap();
        set_scoped("agent", "qoder", "agentos-hub", "recall", None).unwrap();
        crate::policy::set("agentos-hub", "recall", "ask").unwrap();
        let global = decide_scoped(
            Some("qoder"),
            Some("agentos-studio"),
            "agentos-hub",
            "recall",
        );
        assert_eq!(
            (global.policy.as_str(), global.source.as_str()),
            ("ask", "global")
        );

        crate::store::set_test_db(None);
    }

    #[test]
    fn scoped_entries_validate_and_list_without_empty_scopes() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));

        assert!(set_scoped("team", "x", "agentos-hub", "recall", Some("deny")).is_err());
        assert!(set_scoped("agent", "nobody", "agentos-hub", "recall", Some("deny")).is_err());
        assert!(set_scoped("agent", "qoder", "agentos-hub", "recall", Some("sometimes")).is_err());
        assert!(set_scoped("repo", "  ", "agentos-hub", "recall", Some("deny")).is_err());

        set_scoped("agent", "qoder", "agentos-hub", "recall", Some("deny")).unwrap();
        set_scoped(
            "repo",
            "agentos-studio",
            "agentos-hub",
            "delegate_task",
            Some("ask"),
        )
        .unwrap();
        set_scoped(
            "repo",
            "agentos-studio",
            "agentos-hub",
            "complete_task",
            Some("allow"),
        )
        .unwrap();
        let listed = scoped_list();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].scope, "repo", "the narrower scope lists first");
        assert_eq!(listed[2].scope, "agent");

        set_scoped(
            "repo",
            "agentos-studio",
            "agentos-hub",
            "delegate_task",
            None,
        )
        .unwrap();
        set_scoped(
            "repo",
            "agentos-studio",
            "agentos-hub",
            "complete_task",
            None,
        )
        .unwrap();
        set_scoped("agent", "qoder", "agentos-hub", "recall", None).unwrap();
        assert!(scoped_list().is_empty(), "clearing removes the scopes");

        let c = store::open().unwrap();
        let log = list_audit_in(&c, 20, Some("tool_policy.set")).unwrap();
        assert!(log.iter().any(|a| a.detail == "policy = inherit"));
        crate::store::set_test_db(None);
    }

    #[test]
    fn the_gate_names_the_scope_that_decided() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));

        set_scoped("agent", "qoder", "agentos-hub", "recall", Some("deny")).unwrap();
        let e = check_scoped(Some("qoder"), None, "agentos-hub", "recall").unwrap_err();
        assert!(e.contains("blocked by the policy for qoder"), "{e}");
        assert!(check_scoped(Some("hermes"), None, "agentos-hub", "recall").is_ok());

        set_scoped(
            "repo",
            "agentos-studio",
            "agentos-hub",
            "delegate_task",
            Some("ask"),
        )
        .unwrap();
        let e = check_scoped(
            Some("qoder"),
            Some("agentos-studio"),
            "agentos-hub",
            "delegate_task",
        )
        .unwrap_err();
        assert!(e.contains("requires approval"), "{e}");
        assert!(e.contains("the policy for agentos-studio"), "{e}");

        set_scoped("agent", "qoder", "agentos-hub", "recall", None).unwrap();
        set_scoped(
            "repo",
            "agentos-studio",
            "agentos-hub",
            "delegate_task",
            None,
        )
        .unwrap();
        assert!(check_scoped(
            Some("qoder"),
            Some("agentos-studio"),
            "agentos-hub",
            "recall"
        )
        .is_ok());
        crate::store::set_test_db(None);
    }
}
