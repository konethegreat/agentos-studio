//! Hermes surfaces against the real install. Creates one paused cron job and removes it; reads
//! everything else. Never calls `hermes send` (that would message the user).
//! Run: cargo test --test live -- live_hermes_surfaces:: --ignored --nocapture --test-threads 1
use agentos_studio_lib::hermes_surfaces as hs;

#[test]
#[ignore]
fn hermes_surfaces_live() {
    let m = hs::memory_view();
    println!(
        "memory: MEMORY.md {} chars, USER.md {} chars, status has 'Built-in': {}",
        m.memory.len(),
        m.user.len(),
        m.status.contains("Built-in")
    );
    let nodes = m
        .journey
        .get("nodes")
        .and_then(|n| n.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    println!("journey nodes: {nodes}");
    assert!(m.status.contains("Built-in"), "memory status: {}", m.status);
    assert!(nodes > 0, "journey --json gave no nodes");

    let insights = hs::insights(7).expect("insights");
    println!(
        "insights: {} chars, has Overview: {}",
        insights.len(),
        insights.contains("Overview")
    );
    assert!(insights.contains("Overview"));

    let audit = hs::security_audit().expect("audit");
    println!(
        "audit: scanned {} findings {}",
        audit["total_components_scanned"], audit["finding_count"]
    );
    assert!(audit.get("findings").is_some());

    let profiles = hs::profiles();
    println!(
        "profiles: {:?}",
        profiles
            .iter()
            .map(|p| (&p.name, &p.model))
            .collect::<Vec<_>>()
    );
    assert_eq!(profiles[0].name, "default");

    let before = hs::cron_jobs().len();
    let out = hs::cron_create(&hs::CronSpec {
        schedule: "every 6h".into(),
        prompt: "Reply OK".into(),
        name: Some("studio-live-test".into()),
        paused: true,
        ..Default::default()
    })
    .expect("cron create");
    println!("create: {}", out.lines().next().unwrap_or(""));
    let job = hs::cron_jobs()
        .into_iter()
        .find(|j| j.name.as_deref() == Some("studio-live-test"))
        .expect("job listed from jobs.json");
    assert_eq!(job.state.as_deref(), Some("paused"));
    println!(
        "resume: {}",
        hs::cron_action(&job.id, "resume").expect("resume")
    );
    let resumed = hs::cron_jobs()
        .into_iter()
        .find(|j| j.id == job.id)
        .unwrap();
    println!(
        "state after resume: {:?} next {:?}",
        resumed.state, resumed.next_run_at
    );
    println!(
        "pause: {}",
        hs::cron_action(&job.id, "pause").expect("pause")
    );
    println!(
        "remove: {}",
        hs::cron_action(&job.id, "remove").expect("remove")
    );
    assert_eq!(hs::cron_jobs().len(), before, "test job left behind");
}
