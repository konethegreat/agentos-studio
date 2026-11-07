//! Hermes' model layer, shown and edited without rebuilding it: the fallback chain, credential
//! pools, mixture-of-agents presets, and Studio's per-folder default model for new chats.
//!
//! Reads go through `hermes config get` (resolved YAML) and `hermes auth list` (text); Studio never
//! opens `auth.json`, which holds secrets. Writes go through `hermes config set/unset` and
//! `hermes auth`. Anything that needs a secret or an interactive picker opens a terminal instead.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_yaml::Value as Yaml;

use crate::procs;

fn hermes(args: &[&str]) -> Result<String> {
    let out = procs::run("hermes", args).ok_or_else(|| anyhow!("hermes is not installed"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    if out.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let msg = stderr
        .lines()
        .chain(stdout.lines())
        .rev()
        .find(|l| !l.starts_with("Skipping unreadable plugin") && !l.trim().is_empty())
        .unwrap_or("hermes failed")
        .to_string();
    bail!("{msg}")
}

fn config_get(key: &str) -> Yaml {
    hermes(&["config", "get", key])
        .ok()
        .and_then(|t| serde_yaml::from_str(&t).ok())
        .unwrap_or(Yaml::Null)
}

fn safe_id(s: &str) -> bool {
    !s.trim().is_empty()
        && s.len() <= 200
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/[]@".contains(c))
}

// ------------------------------------------------------------------------------ fallback chain

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Route {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

pub fn parse_chain(v: &Yaml) -> Vec<Route> {
    let items: Vec<Yaml> = match v {
        Yaml::Sequence(s) => s.clone(),
        Yaml::Mapping(_) => vec![v.clone()],
        _ => vec![],
    };
    items
        .into_iter()
        .filter_map(|i| serde_yaml::from_value::<Route>(i).ok())
        .filter(|r| !r.provider.is_empty() && !r.model.is_empty())
        .collect()
}

pub fn fallback_chain() -> Vec<Route> {
    let mut chain = parse_chain(&config_get("fallback_providers"));
    for r in parse_chain(&config_get("fallback_model")) {
        if !chain
            .iter()
            .any(|c| c.provider == r.provider && c.model == r.model)
        {
            chain.push(r);
        }
    }
    chain
}

/// The value written by `hermes config set fallback_providers` (JSON is valid YAML). Pure.
pub fn chain_value(chain: &[Route]) -> Result<String> {
    for r in chain {
        if !safe_id(&r.provider) || !safe_id(&r.model) {
            bail!("Invalid fallback entry {}:{}", r.provider, r.model);
        }
    }
    Ok(serde_json::to_string(chain)?)
}

/// Replace the whole chain, in order. An empty chain removes the key.
pub fn set_fallback_chain(chain: &[Route]) -> Result<()> {
    if chain.is_empty() {
        hermes(&["config", "unset", "fallback_providers"]).map(|_| ())
    } else {
        hermes(&["config", "set", "fallback_providers", &chain_value(chain)?]).map(|_| ())
    }
}

// ---------------------------------------------------------------------------- credential pools

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Credential {
    pub provider: String,
    pub index: u32,
    pub label: String,
    pub kind: String,
    pub id: String,
    pub priority: i64,
    pub source: String,
    /// Marked with an arrow by Hermes: the credential it uses next.
    pub current: bool,
    /// Anything after the source (exhaustion, cooldown), as Hermes printed it.
    pub status: String,
}

/// Parse `hermes auth list`. Pure. Lines look like
/// `  #1  OPENROUTER_API_KEY   api_key id=c94e0f priority=0 env:OPENROUTER_API_KEY ←`.
pub fn parse_auth_list(text: &str) -> Vec<Credential> {
    let mut out = Vec::new();
    let mut provider = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if !line.starts_with(' ') && t.contains(" credential") && t.ends_with(':') {
            provider = t.split_whitespace().next().unwrap_or("").to_string();
            continue;
        }
        let Some(rest) = t.strip_prefix('#') else {
            continue;
        };
        let words: Vec<&str> = rest.split_whitespace().collect();
        let Some(index) = words.first().and_then(|w| w.parse::<u32>().ok()) else {
            continue;
        };
        let id_at = words.iter().position(|w| w.starts_with("id="));
        let Some(id_at) = id_at else { continue };
        let kind = if id_at >= 2 {
            words[id_at - 1].to_string()
        } else {
            String::new()
        };
        let label = if id_at >= 3 {
            words[1..id_at - 1].join(" ")
        } else {
            String::new()
        };
        let mut priority = 0;
        let mut source = String::new();
        let mut status = Vec::new();
        let mut current = false;
        for w in &words[id_at + 1..] {
            if let Some(p) = w.strip_prefix("priority=") {
                priority = p.parse().unwrap_or(0);
            } else if *w == "←" {
                current = true;
            } else if source.is_empty() {
                source = w.to_string();
            } else {
                status.push(*w);
            }
        }
        out.push(Credential {
            provider: provider.clone(),
            index,
            label,
            kind,
            id: words[id_at].trim_start_matches("id=").to_string(),
            priority,
            source,
            current,
            status: status.join(" "),
        });
    }
    out
}

pub fn credentials() -> Result<Vec<Credential>> {
    Ok(parse_auth_list(&hermes(&["auth", "list"])?))
}

/// `priority`, `reset` or `remove` on one credential, by its id.
pub fn credential_action(
    provider: &str,
    id: &str,
    action: &str,
    priority: Option<u32>,
) -> Result<String> {
    if !safe_id(provider) || !id.chars().all(|c| c.is_ascii_alphanumeric()) || id.is_empty() {
        bail!("Invalid credential");
    }
    let out = match action {
        "priority" => {
            let p = priority
                .ok_or_else(|| anyhow!("priority needs a number"))?
                .to_string();
            hermes(&["auth", "priority", provider, id, &p])?
        }
        "reset" => hermes(&["auth", "reset", provider, id])?,
        "remove" => hermes(&["auth", "remove", provider, id])?,
        other => bail!("Unknown credential action {other}"),
    };
    Ok(out.trim().to_string())
}

// ------------------------------------------------------------------------ mixture of agents

#[derive(Serialize, Debug, Clone, Default)]
pub struct Moa {
    pub default_preset: String,
    pub active_preset: String,
    pub presets: Vec<MoaPreset>,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct MoaPreset {
    pub name: String,
    pub enabled: bool,
    pub reference_models: Vec<Route>,
    pub aggregator: Option<Route>,
}

pub fn parse_moa(v: &Yaml) -> Moa {
    let s = |k: &str| v.get(k).and_then(Yaml::as_str).unwrap_or("").to_string();
    let mut presets: Vec<MoaPreset> = v
        .get("presets")
        .and_then(Yaml::as_mapping)
        .map(|m| {
            m.iter()
                .filter_map(|(k, p)| {
                    Some(MoaPreset {
                        name: k.as_str()?.to_string(),
                        enabled: p.get("enabled").and_then(Yaml::as_bool).unwrap_or(true),
                        reference_models: p
                            .get("reference_models")
                            .map(parse_chain)
                            .unwrap_or_default(),
                        aggregator: p
                            .get("aggregator")
                            .map(parse_chain)
                            .and_then(|c| c.into_iter().next()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    presets.sort_by(|a, b| a.name.cmp(&b.name));
    Moa {
        default_preset: s("default_preset"),
        active_preset: s("active_preset"),
        presets,
    }
}

pub fn moa() -> Moa {
    parse_moa(&config_get("moa"))
}

/// Turn mixture-of-agents on with a preset, or off with an empty name.
pub fn set_active_moa(preset: &str) -> Result<()> {
    if preset.is_empty() {
        return hermes(&["config", "set", "moa.active_preset", "''"]).map(|_| ());
    }
    if !moa().presets.iter().any(|p| p.name == preset) {
        bail!("No MoA preset named {preset}");
    }
    hermes(&["config", "set", "moa.active_preset", preset]).map(|_| ())
}

// --------------------------------------------------------------------- per-folder model defaults

pub const KV_FOLDER_MODELS: &str = "chat.folder_models";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FolderModel {
    pub folder: String,
    pub provider: String,
    pub model: String,
}

fn norm(p: &str) -> String {
    p.trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// The default for `cwd`: the entry whose folder is the longest prefix of it. Pure.
pub fn folder_default<'a>(entries: &'a [FolderModel], cwd: &str) -> Option<&'a FolderModel> {
    let c = norm(cwd);
    entries
        .iter()
        .filter(|e| {
            let f = norm(&e.folder);
            !f.is_empty() && (c == f || c.starts_with(&format!("{f}/")))
        })
        .max_by_key(|e| norm(&e.folder).len())
}

pub fn folder_models() -> Vec<FolderModel> {
    crate::store::open()
        .ok()
        .and_then(|c| crate::store::kv_get(&c, KV_FOLDER_MODELS).ok().flatten())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn set_folder_models(entries: &[FolderModel]) -> Result<()> {
    for e in entries {
        if !std::path::Path::new(e.folder.trim()).is_dir() {
            bail!("{} is not a folder", e.folder);
        }
        if !safe_id(&e.provider) || !safe_id(&e.model) {
            bail!("Invalid model {}:{}", e.provider, e.model);
        }
    }
    let c = crate::store::open()?;
    crate::store::kv_set(&c, KV_FOLDER_MODELS, &serde_json::to_string(entries)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hermes_auth_list() {
        let text = "nous (1 credentials):\n  #1  someone@example.com oauth   id=020fec priority=0 device_code ←\n\nopenrouter (2 credentials):\n  #1  OPENROUTER_API_KEY   api_key id=c94e0f priority=0 env:OPENROUTER_API_KEY ←\n  #2  backup key   api_key id=aa11bb priority=1 manual exhausted until 12:00\n";
        let c = parse_auth_list(text);
        assert_eq!(c.len(), 3);
        assert_eq!(
            (c[0].provider.as_str(), c[0].kind.as_str(), c[0].current),
            ("nous", "oauth", true)
        );
        assert_eq!(c[1].label, "OPENROUTER_API_KEY");
        assert_eq!(c[1].source, "env:OPENROUTER_API_KEY");
        assert_eq!(
            (c[2].label.as_str(), c[2].priority, c[2].id.as_str()),
            ("backup key", 1, "aa11bb")
        );
        assert_eq!(c[2].status, "exhausted until 12:00");
        assert!(!c[2].current);
    }

    #[test]
    fn chain_and_moa_read_from_resolved_yaml() {
        let chain: Yaml = serde_yaml::from_str(
            "- provider: openrouter\n  model: deepseek/x\n- provider: ''\n  model: y\n",
        )
        .unwrap();
        assert_eq!(parse_chain(&chain).len(), 1);
        assert_eq!(
            chain_value(&parse_chain(&chain)).unwrap(),
            r#"[{"provider":"openrouter","model":"deepseek/x"}]"#
        );
        assert!(chain_value(&[Route {
            provider: "--x".into(),
            model: "m".into(),
            base_url: None
        }])
        .is_err());
        let moa: Yaml = serde_yaml::from_str("default_preset: default\nactive_preset: ''\npresets:\n  default:\n    reference_models:\n      - provider: openai-codex\n        model: gpt-5.5\n    aggregator:\n      provider: openrouter\n      model: anthropic/claude-opus-4.8\n    enabled: true\n").unwrap();
        let m = parse_moa(&moa);
        assert_eq!(m.active_preset, "");
        assert_eq!(m.presets[0].reference_models[0].model, "gpt-5.5");
        assert_eq!(
            m.presets[0].aggregator.as_ref().unwrap().provider,
            "openrouter"
        );
    }

    #[test]
    fn the_deepest_folder_default_wins() {
        let e = |f: &str, m: &str| FolderModel {
            folder: f.into(),
            provider: "p".into(),
            model: m.into(),
        };
        let list = vec![
            e("C:/Users/me", "a"),
            e("C:\\Users\\me\\repo", "b"),
            e("C:/Users/me/repo2", "c"),
        ];
        assert_eq!(
            folder_default(&list, "c:/users/me/repo/src").unwrap().model,
            "b"
        );
        assert_eq!(
            folder_default(&list, "C:/Users/me/repo2").unwrap().model,
            "c"
        );
        assert_eq!(
            folder_default(&list, "C:/Users/me/repository")
                .unwrap()
                .model,
            "a"
        );
        assert!(folder_default(&list, "D:/other").is_none());
    }
}
