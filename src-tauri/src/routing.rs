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
    /// Extra Claude Code tool rules for headless mission workers in this repository, for example
    /// `Bash(npm test*)` or `Bash(cargo *)`. Passed as `--allowedTools`; a bare shell rule with no
    /// pattern is refused at parse time, because it would allow any command.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// What every headless Claude Code mission worker may run: git, pull requests and the hub.
pub const BASE_TOOLS: [&str; 3] = ["mcp__agentos-hub", "Bash(git *)", "Bash(gh pr *)"];

/// The `--allowedTools` list for a Claude Code worker: the base rules plus the repository's own,
/// each `Bash(...)` rule mirrored as `PowerShell(...)` because Claude Code on Windows runs shell
/// commands through either tool. Deduplicated, order kept. Pure.
pub fn claude_allowed_tools(extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    for rule in BASE_TOOLS
        .iter()
        .map(|s| s.to_string())
        .chain(extra.iter().cloned())
    {
        let rule = rule.trim().to_string();
        if rule.is_empty() {
            continue;
        }
        if let Some(inner) = rule.strip_prefix("Bash(") {
            push(rule.clone());
            push(format!("PowerShell({inner}"));
        } else {
            push(rule);
        }
    }
    out
}

/// The same allowlist in Antigravity's rule syntax, for `agy --settings`. `mcp__<server>` becomes
/// `mcp(<server>/*)` and `Bash(<pattern>)` becomes `command(regex:<pattern>)` with `*` as `.*`.
/// A plain `command(git)` prefix rule does not match on Windows (verified with agy 1.5.3), the
/// regex form does. `PowerShell(...)` mirrors and other Claude tool names are dropped. Pure.
pub fn agy_allow_rules(claude_rules: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in claude_rules {
        let r = r.trim();
        let rule = if let Some(server) = r.strip_prefix("mcp__") {
            format!("mcp({server}/*)")
        } else if let Some(inner) = r.strip_prefix("Bash(").and_then(|x| x.strip_suffix(')')) {
            let pattern: String = inner
                .split('*')
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(".*");
            format!("command(regex:{pattern})")
        } else {
            continue;
        };
        if !out.contains(&rule) {
            out.push(rule);
        }
    }
    out
}

/// A rule that would hand a headless worker an unrestricted shell or skip permissions. Pure.
fn unsafe_rule(rule: &str) -> bool {
    let r = rule.trim();
    let bare = matches!(r, "Bash" | "PowerShell" | "Bash(*)" | "PowerShell(*)" | "*");
    bare || r.to_lowercase().contains("dangerously")
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
    if let Some(bad) = p.allowed_tools.iter().find(|r| unsafe_rule(r)) {
        anyhow::bail!("allowed_tools rule '{bad}' would allow any command; name a pattern");
    }
    Ok(p)
}

/// The repository's extra tool rules for headless workers (empty without a policy).
pub fn allowed_tools(repo_dir: &Path) -> Vec<String> {
    load(repo_dir).map(|p| p.allowed_tools).unwrap_or_default()
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
    fn worker_tools_are_the_base_plus_the_repo_mirrored_for_powershell() {
        let t = claude_allowed_tools(&["Bash(npm test*)".into(), "Bash(git *)".into()]);
        assert_eq!(t[0], "mcp__agentos-hub");
        assert!(t.contains(&"Bash(git *)".to_string()));
        assert!(t.contains(&"PowerShell(git *)".to_string()));
        assert!(t.contains(&"PowerShell(gh pr *)".to_string()));
        assert!(t.contains(&"PowerShell(npm test*)".to_string()));
        assert_eq!(
            t.iter().filter(|r| *r == "Bash(git *)").count(),
            1,
            "deduplicated"
        );

        let p = parse(r#"{"allowed_tools":["Bash(cargo *)"]}"#).unwrap();
        assert_eq!(p.allowed_tools, vec!["Bash(cargo *)"]);
        for bad in [
            "Bash",
            "PowerShell(*)",
            "*",
            "--dangerously-skip-permissions",
        ] {
            let json = format!(r#"{{"allowed_tools":["{bad}"]}}"#);
            assert!(parse(&json).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn antigravity_gets_the_same_allowlist_in_its_own_syntax() {
        let claude = claude_allowed_tools(&["Bash(npm test*)".into(), "Bash(node *.js)".into()]);
        let agy = agy_allow_rules(&claude);
        assert_eq!(
            agy,
            vec![
                "mcp(agentos-hub/*)",
                "command(regex:git .*)",
                "command(regex:gh pr .*)",
                "command(regex:npm test.*)",
                "command(regex:node .*\\.js)",
            ]
        );
        assert!(!agy
            .iter()
            .any(|r| r.contains("PowerShell") || r == "command(*)"));
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
