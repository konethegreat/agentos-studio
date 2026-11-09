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

/// The user's home folder. `USERPROFILE` comes first on Windows for the same reason as
/// `APPDATA` below: the known-folder call ignores a redirected environment, so a test or a
/// portable install can move every agent config and vault lookup under one temp folder.
pub fn home() -> PathBuf {
    if cfg!(windows) {
        if let Ok(h) = std::env::var("USERPROFILE") {
            if !h.trim().is_empty() {
                return PathBuf::from(h);
            }
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// The app config folder. `APPDATA` comes first on Windows because the known-folder call ignores
/// a redirected environment, so a test or a portable install can point the whole config at a temp
/// folder with one variable and this app then writes nothing outside it.
pub fn app_config_dir() -> PathBuf {
    if cfg!(windows) {
        if let Ok(a) = std::env::var("APPDATA") {
            if !a.trim().is_empty() {
                return PathBuf::from(a).join("agentos-studio");
            }
        }
    }
    dirs::config_dir()
        .unwrap_or_else(home)
        .join("agentos-studio")
}

fn settings_file() -> PathBuf {
    app_config_dir().join("settings.json")
}

/// The configured vault, resolved without an AppState. Used by background work and tests.
pub fn vault_path() -> PathBuf {
    Settings::load().vault_path()
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
        // The default profile's vault, adopted only when it already exists, so an existing
        // install keeps its notes. A stranger never has it, so first run lands on the folder
        // below and boot creates it.
        let existing = home().join("Obsidian").join("Hermes Obsidian vault");
        if existing.is_dir() {
            return existing;
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
