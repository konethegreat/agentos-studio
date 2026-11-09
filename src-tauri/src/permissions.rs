//! Per-action permission policies and the audit log.
//!
//! Every sensitive command calls [`guard`] before doing anything. The policy decides:
//! `deny` refuses, `ask` requires the UI to re-invoke with `approved = true` after the
//! user confirmed in the approval modal, `allow` runs straight away. The check lives in
//! Rust so a UI bug cannot skip it. Every guarded action is written to the audit log.

use anyhow::{anyhow, bail, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

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
}
