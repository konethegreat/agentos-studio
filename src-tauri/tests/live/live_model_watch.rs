//! Model watch against the real Hermes cache with a temporary studio.db: the first check seeds and
//! flags nothing; a second check right after reports no changes.
//! Run: cargo test --test live -- live_model_watch:: --ignored --nocapture
use agentos_studio_lib::model_watch;

#[test]
#[ignore]
fn first_check_seeds_quietly() {
    let tmp = std::env::temp_dir().join(format!("mwatch-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    std::env::set_var("AGENTOS_STUDIO_DB", tmp.join("studio.db"));
    let first = model_watch::check().expect("check");
    let second = model_watch::check().expect("check again");
    println!(
        "first: {} new, {} gone; second: {} new, {} gone",
        first.new.len(),
        first.gone.len(),
        second.new.len(),
        second.gone.len()
    );
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(first.new.is_empty() && first.gone.is_empty());
    assert!(second.new.is_empty() && second.gone.is_empty());
}
