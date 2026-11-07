//! Opt-in: `cargo test --test live_fork -- --ignored --nocapture --test-threads 1`
//! Forks a real Hermes session through `hermes sessions import`, checks what Hermes stored, and
//! deletes the fork again. Set FORK_SESSION to choose the parent; otherwise the most recent
//! session with at least two assistant replies is used, and FORK_AT_REPLY picks which reply to
//! fork after. The parent is only read.

use agentos_studio_lib::{fork, hermes_engine};

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn parent_session() -> Option<String> {
    if let Ok(id) = std::env::var("FORK_SESSION") {
        return Some(id);
    }
    hermes_engine::list_hermes_sessions(None, 50)
        .ok()?
        .into_iter()
        // A session active in the last ten minutes may be this one, still growing.
        .filter(|s| s.last_activity < now() - 600.0)
        .find(|s| {
            hermes_engine::load_session_messages(&s.id)
                .map(|m| {
                    m.iter()
                        .filter(|x| x.role == "assistant" && !x.content.trim().is_empty())
                        .count()
                        >= 2
                })
                .unwrap_or(false)
        })
        .map(|s| s.id)
}

#[test]
#[ignore = "creates and deletes a real Hermes session"]
fn fork_round_trip() {
    let parent = parent_session().expect("no Hermes session with two replies to fork");
    let before = hermes_engine::load_session_messages(&parent).unwrap();
    // FORK_AT_REPLY picks the reply to fork after (default the second).
    let reply: usize = std::env::var("FORK_AT_REPLY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let at = before
        .iter()
        .filter(|m| m.role == "assistant" && !m.content.trim().is_empty())
        .nth(reply.saturating_sub(1))
        .expect("the parent has fewer replies than FORK_AT_REPLY")
        .id;
    let plan = fork::plan(&before, at).unwrap();
    println!(
        "parent {parent}: {} messages, fork at {at}, {} turns planned",
        before.len(),
        plan.turns.len()
    );

    let r = fork::fork(&parent, at).expect("fork");
    let child = r
        .session_id
        .clone()
        .expect("a fork after a reply creates a session");
    println!(
        "fork {child}: title={:?} turns={} cwd={:?}",
        r.title, r.turns, r.cwd
    );

    let stored = hermes_engine::load_session_messages(&child).unwrap();
    let cleanup = || {
        hermes_engine::delete_session(&child).expect("delete fork");
        let c = rusqlite::Connection::open(agentos_studio_lib::store::db_path()).unwrap();
        c.execute(
            "DELETE FROM session_links WHERE child_session_id = ?1",
            [&child],
        )
        .unwrap();
    };
    let ok = || {
        assert_eq!(r.turns, plan.turns.len());
        assert_eq!(
            stored.len(),
            plan.turns.len(),
            "Hermes stored a different turn count"
        );
        for (s, t) in stored.iter().zip(&plan.turns) {
            assert_eq!(s.role, t.role);
            assert_eq!(s.content.trim(), t.content.trim());
        }
        assert!(r.title.starts_with("Fork of"), "title {:?}", r.title);
        let listed = hermes_engine::list_hermes_sessions(Some(&child), 5).unwrap();
        assert!(
            listed.iter().any(|s| s.id == child),
            "fork missing from the session list"
        );
        assert!(agentos_studio_lib::projects::children_of(&parent).contains(&child));
        let after = hermes_engine::load_session_messages(&parent).unwrap();
        assert_eq!(after.len(), before.len(), "the parent must not change");
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(ok));
    cleanup();
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
    println!("fork verified and removed");
}
