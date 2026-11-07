//! Team routing policy: a file in the repository that says which model a role should use.
//!
//! A policy is committed next to the code, so a team agrees on model choices in review rather than
//! in someone's local settings. Studio reads it when a mission is created; an explicit provider or
//! model on the mission still wins, because a policy is a default and not a cage.
//!
//! File shape, `.agentos/routing.json` in the repository root:
//!
//! ```json
//! {
//!   "roles": {
//!     "implementer": { "provider": "claude-subscription", "model": "claude-opus-5-5" },
//!     "reviewer":    { "provider": "antigravity-oauth",   "model": "gemini-4-argon-medium" }
//!   },
//!   "allow_merge": false
//! }
//! ```

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RoleChoice {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Policy {
    #[serde(default)]
    pub roles: std::collections::BTreeMap<String, RoleChoice>,
    /// Whether this repository may merge a mission's pull request at all.
    #[serde(default)]
    pub allow_merge: bool,
}

pub fn path_in(repo_dir: &Path) -> std::path::PathBuf {
    repo_dir.join(".agentos").join("routing.json")
}

pub fn parse(text: &str) -> Result<Policy> {
    let p: Policy = serde_json::from_str(text)?;
    for (role, c) in &p.roles {
        if !crate::missions::ROLES.contains(&role.as_str()) {
            anyhow::bail!("unknown role '{role}' in routing policy");
        }
        if c.provider.trim().is_empty() {
            anyhow::bail!("role '{role}' has no provider");
        }
    }
    Ok(p)
}

pub fn load(repo_dir: &Path) -> Option<Policy> {
    let text = std::fs::read_to_string(path_in(repo_dir)).ok()?;
    parse(&text).ok()
}

/// Provider and model a mission should inherit for this role.
pub fn defaults_for(repo_dir: &Path, role: &str) -> Option<RoleChoice> {
    load(repo_dir)?.roles.get(role).cloned()
}

/// Whether the policy allows merging in this repository.
pub fn merge_allowed(repo_dir: &Path) -> bool {
    load(repo_dir).map(|p| p.allow_merge).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_parses_and_rejects_nonsense() {
        let p = parse(
            r#"{"roles":{"implementer":{"provider":"claude-subscription","model":"m"},"reviewer":{"provider":"antigravity-oauth"}},"allow_merge":true}"#,
        )
        .unwrap();
        assert_eq!(p.roles["implementer"].model, "m");
        assert!(p.allow_merge);

        assert!(
            parse(r#"{"roles":{"wizard":{"provider":"x"}}}"#).is_err(),
            "unknown role"
        );
        assert!(
            parse(r#"{"roles":{"reviewer":{"provider":"  "}}}"#).is_err(),
            "empty provider"
        );
        assert!(parse("not json").is_err());
    }

    #[test]
    fn missing_file_means_no_policy_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(load(d.path()).is_none());
        assert!(defaults_for(d.path(), "implementer").is_none());
        assert!(!merge_allowed(d.path()));
    }

    #[test]
    fn defaults_come_from_the_repo_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".agentos")).unwrap();
        std::fs::write(
            path_in(d.path()),
            r#"{"roles":{"implementer":{"provider":"claude-subscription","model":"opus"}}}"#,
        )
        .unwrap();
        let c = defaults_for(d.path(), "implementer").unwrap();
        assert_eq!(c.provider, "claude-subscription");
        assert_eq!(c.model, "opus");
        assert!(defaults_for(d.path(), "reviewer").is_none());
    }
}
