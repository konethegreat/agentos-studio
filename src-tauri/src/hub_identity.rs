//! Agent identity on agentos-hub.
//!
//! Each registered agent gets its own token in its agentos-hub MCP entry, as the environment
//! variable `AGENTOS_HUB_TOKEN`. Studio generates the token, writes it into that agent's entry
//! through the agent's own CLI or config file, and keeps only its SHA-256 in `studio.db` kv
//! `hub.token.<agent>`. Tokens are never logged or returned by any command.
//!
//! The hub resolves its caller once at start-up from the variable. A known caller may write as
//! itself only: `claim_task`, `complete_task`, `delegate_task` (as creator), `remember` and
//! `write_shared_memory` refuse another agent's name. A caller with no token or an unknown one is
//! read-only. `whoami` reports which it is.

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

pub const ENV: &str = "AGENTOS_HUB_TOKEN";
/// Agents that get a token: everything with an agentos-hub registration Studio manages.
pub const AGENTS: [&str; 4] = ["hermes", "claude-code", "qoder", "antigravity"];
const KV_PREFIX: &str = "hub.token.";

/// Tools an unknown caller may still use.
pub const READ_ONLY_TOOLS: [&str; 5] = [
    "fetch_pending_tasks",
    "search_shared_memory",
    "recall",
    "project_brief",
    "whoami",
];

/// Who is calling the hub. `agent` is None for an unknown caller.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Caller {
    pub agent: Option<String>,
    /// Why the caller is unknown, for `whoami` and refusals.
    pub reason: Option<String>,
}

impl Caller {
    pub fn known(agent: &str) -> Self {
        Caller {
            agent: Some(agent.to_string()),
            reason: None,
        }
    }

    pub fn unknown(reason: &str) -> Self {
        Caller {
            agent: None,
            reason: Some(reason.to_string()),
        }
    }
}

/// A fresh random token: 64 hex characters from two v4 UUIDs (OS randomness).
pub fn generate() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// SHA-256 of a token, lowercase hex. Pure.
pub fn hash(token: &str) -> String {
    let d = Sha256::digest(token.trim().as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Remember the token for an agent (its hash only).
pub fn store(agent: &str, token: &str) -> Result<()> {
    let c = crate::store::open()?;
    crate::store::kv_set(&c, &format!("{KV_PREFIX}{agent}"), &hash(token))
}

/// The agent a token belongs to, if any.
pub fn agent_for(token: &str) -> Option<String> {
    if token.trim().is_empty() {
        return None;
    }
    let h = hash(token);
    let c = crate::store::open().ok()?;
    AGENTS
        .iter()
        .find(|a| {
            crate::store::kv_get(&c, &format!("{KV_PREFIX}{a}"))
                .ok()
                .flatten()
                .is_some_and(|stored| stored == h)
        })
        .map(|a| a.to_string())
}

/// Resolve the hub's caller from its environment value.
pub fn resolve(token: Option<&str>) -> Caller {
    match token.map(str::trim).filter(|t| !t.is_empty()) {
        None => Caller::unknown(&format!(
            "no {ENV} in this agent's agentos-hub entry; open AgentOS Studio once to register it"
        )),
        Some(t) => match agent_for(t) {
            Some(a) => Caller::known(&a),
            None => Caller::unknown(&format!(
                "{ENV} is not one AgentOS Studio issued; open AgentOS Studio once to refresh it"
            )),
        },
    }
}

/// May this caller run this tool, acting as `claimed` (the agent name in the arguments, if the
/// tool takes one)? Pure.
pub fn check(caller: &Caller, tool: &str, claimed: Option<&str>) -> Result<()> {
    if READ_ONLY_TOOLS.contains(&tool) {
        return Ok(());
    }
    let Some(me) = caller.agent.as_deref() else {
        bail!(
            "{tool} refused: this caller is read-only ({}). Read tools still work.",
            caller.reason.as_deref().unwrap_or("unknown caller")
        );
    };
    if let Some(name) = claimed.map(str::trim).filter(|n| !n.is_empty()) {
        let name = crate::vault::norm_agent(name).unwrap_or_else(|_| name.to_lowercase());
        if name != me {
            bail!("{tool} refused: you are {me}, not {name}. Agents act only as themselves.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_only_their_hash_is_kept() {
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let a = generate();
        let b = generate();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        store("claude-code", &a).unwrap();
        let c = crate::store::open().unwrap();
        let kept = crate::store::kv_get(&c, "hub.token.claude-code")
            .unwrap()
            .unwrap();
        assert_eq!(kept, hash(&a));
        assert_ne!(kept, a, "never the token itself");
        assert_eq!(agent_for(&a).as_deref(), Some("claude-code"));
        assert_eq!(agent_for(&b), None);
        assert_eq!(resolve(Some(&a)), Caller::known("claude-code"));
        assert!(resolve(Some(&b)).agent.is_none());
        assert!(resolve(None).reason.unwrap().contains(ENV));
        crate::store::set_test_db(None);
    }

    #[test]
    fn agents_write_only_as_themselves_and_unknown_callers_only_read() {
        let me = Caller::known("claude-code");
        assert!(check(&me, "claim_task", Some("claude-code")).is_ok());
        assert!(
            check(&me, "claim_task", Some("Claude")).is_ok(),
            "aliases normalise"
        );
        let e = check(&me, "complete_task", Some("hermes"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("not hermes"), "{e}");
        assert!(check(&me, "remember", None).is_ok());

        let anon = Caller::unknown("no token");
        for t in READ_ONLY_TOOLS {
            assert!(check(&anon, t, None).is_ok(), "{t} stays readable");
        }
        for t in [
            "delegate_task",
            "claim_task",
            "complete_task",
            "remember",
            "write_shared_memory",
        ] {
            assert!(check(&anon, t, None)
                .unwrap_err()
                .to_string()
                .contains("read-only"));
        }
    }
}
