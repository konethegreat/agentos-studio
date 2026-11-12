//! Connecting a provider without editing files by hand.
//!
//! Three things can be connected here:
//! - a Claude subscription, which works through the Claude CLI's own login;
//! - an Antigravity subscription, which works through the Hermes provider plugin;
//! - an OpenRouter API key, which has to be written to Hermes's env file.
//!
//! Keys are written to `%LOCALAPPDATA%/hermes/.env`, because that is where Hermes reads them from.
//! The app never stores a copy and never displays a value back, only whether one is present.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Result};
use serde::Serialize;

use crate::procs;

#[derive(Debug, Clone, Serialize)]
pub struct Connection {
    pub id: String,
    pub label: String,
    pub connected: bool,
    pub detail: String,
    pub needs: String,
    pub env_var: Option<String>,
    /// Setup component that connects it (`install_or_repair_component`), with its button label.
    pub setup: Option<String>,
    pub setup_label: Option<String>,
    /// Where to get a key or an account.
    pub link: Option<String>,
}

fn hermes_env() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(crate::settings::home)
        .join("hermes")
        .join(".env")
}

/// Insert or replace `KEY=value` in a dotenv file, preserving every other line.
/// Returns the new content; the caller writes it.
pub fn upsert_env(text: &str, key: &str, value: &str) -> String {
    let line = format!("{key}={value}");
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    for l in text.lines() {
        if l.trim_start().starts_with(&format!("{key}=")) {
            if !replaced {
                out.push(line.clone());
                replaced = true;
            }
            continue;
        }
        out.push(l.to_string());
    }
    if !replaced {
        if !out.is_empty() && !out.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
            out.push(String::new());
        }
        out.push(line);
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

pub fn env_has(key: &str) -> bool {
    std::fs::read_to_string(hermes_env())
        .map(|t| {
            crate::notion::parse_env(&t, key)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn claude_connected() -> (bool, String) {
    let Some(exe) = procs::find("claude") else {
        return (false, "Claude Code is not installed".into());
    };
    let mut c = Command::new(exe);
    c.arg("auth").arg("status");
    c.env("PATH", procs::search_path());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(procs::CREATE_NO_WINDOW);
    }
    let out = c.output().ok();
    let text = out
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    claude_auth_verdict(&text)
}

/// Read `claude auth status` JSON. Only `loggedIn: true` counts: the old text check also matched
/// "Not logged in", so a signed-out Claude read as connected. Pure.
pub fn claude_auth_verdict(text: &str) -> (bool, String) {
    let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap_or_default();
    if v.get("loggedIn").and_then(serde_json::Value::as_bool) == Some(true) {
        let how = v
            .get("authMethod")
            .and_then(|x| x.as_str())
            .unwrap_or("claude.ai");
        (true, format!("Signed in through the Claude CLI ({how})"))
    } else {
        (
            false,
            "Signed out. Run `claude auth login` once, or use Sign in".into(),
        )
    }
}

/// `hermes plugins show <name>` prints `Status: enabled` for a plugin Hermes will load. A folder on
/// disk is not enough: Hermes disables a plugin it cannot read, and the provider then fails.
pub fn plugin_enabled_in(show_output: &str) -> bool {
    show_output
        .lines()
        .any(|l| l.trim().eq_ignore_ascii_case("status: enabled"))
}

fn antigravity_connected() -> (bool, String) {
    let out = procs::run("hermes", &["plugins", "show", "antigravity-oauth"]);
    let text = out
        .as_ref()
        .map(|o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .unwrap_or_default();
    let installed = out.as_ref().is_some_and(|o| o.status.success());
    if installed && plugin_enabled_in(&text) {
        (
            true,
            "The antigravity-oauth plugin is enabled in Hermes".into(),
        )
    } else if installed || text.contains("Access is denied") {
        (
            false,
            "The antigravity-oauth plugin is installed but Hermes has disabled it. Run `hermes plugins show antigravity-oauth` for the reason".into(),
        )
    } else {
        (
            false,
            "Optional. Needs the community antigravity-oauth plugin and your Antigravity login"
                .into(),
        )
    }
}

pub fn status() -> Vec<Connection> {
    let (claude, claude_detail) = claude_connected();
    let (anti, anti_detail) = antigravity_connected();
    let openrouter = env_has("OPENROUTER_API_KEY");
    vec![
        Connection {
            id: "claude-subscription".into(),
            label: "Claude subscription".into(),
            connected: claude,
            detail: claude_detail,
            needs: "Your Claude Pro or Max login, through the Claude CLI".into(),
            env_var: None,
            setup: Some(
                if procs::find("claude").is_some() {
                    "claude-auth"
                } else {
                    "claude-cli"
                }
                .into(),
            ),
            setup_label: Some(
                if procs::find("claude").is_some() {
                    "Sign in"
                } else {
                    "Install Claude Code"
                }
                .into(),
            ),
            link: Some("https://claude.ai/upgrade".into()),
        },
        Connection {
            id: "antigravity-oauth".into(),
            label: "Antigravity (Argon)".into(),
            connected: anti,
            detail: anti_detail.clone(),
            needs: anti_detail,
            env_var: None,
            setup: (!anti && procs::find("hermes").is_some()).then(|| "antigravity-plugin".into()),
            setup_label: (!anti && procs::find("hermes").is_some())
                .then(|| "Install plugin and sign in".into()),
            link: Some(crate::bootstrapper::ANTIGRAVITY_PLUGIN_URL.into()),
        },
        Connection {
            id: "openrouter".into(),
            label: "OpenRouter".into(),
            connected: openrouter,
            detail: if openrouter {
                "Key present in the Hermes env file".into()
            } else {
                "No key yet".into()
            },
            needs: "An OpenRouter API key, metered per token".into(),
            env_var: Some("OPENROUTER_API_KEY".into()),
            setup: None,
            setup_label: None,
            link: Some("https://openrouter.ai/keys".into()),
        },
    ]
}

/// Save an API key where Hermes reads it. Refuses to write anything that is obviously not a key.
pub fn save_key(provider: &str, key: &str) -> Result<String> {
    let var = match provider {
        "openrouter" => "OPENROUTER_API_KEY",
        other => bail!("{other} does not take an API key here; it uses its own login"),
    };
    let k = key.trim();
    if k.len() < 20 || k.chars().any(char::is_whitespace) {
        bail!("that does not look like an API key");
    }
    let path = hermes_env();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = upsert_env(&existing, var, k);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, updated)?;
    // Never echo the value; only the variable name.
    Ok(format!("{var} saved to the Hermes env file"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_logged_in_true_counts_as_signed_in() {
        assert!(claude_auth_verdict(r#"{"loggedIn": true, "authMethod": "claude.ai"}"#).0);
        assert!(!claude_auth_verdict(r#"{"loggedIn": false}"#).0);
        assert!(!claude_auth_verdict("Not logged in. Run claude auth login").0);
        assert!(!claude_auth_verdict("").0);
    }

    #[test]
    fn plugin_status_needs_enabled_not_just_present() {
        assert!(plugin_enabled_in(
            "antigravity-oauth v0.4\nStatus: enabled\n"
        ));
        assert!(!plugin_enabled_in(
            "antigravity-oauth v0.4\nStatus: disabled\n"
        ));
        assert!(!plugin_enabled_in(
            "Skipping unreadable plugin directory antigravity-oauth"
        ));
    }

    #[test]
    fn upsert_replaces_without_duplicating_or_touching_others() {
        let start = "A=1\nOPENROUTER_API_KEY=old\nB=2\n";
        let out = upsert_env(start, "OPENROUTER_API_KEY", "new");
        assert_eq!(out.matches("OPENROUTER_API_KEY=").count(), 1);
        assert!(out.contains("OPENROUTER_API_KEY=new"));
        assert!(out.contains("A=1") && out.contains("B=2"));
        assert!(!out.contains("old"));
    }

    #[test]
    fn upsert_appends_when_missing() {
        let out = upsert_env("A=1\n", "NOTION_API_KEY", "x");
        assert!(out.contains("NOTION_API_KEY=x"));
        assert!(out.contains("A=1"));
    }

    #[test]
    fn keys_are_validated_before_writing() {
        assert!(save_key("claude-subscription", "whatever").is_err());
        assert!(save_key("openrouter", "short").is_err());
        assert!(save_key("openrouter", "has spaces in it and is long enough").is_err());
    }

    #[test]
    fn status_names_the_three_subscriptions() {
        let s = status();
        assert_eq!(s.len(), 3);
        assert!(s
            .iter()
            .any(|c| c.id == "openrouter" && c.env_var.as_deref() == Some("OPENROUTER_API_KEY")));
    }
}
