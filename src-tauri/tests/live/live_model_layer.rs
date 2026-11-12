//! The model layer against the real Hermes install. Reads credentials and MoA presets, and round
//! trips the fallback chain: config.yaml must be byte-identical afterwards.
//! Run: cargo test --test live -- live_model_layer:: --ignored --nocapture
use agentos_studio_lib::model_layer as ml;

#[test]
#[ignore]
fn model_layer_live() {
    let cfg = dirs::data_local_dir()
        .unwrap()
        .join("hermes")
        .join("config.yaml");
    let before = std::fs::read(&cfg).unwrap();

    let creds = ml::credentials().expect("hermes auth list");
    println!(
        "credentials: {:?}",
        creds
            .iter()
            .map(|c| (&c.provider, &c.kind, c.priority, c.current))
            .collect::<Vec<_>>()
    );
    assert!(!creds.is_empty());
    assert!(creds
        .iter()
        .all(|c| !c.id.is_empty() && !c.provider.is_empty()));

    let moa = ml::moa();
    println!(
        "moa: active {:?}, presets {:?}",
        moa.active_preset,
        moa.presets
            .iter()
            .map(|p| (&p.name, p.reference_models.len()))
            .collect::<Vec<_>>()
    );

    let original = ml::fallback_chain();
    println!("fallback chain before: {original:?}");
    let probe = vec![ml::Route {
        provider: "openrouter".into(),
        model: "deepseek/deepseek-v4.1-flash".into(),
        base_url: None,
    }];
    ml::set_fallback_chain(&probe).expect("set");
    let read = ml::fallback_chain();
    println!("fallback chain after set: {read:?}");
    ml::set_fallback_chain(&original).expect("restore");

    // An alias Hermes resolves on a real turn (flat-rate subscription, so no spend).
    let name = format!("studio-probe-{}", std::process::id());
    ml::set_alias(
        &name,
        "claude-haiku-5-5[1m]",
        "claude-subscription-directsdk-experimental",
        "",
    )
    .expect("set alias");
    let listed = ml::aliases();
    println!(
        "aliases: {:?}",
        listed
            .iter()
            .map(|a| (&a.name, &a.model))
            .collect::<Vec<_>>()
    );
    let turn = std::process::Command::new("hermes")
        .args([
            "chat",
            "-q",
            "Reply with the single word ok.",
            "-m",
            &name,
            "--format",
            "stream-json",
            "--source",
            "gui",
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&turn.stdout);
    let sid = out
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| {
            v.get("session_id")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
        });
    let model_line = out
        .lines()
        .find(|l| l.contains("\"system\""))
        .unwrap_or("")
        .to_string();
    println!(
        "alias turn exit {:?}: {}",
        turn.status.code(),
        model_line.chars().take(200).collect::<String>()
    );
    let stored: String = sid
        .as_ref()
        .and_then(|s| {
            let db = dirs::data_local_dir()?.join("hermes").join("state.db");
            let c = rusqlite::Connection::open_with_flags(
                db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .ok()?;
            c.query_row("SELECT model FROM sessions WHERE id = ?1", [s], |r| {
                r.get(0)
            })
            .ok()
        })
        .unwrap_or_default();
    println!("stored model for the alias turn: {stored}");
    if let Some(s) = sid {
        let _ = std::process::Command::new("hermes")
            .args(["sessions", "delete", &s, "--yes"])
            .output();
    }
    ml::remove_alias(&name).expect("remove alias");
    assert!(listed
        .iter()
        .any(|a| a.name == name && a.model == "claude-haiku-5-5[1m]"));
    assert!(turn.status.success(), "hermes resolved -m {name}");
    assert!(
        stored.contains("claude-haiku-5-5"),
        "turn ran on the aliased model: {stored} ({model_line})"
    );
    let after = std::fs::read(&cfg).unwrap();
    if after != before {
        std::fs::write(&cfg, &before).unwrap();
        panic!("config.yaml changed after a set and restore; restored from the copy");
    }
    assert_eq!(read, probe);
}
