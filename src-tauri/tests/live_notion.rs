//! Opt-in dry run against the real vault: `cargo test --test live_notion -- --ignored --nocapture`

use agentos_studio_lib::notion;
use agentos_studio_lib::settings;
use agentos_studio_lib::vault::Vault;

#[test]
#[ignore = "writes pages to the real Notion workspace"]
fn publish_really_creates_the_pages() {
    let v = Vault::new(settings::vault_path());
    let report = notion::publish(&v, false);
    println!("connected: {} dry_run: {}", report.connected, report.dry_run);
    for p in &report.pages {
        println!("  {:<28} {}", p.title, p.action);
    }
    assert!(report.connected, "token or parent page missing");
    assert!(!report.dry_run);
    assert!(report.error.is_none(), "{:?}", report.error);
    assert!(!report.pages.is_empty());
}

#[test]
#[ignore = "reads the real vault"]
fn dry_run_reports_what_would_change() {
    let v = Vault::new(settings::vault_path());
    let report = notion::publish(&v, true);
    println!("connected: {}", report.connected);
    println!("dry run: {}", report.dry_run);
    for p in &report.pages {
        println!("  {:<28} {:<24} {}", p.title, p.action, &p.hash[..8]);
    }
    assert!(report.dry_run, "a dry run never sends");
    assert!(!report.pages.is_empty());
    assert!(report.pages.iter().all(|p| p.action.contains("dry run")));
    assert!(report.error.is_none(), "{:?}", report.error);
}
