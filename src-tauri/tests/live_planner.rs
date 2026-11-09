//! Live: a goal is planned by Hermes' own decomposer into missions, and nothing in the plan
//! reaches a Hermes board where the gateway's dispatcher would run it.
//!
//! Opt-in, it makes one auxiliary model call through Hermes:
//!   cargo test --test live_planner -- --ignored --nocapture --test-threads 1
//!
//! Uses a temporary studio.db (AGENTOS_STUDIO_DB), so the real board and the running app never
//! see the goal, and a temporary vault for the goal note.

use agentos_studio_lib::{goals, planner, vault::Vault};

fn board_task_counts() -> String {
    let out = std::process::Command::new("hermes")
        .args(["kanban", "boards", "list"])
        .output()
        .expect("hermes kanban boards list");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
#[ignore]
fn a_goal_is_planned_into_missions_without_touching_a_hermes_board() {
    let db = tempfile::tempdir().unwrap();
    std::env::set_var("AGENTOS_STUDIO_DB", db.path().join("studio.db"));
    let vdir = tempfile::tempdir().unwrap();
    let v = Vault::new(vdir.path());
    v.scaffold().unwrap();

    let boards_before = board_task_counts();
    let g = goals::create(&goals::NewGoal {
        outcome: "Add a GET /probe endpoint that returns {\"ok\":true}, with a unit test, and document it in README.md".into(),
        repos: vec!["agentos-sandbox-1".into()],
        checks: vec![
            goals::Check {
                kind: "command".into(),
                target: "npm test".into(),
                repo: None,
            },
            goals::Check {
                kind: "file".into(),
                target: "README.md".into(),
                repo: None,
            },
        ],
        budget_usd: 0.0,
        deadline: None,
        autonomy: Some("ask".into()),
    })
    .unwrap();

    let started = std::time::Instant::now();
    let planned = planner::plan(&v, &g.id).expect("the planner plans the goal");
    println!(
        "{} planned in {:.1}s: status {}, note {:?}",
        g.id,
        started.elapsed().as_secs_f64(),
        planned.status,
        planned.note
    );
    let ms = goals::missions_of(&g.id).unwrap();
    for m in &ms {
        println!(
            "  {} repo={} agent={} after={:?}\n    title: {}\n    acceptance: {}",
            m.id,
            m.repo,
            m.agent,
            planner::depends_on(&m.id),
            m.goal.lines().next().unwrap_or(""),
            m.acceptance.lines().next().unwrap_or("")
        );
    }

    assert_eq!(planned.status, "running", "Ask on risk starts at once");
    assert!(
        !ms.is_empty() && ms.len() <= 8,
        "a small plan: {}",
        ms.len()
    );
    for m in &ms {
        assert_eq!(m.status, "queued");
        assert_eq!(m.repo, "agentos-sandbox-1");
        assert_eq!(m.agent, "auto", "assign.rs picks the agent at start");
        assert!(!m.acceptance.trim().is_empty());
        assert!(m.goal.contains(&format!("Part of goal {}", g.id)));
        for d in planner::depends_on(&m.id) {
            assert!(
                ms.iter().any(|x| x.id == d),
                "{} depends inside the plan",
                m.id
            );
        }
    }
    assert!(
        !planner::plan_db(&g.id).exists(),
        "the private kanban database is deleted"
    );
    assert_eq!(
        board_task_counts(),
        boards_before,
        "no Hermes board gained a task"
    );
    let note = std::fs::read_to_string(vdir.path().join(goals::note_rel(&g.id))).unwrap();
    assert!(note.contains("status: running"));

    goals::delete(&g.id).unwrap();
    assert!(goals::get(&g.id).unwrap().is_none());
    std::env::remove_var("AGENTOS_STUDIO_DB");
}
