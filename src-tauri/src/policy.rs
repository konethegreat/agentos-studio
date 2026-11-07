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

use anyhow::Result;
use rusqlite::params;
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
    let Ok(c) = store::open() else {
        return "allow".into();
    };
    c.query_row(
        "SELECT policy FROM tool_policy WHERE server = ?1 AND tool = ?2",
        params![server, tool],
        |r| r.get::<_, String>(0),
    )
    .map(|p| {
        if POLICIES.contains(&p.as_str()) {
            p
        } else {
            "allow".into()
        }
    })
    .unwrap_or_else(|_| "allow".into())
}

pub fn set(server: &str, tool: &str, policy: &str) -> Result<()> {
    if !POLICIES.contains(&policy) {
        anyhow::bail!("policy must be allow, ask or deny");
    }
    let c = store::open()?;
    c.execute(
        "INSERT INTO tool_policy (server, tool, policy) VALUES (?1, ?2, ?3)
         ON CONFLICT(server, tool) DO UPDATE SET policy = excluded.policy",
        params![server, tool, policy],
    )?;
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
        crate::store::set_test_db(None);
    }
}
