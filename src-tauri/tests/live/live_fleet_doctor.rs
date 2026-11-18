use std::time::{SystemTime, UNIX_EPOCH};

use agentos_studio_lib::{canary, missions, model_layer::Route, quota_grid, store, stuck};

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
    assert!(routes.len() >= 2, "expected at least 2 routes in quota_grid");
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

    let m = missions::create_mission(
        None,
        "Doctor starving test",
        "Reply with 42",
        ".",
        ".",
        None,
        "code",
        Some(&bad_lane.provider),
        Some(&bad_lane.model),
    )
    .unwrap();

    let conn = store::conn().unwrap();
    conn.execute("UPDATE missions SET status = 'running' WHERE id = ?1", [&m.id])
        .unwrap();

    let status = stuck::check_now();
    let diag = status
        .diagnoses
        .iter()
        .find(|d| d.mission_id == m.id)
        .expect("expected diagnosis for running mission");
    assert_eq!(diag.diagnosis, "starving_for_quota");

    let updated = missions::get_mission(&m.id).unwrap().unwrap();
    assert_eq!(updated.status, missions::MissionStatus::Queued);
    assert_eq!(updated.provider.as_deref(), Some(healthy_lane.provider.as_str()));

    let routed = Route {
        provider: updated.provider.unwrap(),
        model: updated.model.unwrap(),
        base_url: None,
    };
    let p = canary::run_probe_in(&routed, &tmp_dir, None);
    assert_eq!(p.outcome, canary::Outcome::Ok, "steered lane probe failed: {:?}", p);
}
