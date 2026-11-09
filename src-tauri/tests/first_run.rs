//! First run on a machine that is not the owner's: no vault, no Notion, no Qoder and no
//! Antigravity. Every path the checks read or write is pointed at one temp folder, and nothing
//! here starts Hermes, so this test is safe to run beside a live fleet.

use agentos_studio_lib::boot::{self, Status};
use agentos_studio_lib::bootstrapper::{self, Component};
use agentos_studio_lib::settings;
use std::fs;
use std::path::Path;

fn redirect(key: &str, value: &Path) {
    std::env::set_var(key, value);
}

#[test]
fn a_first_run_without_the_fleet_creates_a_vault_and_leaves_missing_agents_optional() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let appdata = d.path().join("appdata");
    let local = d.path().join("localappdata");
    let hermes = d.path().join("hermes");
    let vault = d.path().join("AgentOS Vault");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&hermes).unwrap();

    redirect("USERPROFILE", &home);
    redirect("HOME", &home);
    redirect("APPDATA", &appdata);
    redirect("LOCALAPPDATA", &local);
    redirect("HERMES_HOME", &hermes);
    redirect("AGENTOS_VAULT", &vault);
    redirect("AGENTOS_STUDIO_DB", &d.path().join("studio.db"));
    redirect("AGENTOS_STUDIO_MEMORY_DIR", &d.path().join("memory"));
    std::env::remove_var("NOTION_TOKEN");
    std::env::remove_var("NOTION_API_KEY");

    // Nothing of the owner's fleet is reachable through the redirected home.
    assert_eq!(settings::home(), home);
    assert!(!bootstrapper::agent_present("qoder"));
    assert!(!bootstrapper::agent_present("antigravity"));
    assert!(!hermes.join("mcp-tokens").join("notion.json").exists());
    assert!(!settings::Settings::load().vault_path().exists());

    // Onboarding resolves the vault, creates it, and saves it for the next launch.
    let resolved = settings::Settings::load().vault_path();
    assert_eq!(resolved, vault);
    let (status, detail) = boot::ensure_vault(&resolved, None);
    assert_eq!(status, Status::Repaired, "{detail}");
    assert!(detail.contains("saved it as the vault"), "{detail}");
    for rel in [
        "DASHBOARD.md",
        "00_Inbox",
        "02_Agent_Bus/Pending",
        "02_Agent_Bus/Completed",
        "03_Memory/DECISION_LOG.md",
    ] {
        assert!(vault.join(rel).exists(), "{rel} missing after the scaffold");
    }
    assert_eq!(
        settings::Settings::load().vault,
        Some(vault.display().to_string())
    );
    assert!(appdata
        .join("agentos-studio")
        .join("settings.json")
        .exists());

    // The second launch finds a complete vault and changes nothing.
    let (status, _) = boot::ensure_vault(&resolved, Some(&vault.display().to_string()));
    assert_eq!(status, Status::Ok);

    // A folder the user picked and saved is reported, never created behind their back.
    let picked = d.path().join("unmounted").join("Vault");
    let (status, detail) = boot::ensure_vault(&picked, Some(&picked.display().to_string()));
    assert_eq!(status, Status::Warn);
    assert!(detail.ends_with("does not exist"), "{detail}");
    assert!(!picked.exists());

    // An agent that is not installed, and Notion, are optional. This mirrors the pillar rule the
    // Setup screen uses: a component that is missing on purpose never reddens its pillar.
    let missing_qoder = bootstrapper::optional_when_absent(
        Component {
            id: "mcp-qoder".into(),
            label: "Qoder".into(),
            pillar: 6,
            ok: false,
            detail: "Qoder is not installed".into(),
            action: "Wire Config".into(),
            optional: false,
        },
        bootstrapper::agent_present("qoder"),
    );
    assert!(missing_qoder.optional);
    assert!(missing_qoder.detail.ends_with("(optional)"));
    let missing_antigravity = bootstrapper::optional_when_absent(
        Component {
            id: "mcp-antigravity".into(),
            label: "Antigravity".into(),
            pillar: 6,
            ok: false,
            detail: "Antigravity is not installed".into(),
            action: "Wire Config".into(),
            optional: false,
        },
        bootstrapper::agent_present("antigravity"),
    );
    let notion = Component {
        id: "notion".into(),
        label: "Notion MCP (via Hermes, optional)".into(),
        pillar: 4,
        ok: false,
        detail: "Not configured".into(),
        action: "Connect".into(),
        optional: true,
    };
    let ide_fleet = [missing_qoder, missing_antigravity];
    assert!(ide_fleet.iter().all(|c| c.ok || c.optional));
    assert!(notion.ok || notion.optional);
}
