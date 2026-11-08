//! The router against the real studio.db (read only): what it would pin per role, or why not.
//! Run: cargo test --test live_router -- --ignored --nocapture
use agentos_studio_lib::{missions, router};

#[test]
#[ignore]
fn router_on_real_history() {
    let all = missions::list(500).expect("missions");
    println!("missions: {}", all.len());
    for role in missions::ROLES {
        match router::pick(&all, role, chrono::Utc::now().timestamp()) {
            Ok(s) => println!("{role}: pin {}:{} ({})", s.provider, s.model, s.reason),
            Err(why) => println!("{role}: declines, {why}"),
        }
    }
    let pin = router::resolve_pin(
        "",
        "",
        None,
        Some(router::pick(
            &all,
            "implementer",
            chrono::Utc::now().timestamp(),
        )),
    );
    println!("a new blank implementer mission: {pin:?}");
    println!(
        "shadow: {}",
        router::shadow_line(&router::pick(
            &all,
            "implementer",
            chrono::Utc::now().timestamp()
        ))
    );
    assert!(pin.source == "router" || pin.source == "inherit");
}
