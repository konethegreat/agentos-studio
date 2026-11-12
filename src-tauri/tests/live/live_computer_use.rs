//! Computer use against the real Hermes install, read only: toolset state, the cua-driver,
//! recent actions from state.db, and one screenshot from Hermes' image cache.
//! Run: cargo test --test live -- live_computer_use:: --ignored --nocapture
use agentos_studio_lib::computer_use;

#[test]
#[ignore]
fn computer_use_read_only() {
    let s = computer_use::status();
    println!("status: {s:?}");
    assert!(s.toolset_enabled.is_some(), "hermes tools list was read");
    assert!(
        s.driver_path.is_some() && s.driver_version.is_some(),
        "cua-driver found"
    );
    let acts = computer_use::activity(20).expect("activity");
    for a in acts.iter().take(6) {
        println!(
            "{:.0} {} {:?} {}",
            a.at,
            a.action,
            a.ok,
            a.summary.chars().take(80).collect::<String>()
        );
    }
    if let Some(p) = acts
        .iter()
        .filter_map(|a| a.screenshot.clone())
        .find(|p| std::path::Path::new(p).exists())
    {
        let url = computer_use::screenshot(&p).expect("screenshot");
        println!("screenshot {p}: {} bytes as a data URL", url.len());
        assert!(url.starts_with("data:image/"));
    } else {
        println!("no screenshot on disk among the recent actions");
    }
    assert!(computer_use::screenshot("C:/Windows/win.ini").is_err());
}
