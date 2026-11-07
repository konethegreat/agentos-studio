//! Read-only check against this machine's agent configs: which agents an app update would move
//! to the new staged hub. Run: cargo test --test live_hub_autoupdate -- --ignored --nocapture
use agentos_studio_lib::mcp_manager::{
    all_sources, hub_binary_for, hub_root, managed_outdated_agents,
};

#[test]
#[ignore]
fn agents_an_update_would_move() {
    let root = hub_root();
    let want = hub_binary_for(&root, env!("CARGO_PKG_VERSION"));
    let moved = managed_outdated_agents(&all_sources(), &root, &want);
    println!(
        "version {} would move: {moved:?}",
        env!("CARGO_PKG_VERSION")
    );
}
