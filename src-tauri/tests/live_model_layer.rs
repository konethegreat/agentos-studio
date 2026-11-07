//! The model layer against the real Hermes install. Reads credentials and MoA presets, and round
//! trips the fallback chain: config.yaml must be byte-identical afterwards.
//! Run: cargo test --test live_model_layer -- --ignored --nocapture
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
    let after = std::fs::read(&cfg).unwrap();
    if after != before {
        std::fs::write(&cfg, &before).unwrap();
        panic!("config.yaml changed after a set and restore; restored from the copy");
    }
    assert_eq!(read, probe);
}
