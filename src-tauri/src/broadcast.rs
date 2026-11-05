//! Save & Broadcast: persist the constitution and push it to the vault templates,
//! global user rule files, and every local repository.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use crate::constitution::{self, Kind};
use crate::fleet;
use crate::settings::{home, Settings};
use crate::vault::Vault;

#[derive(Debug, Serialize)]
pub struct BroadcastReport {
    pub dashes_replaced: bool,
    pub vault: Vec<(String, String)>,
    pub global: Vec<(String, String)>,
    pub repos: Vec<fleet::SyncResult>,
}

pub fn global_targets() -> Vec<(PathBuf, Kind)> {
    let h = home();
    vec![
        (h.join(".claude").join("CLAUDE.md"), Kind::Claude),
        (h.join(".qoder").join("AGENTS.md"), Kind::Agents),
        (h.join(".gemini").join("GEMINI.md"), Kind::Gemini),
    ]
}

pub fn save_and_broadcast(
    vault: &Vault,
    settings: &Settings,
    constitution_raw: &str,
    system_context: &str,
    include_repos: bool,
) -> Result<BroadcastReport> {
    let dash = constitution::dash_re();
    let dashes_replaced = dash.is_match(constitution_raw) || dash.is_match(system_context);
    let raw = constitution::undash(&constitution_raw.replace("\r\n", "\n"));
    let sys = constitution::undash(&system_context.replace("\r\n", "\n"));

    let mut vault_actions = Vec::new();
    let cpath = constitution::constitution_path(&vault.root);
    vault_actions.push((
        cpath.display().to_string(),
        constitution::write_owned(&cpath, &raw)?.to_string(),
    ));

    let sys_path = vault.memory().join("SYSTEM_CONTEXT.md");
    fs::create_dir_all(vault.memory())?;
    fs::write(&sys_path, &sys)?;
    vault_actions.push((
        sys_path.display().to_string(),
        constitution::upsert_block(&sys_path, &constitution::managed_block(&raw, Kind::System))?
            .to_string(),
    ));

    let templates = cpath
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| vault.root.join("04_Agent_Engine").join("templates"));
    for kind in Kind::REPO {
        let p = templates.join(kind.file_name());
        vault_actions.push((
            p.display().to_string(),
            constitution::write_owned(&p, &(constitution::managed_block(&raw, kind) + "\n"))?
                .to_string(),
        ));
    }
    for (name, content) in [
        (".cursorrules", constitution::cursorrules(&raw)),
        ("AGENT_PROTOCOL.md", constitution::rule_file(&raw)),
        ("rules/agentos.md", constitution::rule_file(&raw)),
    ] {
        let p = templates.join(name);
        vault_actions.push((
            p.display().to_string(),
            constitution::write_owned(&p, &content)?.to_string(),
        ));
    }

    let mut global = Vec::new();
    for (path, kind) in global_targets() {
        global.push((
            path.display().to_string(),
            constitution::upsert_block(&path, &constitution::managed_block(&raw, kind))?
                .to_string(),
        ));
    }

    let repos = if include_repos {
        let paths: Vec<String> = fleet::discover(&settings.roots())
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        fleet::sync_fleet_rules(&paths, false, &raw)
    } else {
        Vec::new()
    };
    vault.write_shared_memory(
        "Constitution Broadcasts",
        &format!(
            "Constitution saved and broadcast from AgentOS Studio. Vault files: {}, global user files: {}, repositories synced: {}.",
            vault_actions.len(),
            global.len(),
            repos.len()
        ),
        "convention",
    )?;
    Ok(BroadcastReport {
        dashes_replaced,
        vault: vault_actions,
        global,
        repos,
    })
}
