//! Persistent app settings (vault path, fleet scan roots).

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub vault: Option<String>,
    #[serde(default)]
    pub scan_roots: Vec<String>,
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn settings_file() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(home)
        .join("agentos-studio")
        .join("settings.json")
}

impl Settings {
    pub fn load() -> Settings {
        fs::read_to_string(settings_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let f = settings_file();
        if let Some(p) = f.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(f, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Configured vault, else `AGENTOS_VAULT`, else the Hermes vault if present, else ~/Documents/AgentOS Vault.
    pub fn vault_path(&self) -> PathBuf {
        if let Some(v) = self.vault.as_ref().filter(|v| !v.trim().is_empty()) {
            return PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("AGENTOS_VAULT") {
            if !v.trim().is_empty() {
                return PathBuf::from(v);
            }
        }
        let hermes_vault = home().join("Obsidian").join("Hermes Obsidian vault");
        if hermes_vault.is_dir() {
            return hermes_vault;
        }
        dirs::document_dir()
            .unwrap_or_else(home)
            .join("AgentOS Vault")
    }

    /// Configured roots, else home plus the real Desktop/Documents (OneDrive-redirected or not).
    pub fn roots(&self) -> Vec<PathBuf> {
        if !self.scan_roots.is_empty() {
            return self.scan_roots.iter().map(PathBuf::from).collect();
        }
        let h = home();
        let mut roots = vec![h.clone()];
        for extra in [
            h.join("OneDrive").join("Desktop"),
            h.join("OneDrive").join("Documents"),
            h.join("Desktop"),
            h.join("Documents"),
        ] {
            if extra.is_dir() && !roots.contains(&extra) {
                roots.push(extra);
            }
        }
        roots
    }
}
