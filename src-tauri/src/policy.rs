//! Per-tool permissions for the MCP surface.
//!
//! The existing permission model gates Studio's own actions. This one gates the tools agents call
//! through `agentos-hub`, and it is enforced inside the MCP server, so it cannot be bypassed by a
//! chat that decides to call a tool directly.
//!
//! Policy per (server, tool):
//! - `allow`: runs, still audited by the caller.
//! - `ask`: refused over stdio, because there is no human attached to a stdio session. It is an
//!   honest refusal rather than a silent prompt nobody sees.
//! - `deny`: refused.
//!
//! Default is `allow` for the hub's own tools, so an upgrade never breaks a working fleet. Anything
//! destructive is opted into `ask` or `deny` explicitly.

use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::store;

pub const POLICIES: [&str; 3] = ["allow", "ask", "deny"];

#[derive(Debug, Clone, Serialize)]
pub struct ToolPolicy {
    pub server: String,
    pub tool: String,
    pub policy: String,
}

pub fn decide(server: &str, tool: &str) -> String {
    match store::open() {
        Ok(c) => decide_in(&c, server, tool),
        Err(_) => "allow".into(),
    }
}

fn stored_in(c: &Connection, server: &str, tool: &str) -> Option<String> {
    c.query_row(
        "SELECT policy FROM tool_policy WHERE server = ?1 AND tool = ?2",
        params![server, tool],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .filter(|p| POLICIES.contains(&p.as_str()))
}

/// The policy for one tool: its own row, then the server's `*` row (an MCP bundle registers
/// `mcp.<server>.* = ask`, so tools it adds later still ask), then `allow`.
pub fn decide_in(c: &Connection, server: &str, tool: &str) -> String {
    stored_in(c, server, tool)
        .or_else(|| (tool != "*").then(|| stored_in(c, server, "*")).flatten())
        .unwrap_or_else(|| "allow".into())
}

fn stored_checked_in(c: &Connection, server: &str, tool: &str) -> Result<Option<String>> {
    let policy: Option<String> = c
        .query_row(
            "SELECT policy FROM tool_policy WHERE server = ?1 AND tool = ?2",
            params![server, tool],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(p) = &policy {
        ensure!(POLICIES.contains(&p.as_str()), "invalid stored tool policy");
    }
    Ok(policy)
}

/// Apply exact and server-wide policies, defaulting only when neither is stored.
pub fn decide_checked_in(c: &Connection, server: &str, tool: &str) -> Result<String> {
    if let Some(policy) = stored_checked_in(c, server, tool)? {
        return Ok(policy);
    }
    if tool != "*" {
        if let Some(policy) = stored_checked_in(c, server, "*")? {
            return Ok(policy);
        }
    }
    Ok("allow".into())
}

pub fn set(server: &str, tool: &str, policy: &str) -> Result<()> {
    set_in(&store::open()?, server, tool, policy)
}

pub fn set_in(c: &Connection, server: &str, tool: &str, policy: &str) -> Result<()> {
    if !POLICIES.contains(&policy) {
        anyhow::bail!("policy must be allow, ask or deny");
    }
    c.execute(
        "INSERT INTO tool_policy (server, tool, policy) VALUES (?1, ?2, ?3)
         ON CONFLICT(server, tool) DO UPDATE SET policy = excluded.policy",
        params![server, tool, policy],
    )?;
    Ok(())
}

/// Write a default: `deny` always applies (it only narrows), anything else only when the owner
/// has no row for the tool yet, so registering a bundle again never widens or resets a choice.
pub fn set_default_in(c: &Connection, server: &str, tool: &str, policy: &str) -> Result<()> {
    if policy == "deny" || stored_in(c, server, tool).is_none() {
        set_in(c, server, tool, policy)?;
    }
    Ok(())
}

pub fn list() -> Vec<ToolPolicy> {
    let Ok(c) = store::open() else {
        return Vec::new();
    };
    let Ok(mut st) = c.prepare("SELECT server, tool, policy FROM tool_policy ORDER BY tool") else {
        return Vec::new();
    };
    let rows = st.query_map([], |r| {
        Ok(ToolPolicy {
            server: r.get(0)?,
            tool: r.get(1)?,
            policy: r.get(2)?,
        })
    });
    rows.map(|r| r.flatten().collect()).unwrap_or_default()
}

/// The gate the MCP server calls before running anything.
pub fn check(server: &str, tool: &str) -> Result<(), String> {
    match decide(server, tool).as_str() {
        "deny" => Err(format!(
            "Tool '{tool}' is blocked by the AgentOS tool policy. Change it in Safety > Tool permissions if this is intended."
        )),
        "ask" => Err(format!(
            "Tool '{tool}' requires approval, and this is a headless MCP session with nobody to ask. Run it from a Studio chat, or set the policy to allow in Safety > Tool permissions."
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_policy_prefers_exact_then_wildcard_then_absent_allow() {
        let c = store::test_conn();
        assert_eq!(decide_checked_in(&c, "hub", "read").unwrap(), "allow");
        set_in(&c, "hub", "*", "deny").unwrap();
        set_in(&c, "hub", "read", "ask").unwrap();
        assert_eq!(decide_checked_in(&c, "hub", "read").unwrap(), "ask");
        assert_eq!(decide_checked_in(&c, "hub", "other").unwrap(), "deny");
        assert_eq!(decide_checked_in(&c, "hub", "*").unwrap(), "deny");
        assert_eq!(decide_checked_in(&c, "other", "read").unwrap(), "allow");
    }

    #[test]
    fn checked_policy_never_treats_invalid_values_as_absent() {
        let c = store::test_conn();
        for tool in ["read", "*"] {
            for invalid in ["", "ALLOW", "sometimes", " allow"] {
                c.execute("DELETE FROM tool_policy", []).unwrap();
                c.execute(
                    "INSERT INTO tool_policy (server, tool, policy) VALUES ('hub', ?1, ?2)",
                    params![tool, invalid],
                )
                .unwrap();
                assert!(decide_checked_in(&c, "hub", "read").is_err());
                assert_eq!(decide_in(&c, "hub", "read"), "allow");
            }
        }
    }

    #[test]
    fn checked_policy_propagates_sql_and_value_type_errors() {
        let c = store::test_conn();
        c.execute(
            "INSERT INTO tool_policy (server, tool, policy) VALUES ('hub', 'read', X'00')",
            [],
        )
        .unwrap();
        assert!(decide_checked_in(&c, "hub", "read").is_err());
        c.execute_batch("DROP TABLE tool_policy").unwrap();
        assert!(decide_checked_in(&c, "hub", "read").is_err());
        assert_eq!(decide_in(&c, "hub", "read"), "allow");
    }

    #[test]
    fn policy_round_trips_and_gates() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));

        assert_eq!(decide("agentos-hub", "recall"), "allow", "default is allow");
        set("agentos-hub", "complete_task", "deny").unwrap();
        assert_eq!(decide("agentos-hub", "complete_task"), "deny");
        assert!(check("agentos-hub", "complete_task")
            .unwrap_err()
            .contains("blocked"));

        set("agentos-hub", "delegate_task", "ask").unwrap();
        assert!(check("agentos-hub", "delegate_task")
            .unwrap_err()
            .contains("requires approval"));
        assert!(check("agentos-hub", "recall").is_ok());
        assert!(set("agentos-hub", "x", "sometimes").is_err());

        assert_eq!(list().len(), 2);

        // A server wildcard covers tools without their own row; a tool row wins over it.
        set("bundle", "*", "ask").unwrap();
        set("bundle", "safe", "allow").unwrap();
        assert_eq!(decide("bundle", "anything"), "ask");
        assert_eq!(decide("bundle", "safe"), "allow");
        let c = crate::store::open().unwrap();
        set_default_in(&c, "bundle", "safe", "ask").unwrap();
        assert_eq!(
            decide("bundle", "safe"),
            "allow",
            "a default never resets a choice"
        );
        set_default_in(&c, "bundle", "safe", "deny").unwrap();
        assert_eq!(decide("bundle", "safe"), "deny", "deny always narrows");
        crate::store::set_test_db(None);
    }
}
