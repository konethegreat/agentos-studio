//! Live first run with only Hermes installed. Hermes runs this on the owner's machine after
//! taking the live-machine lock:
//!
//!   cargo test --test live_first_run -- --ignored --nocapture --test-threads 1
//!
//! Every path the boot sequence resolves is redirected into one temp folder, a minimal
//! config.yaml makes Hermes look installed, and the real `boot::run` executes. The owner's
//! vault, settings, agent configs, studio.db, logs and hub folder are never opened or written.

use agentos_studio_lib::boot::{self, Status};
use agentos_studio_lib::settings;
use std::fs;
use std::path::Path;

fn redirect(key: &str, value: &Path) {
    std::env::set_var(key, value);
}

#[test]
#[ignore]
fn a_hermes_only_machine_reaches_a_ready_first_run() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let appdata = d.path().join("appdata");
    let hermes = d.path().join("hermes");
    let vault = d.path().join("AgentOS Vault");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&hermes).unwrap();
    fs::write(
        hermes.join("config.yaml"),
        "model:\n  provider: claude-subscription-directsdk\n  default: claude-sonnet-4-5\nmcp_servers: {}\n",
    )
    .unwrap();

    redirect("USERPROFILE", &home);
    redirect("HOME", &home);
    redirect("APPDATA", &appdata);
    redirect("LOCALAPPDATA", &d.path().join("localappdata"));
    redirect("HERMES_HOME", &hermes);
    redirect("AGENTOS_VAULT", &vault);
    redirect("AGENTOS_STUDIO_DB", &d.path().join("studio.db"));
    redirect("AGENTOS_STUDIO_MEMORY_DIR", &d.path().join("memory"));
    std::env::remove_var("NOTION_TOKEN");
    std::env::remove_var("NOTION_API_KEY");

    println!("temp home: {}", home.display());
    println!("temp vault: {}", vault.display());

    let report = boot::run(None);
    for s in &report.steps {
        println!("{:12} {:?} {}", s.id, s.status, s.detail);
    }
    println!(
        "ready={} ok={} repaired={} warn={} failed={}",
        report.ready,
        report.summary.ok,
        report.summary.repaired,
        report.summary.warn,
        report.summary.failed
    );

    assert_eq!(report.vault, vault.display().to_string());
    let vault_step = report
        .steps
        .iter()
        .find(|s| s.id == "vault")
        .expect("vault step");
    assert_eq!(
        vault_step.status,
        Status::Repaired,
        "first run should create the vault: {}",
        vault_step.detail
    );
    assert!(vault.join("DASHBOARD.md").exists());
    assert!(vault.join("03_Memory").is_dir());
    assert_eq!(
        settings::Settings::load().vault,
        Some(vault.display().to_string()),
        "the created vault should be saved for the next launch"
    );

    // No step fails on this machine, and the missing agents (Claude, Qoder, Antigravity) never
    // make the run unready: they are optional, not broken.
    assert_eq!(
        report.summary.failed,
        0,
        "first run reported a failed step: {:?}",
        report
            .steps
            .iter()
            .filter(|s| s.status == Status::Failed)
            .map(|s| (&s.id, &s.detail))
            .collect::<Vec<_>>()
    );
    assert!(report.ready, "report is not ready");
}
