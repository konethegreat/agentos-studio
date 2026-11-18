use std::time::{SystemTime, UNIX_EPOCH};

use agentos_studio_lib::{canary, fleet_doctor, missions, model_layer::Route, quota_grid, store};

#[test]
#[ignore]
fn fleet_doctor_catches_starving_mission_and_steers_to_healthy_lane() {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let tmp_dir = std::env::temp_dir().join(format!("agentos_live_doctor_{ts}"));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let db_path = tmp_dir.join("studio.db");
    store::set_test_db(Some(db_path));

    let routes = quota_grid::routes();
    assert!(
        routes.len() >= 2,
        "expected at least 2 routes in quota_grid"
    );
    let bad_lane = &routes[0];
    let healthy_lane = &routes[1];

    // Close bad_lane via 2 failed canary records.
    let fail_probe = canary::Probe {
        outcome: canary::Outcome::NoReply,
        duration_ms: 100,
        detail: "simulated unreachable".into(),
        actual_model: None,
    };
    for _ in 0..2 {
        let _ = canary::record_in(bad_lane, &fail_probe, false);
    }

    let m = missions::create(&missions::NewMission {
        repo: "agentos-studio".into(),
        title: "Doctor starving test".into(),
        owner_agent: "hermes".into(),
        provider: bad_lane.provider.clone(),
        model: bad_lane.model.clone(),
        branch: "main".into(),
        kanban_task: Some("KT-LIVE-1".into()),
        goal_id: None,
    })
    .unwrap();

    let show_json = format!(
        r#"{{"status":"blocked","runs":[{{"error":"model not found 404","metadata":{{"provider":"{}","model":"{}"}}}},{{"error":"model not found 404","metadata":{{"provider":"{}","model":"{}"}}}}]}}"#,
        bad_lane.provider, bad_lane.model, bad_lane.provider, bad_lane.model
    );
    let view = kanban::parse_show(&show_json).unwrap();
    let diag = fleet_doctor::diag_from_task_view("KT-LIVE-1", &view)
        .expect("expected repeated_crashes diag");
    let lanes = quota_grid::lanes(false);
    let mut repinned_to = None;
    let actions = fleet_doctor::act_on_diagnostics_with(
        &[diag],
        &[m],
        &lanes,
        |_id, prov, model| {
            repinned_to = Some((prov.to_string(), model.to_string()));
            Ok(())
        },
        |_id, _reason| Ok(()),
        |_id| Ok(()),
        |_prov| "ok".into(),
    );
    assert_eq!(actions[0].action, "repinned_and_reclaimed");
    let (prov, model) = repinned_to.expect("expected repin to open lane");
    assert_ne!(prov, bad_lane.provider);

    let routed = Route {
        provider: prov,
        model,
        base_url: None,
    };
    let p = canary::run_probe_in(&routed, &tmp_dir, None);
    assert_eq!(
        p.outcome,
        canary::Outcome::Ok,
        "steered lane probe failed: {:?} (healthy={})",
        p,
        healthy_lane.provider
    );
    store::set_test_db(None);
}
