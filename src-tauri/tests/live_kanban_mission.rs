//! A Hermes mission end to end on Hermes' kanban board, against the real install:
//! `missions::start` puts it on the `agentos-studio` board, the gateway's dispatcher runs the
//! worker, and the scheduler tick moves it to Studio review (never done) with `hermes verify` and
//! the metered spend recorded. Uses a scratch repo registered as a temporary Hermes project and a
//! temporary studio.db; archives the project afterwards.
//! Run: cargo test --test live_kanban_mission -- --ignored --nocapture --test-threads 1
use agentos_studio_lib::{automation, kanban, missions, vault::Vault};
use std::process::Command;
use std::time::{Duration, Instant};

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=studio@test", "-c", "user.name=studio"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
#[ignore]
fn hermes_mission_runs_on_kanban_and_stops_at_studio_review() {
    let tmp = std::env::temp_dir().join(format!("kanban-live-{}", std::process::id()));
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::env::set_var("AGENTOS_STUDIO_DB", tmp.join("studio.db"));
    std::fs::write(
        repo.join("package.json"),
        r#"{"name":"kanban-live","version":"1.0.0","scripts":{"build":"node -e \"console.log('built')\"","test":"node -e \"console.log('tested')\""}}"#,
    )
    .unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "init"]);

    let slug = format!("kanban-live-{}", std::process::id());
    let created = Command::new("hermes")
        .args([
            "project",
            "create",
            &slug,
            &repo.display().to_string(),
            "--slug",
            &slug,
        ])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let v = Vault::new(tmp.join("vault"));
    v.scaffold().unwrap();
    let m = missions::create(
        &slug,
        "Create a file NOTE.md containing the single word ready, commit it on the current branch, then request review. There is no GitHub remote, so do not open a pull request.",
        "NOTE.md exists, contains ready, and is committed",
        "implementer",
        "hermes",
        "claude-subscription-directsdk-experimental",
        "claude-haiku-5-5[1m]",
        1.0,
    )
    .unwrap();
    let started = missions::start(&v, &m.id).expect("start on kanban");
    let task = started.task_id.clone().unwrap();
    println!("mission {} -> {task}", m.id);
    assert!(kanban::is_kanban(Some(&task)));

    let end = Instant::now() + Duration::from_secs(480);
    let mut last = String::new();
    while Instant::now() < end {
        std::thread::sleep(Duration::from_secs(15));
        let report = automation::tick(&v);
        let now = missions::get(&m.id).unwrap().unwrap();
        let k = kanban::show(&task).map(|t| t.status).unwrap_or_default();
        let line = format!("mission {} kanban {k} err {:?}", now.status, report.error);
        if line != last {
            println!("{line}");
            last = line;
        }
        if now.status != "running" {
            break;
        }
    }
    let after = missions::get(&m.id).unwrap().unwrap();
    println!(
        "final: status {} spent ${:.4} verify {}",
        after.status,
        after.spent_usd,
        after
            .verify
            .as_deref()
            .unwrap_or("none")
            .chars()
            .take(300)
            .collect::<String>()
    );
    let note = std::fs::read_to_string(repo.join("NOTE.md")).unwrap_or_default();
    println!("NOTE.md: {:?}", note.trim());

    let _ = Command::new("hermes")
        .args(["project", "archive", &slug])
        .output();
    let _ = Command::new("hermes")
        .args([
            "kanban",
            "--board",
            kanban::BOARD,
            "archive",
            task.trim_start_matches(kanban::PREFIX),
        ])
        .output();

    assert_eq!(
        after.status, "review",
        "finished Hermes work waits for Studio's reviewer"
    );
    assert_ne!(
        after.status, "done",
        "kanban's own review is never Studio approval"
    );
    assert!(
        after
            .verify
            .as_deref()
            .is_some_and(|j| j.contains("\"ok\": true") || j.contains("\"ok\":true")),
        "hermes verify ran and passed"
    );
    assert_eq!(
        after.spent_usd, 0.0,
        "a subscription costs nothing against the budget"
    );
    assert_eq!(note.trim(), "ready");
}
