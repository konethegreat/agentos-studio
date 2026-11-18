//! Live S1 canary check against a throwaway Hermes profile.
//! Hold the live-machine lock (`04_Agent_Engine/locks/live-machine.lock`) first and delete it after.
//! Run: cargo test --test live -- live_canary:: --ignored --nocapture --test-threads 1

use std::process::Command;

use agentos_studio_lib::{canary, model_layer::Route, quota_grid, store};

struct ProfileGuard(String);

impl Drop for ProfileGuard {
    fn drop(&mut self) {
        let _ = Command::new("hermes")
            .args(["profile", "delete", "-y", &self.0])
            .output();
        std::env::remove_var("AGENTOS_STUDIO_DB");
    }
}

fn run_hermes(args: &[&str]) {
    let out = Command::new("hermes")
        .args(args)
        .output()
        .expect("run hermes");
    assert!(
        out.status.success(),
        "hermes {:?} failed: {}\n{}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "creates a throwaway Hermes profile and runs live probes"]
fn canary_closes_unreachable_lane_and_reopens_when_restored() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("studio.db");
    std::env::set_var("AGENTOS_STUDIO_DB", &db_path);
    let c = store::open().expect("open temp studio.db");

    let routes = quota_grid::routes();
    let lane = routes
        .iter()
        .find(|r| r.provider == "antigravity-subscription-directsdk")
        .cloned()
        .expect("antigravity route configured");

    let prof = format!("canary{}", store::now());
    let _guard = ProfileGuard(prof.clone());
    run_hermes(&["profile", "create", &prof, "--clone", "--no-alias"]);

    // `hermes -p <p> plugins disable antigravity-subscription-directsdk` still lets
    // `-z --provider antigravity-subscription-directsdk` answer, so probe an unreachable model
    // on the throwaway profile to trigger a real fallback mismatch against `lane`.
    let unreachable = Route {
        provider: lane.provider.clone(),
        model: "no-such-model-xyz".into(),
        base_url: None,
    };

    let p1 = canary::run_probe_in(&unreachable, tmp.path(), Some(&prof));
    let s1 = canary::record_in(&c, &lane, &p1, store::now(), false);
    println!(
        "probe 1 (unreachable): outcome={} fails={} closed={} detail={}",
        s1.outcome, s1.fails, s1.closed, s1.detail
    );
    assert!(canary::is_failure(&s1.outcome), "expected failure: {s1:?}");
    assert!(!s1.closed);

    let p2 = canary::run_probe_in(&unreachable, tmp.path(), Some(&prof));
    let s2 = canary::record_in(&c, &lane, &p2, store::now(), false);
    println!(
        "probe 2 (unreachable): outcome={} fails={} closed={} detail={}",
        s2.outcome, s2.fails, s2.closed, s2.detail
    );
    assert!(s2.closed, "expected lane closed after two failures: {s2:?}");

    let lanes = quota_grid::lanes(false);
    let closed_lane = lanes
        .iter()
        .find(|l| l.provider == lane.provider && l.model == lane.model)
        .expect("lane in quota_grid");
    assert!(!closed_lane.open, "quota_grid should close the lane");
    let steered = quota_grid::steer(&lanes, store::now());
    println!("steer while closed: {steered:?}");
    match &steered {
        quota_grid::Steer::Primary => panic!("steer chose closed primary"),
        quota_grid::Steer::Pin {
            provider, model, ..
        } => {
            assert!(!(provider == &lane.provider && model == &lane.model));
        }
        quota_grid::Steer::Hold { .. } => {}
    }

    let p3 = canary::run_probe_in(&lane, tmp.path(), Some(&prof));
    let s3 = canary::record_in(&c, &lane, &p3, store::now(), false);
    println!(
        "probe 3 (restored): outcome={} fails={} closed={} detail={}",
        s3.outcome, s3.fails, s3.closed, s3.detail
    );
    assert!(!s3.closed, "expected lane reopened: {s3:?}");
    assert!(!canary::is_failure(&s3.outcome), "expected pass: {s3:?}");
    assert!(
        canary::closed_reason_in(&c, &lane.provider, &lane.model).is_none(),
        "closed_reason_in should be None after reopen"
    );
}
