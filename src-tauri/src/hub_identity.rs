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

use anyhow::{bail, ensure, Result};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

pub const ENV: &str = "AGENTOS_HUB_TOKEN";
/// Agents that get a token: everything with an agentos-hub registration Studio manages.
pub const AGENTS: [&str; 4] = ["hermes", "claude-code", "qoder", "antigravity"];
const KV_PREFIX: &str = "hub.token.";

/// Tools an unknown caller may still use.
pub const READ_ONLY_TOOLS: [&str; 6] = [
    "agents",
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
    let c = crate::store::open_existing_read_only().ok()?;
    agent_for_in(&c, token).ok().flatten()
}

/// Resolve a token without hiding corrupt identities or store errors.
pub fn agent_for_in(c: &Connection, token: &str) -> Result<Option<String>> {
    if token.trim().is_empty() {
        return Ok(None);
    }
    let h = hash(token);
    let mut found = None;
    for agent in AGENTS {
        if let Some(stored) = crate::store::kv_get(c, &format!("{KV_PREFIX}{agent}"))? {
            ensure!(
                stored.len() == 64
                    && stored
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid stored hub identity for {agent}"
            );
            if stored == h {
                ensure!(found.is_none(), "ambiguous stored hub identity");
                found = Some(agent.to_string());
            }
        }
    }
    Ok(found)
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
    fn unknown_provided_token_does_not_create_missing_store() {
        let _g = crate::store::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("studio.db");
        crate::store::set_test_db(Some(path.clone()));
        let caller = resolve(Some("unknown-test-token"));
        let agent = agent_for("unknown-test-token");
        crate::store::set_test_db(None);

        assert!(caller.agent.is_none());
        assert!(agent.is_none());
        assert!(!path.exists(), "unknown token must not create a store");
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn checked_identity_uses_trimmed_sha256_and_only_registered_agents() {
        let c = crate::store::test_conn();
        let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        crate::store::kv_set(&c, "hub.token.unknown", digest).unwrap();
        assert_eq!(agent_for_in(&c, "abc").unwrap(), None);
        crate::store::kv_set(&c, "hub.token.qoder", digest).unwrap();
        assert_eq!(
            agent_for_in(&c, " \tabc\n").unwrap().as_deref(),
            Some("qoder")
        );
        assert_eq!(agent_for_in(&c, "other-token").unwrap(), None);
        assert_eq!(agent_for_in(&c, " \t\n").unwrap(), None);
    }

    #[test]
    fn malformed_or_ambiguous_registered_identity_fails_closed() {
        let _g = crate::store::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("studio.db");
        let c = crate::store::open_at(&path).unwrap();
        crate::store::kv_set(&c, "hub.token.hermes", &hash("abc")).unwrap();
        for malformed in [
            "".to_string(),
            "abc".to_string(),
            "A".repeat(64),
            "g".repeat(64),
        ] {
            crate::store::kv_set(&c, "hub.token.qoder", &malformed).unwrap();
            assert!(agent_for_in(&c, "abc").is_err());
        }
        crate::store::kv_set(&c, "hub.token.qoder", &hash("abc")).unwrap();
        assert!(agent_for_in(&c, "abc").is_err());
        crate::store::set_test_db(Some(path));
        let agent = agent_for("abc");
        let caller = resolve(Some("abc"));
        crate::store::set_test_db(None);
        assert!(agent.is_none());
        assert!(caller.agent.is_none());
    }

    #[test]
    fn checked_identity_propagates_sql_and_value_type_errors() {
        let c = crate::store::test_conn();
        c.execute(
            "INSERT INTO kv (key, value) VALUES ('hub.token.qoder', X'00')",
            [],
        )
        .unwrap();
        assert!(agent_for_in(&c, "abc").is_err());
        c.execute_batch("DROP TABLE kv").unwrap();
        assert!(agent_for_in(&c, "abc").is_err());
        assert_eq!(agent_for_in(&c, "").unwrap(), None);
    }

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
