use std::time::Duration;

use agentos_studio_lib::{fleet_doctor, missions::Mission, procs};

#[test]
#[ignore = "requires a provisioned doctor-* profile, model-error fixture task, and live-machine lock"]
fn isolated_diagnostics_produce_a_linked_advisory() {
    // Hermes diagnostics can maintain its database, so the caller must provision an isolated board.
    let profile = std::env::var("AGENTOS_DOCTOR_TEST_PROFILE")
        .expect("AGENTOS_DOCTOR_TEST_PROFILE must name a caller-provisioned throwaway profile");
    assert!(
        profile.starts_with("doctor-")
            && profile.len() > "doctor-".len()
            && profile
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
        "profile must be a doctor-* name, not default or a path"
    );
    let task_id = std::env::var("AGENTOS_DOCTOR_TEST_TASK")
        .expect("AGENTOS_DOCTOR_TEST_TASK must identify the provisioned model-error task");
    assert!(!task_id.trim().is_empty());
    let (code, stdout, stderr) = procs::run_with_timeout(
        "hermes",
        &[
            "-p",
            &profile,
            "kanban",
            "--board",
            "agentos-doctor-test",
            "diagnostics",
            "--json",
        ],
        None,
        None,
        Duration::from_secs(30),
    )
    .expect("read isolated board diagnostics");
    assert_eq!(code, Some(0), "diagnostics failed: {stderr}");
    let diagnostics =
        fleet_doctor::parse_diagnostics(&stdout).expect("parse real Hermes diagnostics");
    assert!(
        diagnostics.iter().any(|d| d.task_id == task_id),
        "fixture task has no diagnostics"
    );
    let mission = Mission {
        id: "M-DOCTOR-TEST".into(),
        repo: "doctor-test".into(),
        agent: "hermes".into(),
        status: "running".into(),
        task_id: Some(format!("kanban:{task_id}")),
        ..Default::default()
    };
    let findings = fleet_doctor::diagnose_tasks(&diagnostics, &[mission]);
    let finding = findings
        .iter()
        .find(|a| a.task_id == task_id)
        .expect("linked fixture finding");
    assert_eq!(finding.action, "model_change_needed", "{finding:?}");
    assert!(finding.detail.contains("no recovery requested"));
}
