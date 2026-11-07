//! The model layer: what providers exist, what models they actually offer, and what is active.
//!
//! The catalog is Hermes's own provider cache, not a hardcoded list, so a model that appears in the
//! runtime appears in Studio without a release. Nothing here writes configuration; pins live on the
//! mission or the chat, which is the whole point of a per-session switchboard.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    pub models: Vec<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Catalog {
    pub providers: Vec<ProviderInfo>,
    pub active_provider: Option<String>,
    pub active_model: Option<String>,
}

fn cache_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(crate::settings::home)
        .join("hermes")
        .join("provider_models_cache.json")
}

fn config_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(crate::settings::home)
        .join("hermes")
        .join("config.yaml")
}

/// Human names for the ids Hermes uses. Unknown ids are shown as they are rather than hidden.
fn label(id: &str) -> String {
    match id {
        "claude-subscription-directsdk" | "claude-subscription-directsdk-experimental" => {
            "Claude subscription".into()
        }
        "antigravity-oauth" | "antigravity" => "Antigravity (Argon)".into(),
        "anthropic" => "Anthropic API".into(),
        "openrouter" => "OpenRouter".into(),
        "openai" => "OpenAI".into(),
        "deepseek" => "DeepSeek".into(),
        "gemini" | "google" => "Google Gemini".into(),
        "nous" | "nous-portal" => "Nous Portal".into(),
        "ollama" | "local" => "Local (Ollama)".into(),
        other => other.to_string(),
    }
}

/// Turn the cache JSON into providers. Pure so it can be tested without a home directory.
pub fn parse_cache(v: &Value) -> Vec<ProviderInfo> {
    let Some(obj) = v.as_object() else {
        return Vec::new();
    };
    let mut out: Vec<ProviderInfo> = obj
        .iter()
        .map(|(id, entry)| {
            let models = entry
                .get("models")
                .and_then(|m| m.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            ProviderInfo {
                id: id.clone(),
                label: label(id),
                models,
                active: false,
            }
        })
        .collect();
    out.sort_by_key(|p| p.label.to_lowercase());
    out
}

/// The first `key: value` at any indentation, which is how Hermes writes `provider:` and `model:`.
pub fn parse_active(yaml: &str, key: &str) -> Option<String> {
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) else {
            continue;
        };
        let value = rest.trim().trim_matches('"').trim_matches('\'');
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

pub fn catalog() -> Catalog {
    let mut cat = Catalog::default();
    if let Ok(text) = std::fs::read_to_string(cache_path()) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            cat.providers = parse_cache(&v);
        }
    }
    if let Ok(yaml) = std::fs::read_to_string(config_path()) {
        cat.active_provider = parse_active(&yaml, "provider");
        cat.active_model = parse_active(&yaml, "model");
    }
    if let Some(active) = cat.active_provider.as_deref() {
        for p in &mut cat.providers {
            p.active = p.id == active;
        }
    }
    cat
}

pub fn models_for(provider: &str) -> Vec<String> {
    catalog()
        .providers
        .into_iter()
        .find(|p| p.id == provider)
        .map(|p| p.models)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_becomes_providers_with_labels() {
        let v: Value = serde_json::from_str(
            r#"{"anthropic":{"models":["claude-a","claude-b"]},"openrouter":{"models":["x/y"]}}"#,
        )
        .unwrap();
        let p = parse_cache(&v);
        assert_eq!(p.len(), 2);
        assert!(p.iter().any(|x| x.id == "anthropic" && x.models.len() == 2));
        assert!(p.iter().any(|x| x.label == "OpenRouter"));
    }

    #[test]
    fn broken_entries_do_not_hide_the_rest() {
        let v: Value =
            serde_json::from_str(r#"{"a":{},"b":{"models":"nope"},"c":{"models":["m"]}}"#).unwrap();
        let p = parse_cache(&v);
        assert_eq!(p.len(), 3);
        assert!(p.iter().all(|x| x.models.len() <= 1));
        assert_edge();
    }

    fn assert_edge() {
        assert_eq!(parse_cache(&Value::Null).len(), 0);
    }

    #[test]
    fn active_fields_come_from_the_config() {
        let yaml = "model:\n  name: deepseek\nprovider: \"openrouter\"\nmodel: claude-fable-5.1\n";
        assert_eq!(
            parse_active(yaml, "provider").as_deref(),
            Some("openrouter")
        );
        assert_eq!(
            parse_active(yaml, "model").as_deref(),
            Some("claude-fable-5.1")
        );
        assert_eq!(parse_active("# provider: no\n", "provider"), None);
    }
}
