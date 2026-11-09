//! Two-way goal sync against the real Notion workspace, with a temporary studio.db and vault.
//! Creates the goals board under NOTION_PARENT_PAGE, plays a human in Notion (a Decision from an
//! untrusted editor, a goal proposal, an incomplete proposal, then a trusted Decision), syncs,
//! checks both sides, then archives the board (recoverable from Notion's trash). The planner is
//! not started (`plan = false`), so no Hermes turn runs.
//! Run: cargo test --test live_notion_goals -- --ignored --nocapture
use agentos_studio_lib::{escalation, goals, notion, notion_goals, notion_sync, vault::Vault};
use serde_json::json;

fn sync(v: &Vault, label: &str) -> notion_goals::SyncReport {
    let r = notion_goals::sync_inner(v, false, notion::token(), notion::parent_id(), false);
    println!("{label}: {r:?}");
    assert!(r.error.is_none(), "{label}: {:?}", r.error);
    r
}

#[test]
#[ignore]
fn goals_round_trip_through_notion() {
    let tmp = std::env::temp_dir().join(format!("notion-goals-{}", std::process::id()));
    std::fs::create_dir_all(tmp.join("vault")).unwrap();
    std::env::set_var("AGENTOS_STUDIO_DB", tmp.join("studio.db"));
    std::env::set_var("AGENTOS_STUDIO_MEMORY_DIR", tmp.join("memory"));
    let v = Vault::new(tmp.join("vault"));
    v.scaffold().unwrap();
    let tok = notion::token().expect("NOTION_API_KEY in the Hermes env");

    let first = goals::create(&goals::NewGoal {
        outcome: "Studio-side goal".into(),
        repos: vec!["demo".into()],
        checks: vec![goals::Check {
            kind: "file".into(),
            target: "README.md".into(),
            repo: None,
        }],
        budget_usd: 1.0,
        deadline: None,
        autonomy: Some("ask".into()),
    })
    .unwrap();
    let r1 = sync(&v, "sync 1");
    let db = r1.database.clone().expect("board created");
    let r2 = sync(&v, "sync 2");

    // Studio asks a question on the goal; the row shows it.
    let (e, _) = escalation::raise(
        Some(&first.id),
        Some("M-001"),
        "merge",
        "Merge M-001 (demo#1)?",
        &["merge", "skip"],
        "skip",
    )
    .unwrap();
    let r3 = sync(&v, "sync 3");
    let (page, _) = notion::ledger_get(&notion_goals::ledger_key(&first.id)).unwrap();
    let shown = notion::request("GET", &format!("/pages/{page}"), &tok, None).unwrap();
    let shown_esc = shown["properties"]["Escalation"]["rich_text"][0]["plain_text"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // A human in Notion, not trusted yet: answer the question, propose a goal, half-propose one.
    let decide = |answer: &str| {
        notion::request(
            "PATCH",
            &format!("/pages/{page}"),
            &tok,
            Some(&json!({ "properties": { "Decision": { "select": { "name": answer } } } })),
        )
        .unwrap();
    };
    decide("merge");
    let row = |goal: &str, repos: &str, checks: &str| {
        json!({ "parent": { "database_id": db }, "properties": {
            "Goal": { "title": [{ "text": { "content": goal } }] },
            "Repos": { "rich_text": [{ "text": { "content": repos } }] },
            "Done checks": { "rich_text": [{ "text": { "content": checks } }] },
            "Autonomy": { "select": { "name": "full" } }
        }})
    };
    let proposal = notion::request(
        "POST",
        "/pages",
        &tok,
        Some(&row("Proposed in Notion", "demo", "file: docs/PROBE.md")),
    )
    .unwrap();
    let pid = proposal["id"].as_str().unwrap().to_string();
    notion::request(
        "POST",
        "/pages",
        &tok,
        Some(&row("Half an idea", "demo", "")),
    )
    .unwrap();
    let r4 = sync(&v, "sync 4");
    let after_untrusted = escalation::get(&e.id).unwrap().unwrap();
    let proposed = goals::list(10)
        .unwrap()
        .into_iter()
        .find(|g| g.outcome == "Proposed in Notion");
    let back = notion::request("GET", &format!("/pages/{pid}"), &tok, None).unwrap();
    let written_id = back["properties"]["ID"]["rich_text"][0]["plain_text"]
        .as_str()
        .unwrap_or("")
        .to_string();
    // A lost ID write-back: the row looks like a fresh proposal again. The inbound ledger knows
    // this page already produced a goal, so the sync repairs the row instead of creating another.
    notion::request(
        "PATCH",
        &format!("/pages/{pid}"),
        &tok,
        Some(&json!({ "properties": { "ID": { "rich_text": [] } } })),
    )
    .unwrap();
    let goals_before = goals::list(50).unwrap().len();
    let r4b = sync(&v, "sync 4b");
    let goals_after = goals::list(50).unwrap().len();
    let repaired = notion::request("GET", &format!("/pages/{pid}"), &tok, None).unwrap();
    let repaired_id = repaired["properties"]["ID"]["rich_text"][0]["plain_text"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let note_written = notion::request("GET", &format!("/pages/{page}"), &tok, None).unwrap();
    let decision_cleared = note_written["properties"]["Decision"]["select"].is_null();

    // Trust the integration (it is the editor of every row this test writes), decide again.
    let me = notion::request("GET", "/users/me", &tok, None).unwrap();
    let bot = me["id"].as_str().unwrap().to_string();
    notion_sync::set_trusted_authors(std::slice::from_ref(&bot)).unwrap();
    decide("retry");
    let r5 = sync(&v, "sync 5");
    decide("merge");
    let r6 = sync(&v, "sync 6");
    let answered = escalation::get(&e.id).unwrap().unwrap();
    let (_, ledger_in) = notion::ledger_counts();
    let r7 = sync(&v, "sync 7");

    // A deleted goal's row is archived.
    let proposed_id = proposed.as_ref().map(|g| g.id.clone()).unwrap_or_default();
    goals::delete(&proposed_id).unwrap();
    let r8 = sync(&v, "sync 8");
    let archived = notion::request("GET", &format!("/pages/{pid}"), &tok, None).unwrap();

    notion_goals::archive_board().unwrap();
    let left = notion::ledger_keys("notion-goal:");
    let _ = std::fs::remove_dir_all(&tmp);

    assert_eq!(r1.created_rows, 1);
    assert_eq!(
        (r2.unchanged_rows, r2.created_rows, r2.updated_rows),
        (1, 0, 0),
        "unchanged costs nothing"
    );
    assert_eq!(r3.updated_rows, 1, "a new question updates the row");
    assert_eq!(shown_esc, e.id, "the row names the open escalation");
    assert!(r4.decided.is_empty());
    assert!(
        r4.declined.iter().any(|d| d.contains("not a trusted")),
        "{:?}",
        r4.declined
    );
    assert_eq!(
        after_untrusted.status, "open",
        "an untrusted Decision is not applied"
    );
    assert!(decision_cleared, "a handled Decision is cleared");
    let proposed = proposed.expect("proposal became a goal");
    assert_eq!(
        proposed.autonomy, "watch",
        "an untrusted proposal is held at Watch whatever it asked for"
    );
    assert!(proposed.note.contains("not a trusted author"));
    assert_eq!(r4.proposed, vec![proposed.id.clone()]);
    assert_eq!(r4.held_at_watch, vec![proposed.id.clone()]);
    assert!(r4b.proposed.is_empty(), "{:?}", r4b.proposed);
    assert_eq!(
        goals_before, goals_after,
        "no second goal from the same row"
    );
    assert_eq!(repaired_id, proposed.id, "the lost ID was written back");
    assert!(
        ledger_in >= 2,
        "the proposal and the Decision are in the inbound ledger"
    );
    assert_eq!(
        written_id, proposed.id,
        "the ID was written back to the row"
    );
    assert!(r4.declined.iter().any(|d| d.contains("Needs Done checks")));
    assert!(
        r5.declined.iter().any(|d| d.contains("not an answer")),
        "{:?}",
        r5.declined
    );
    assert_eq!(r6.decided, vec![format!("{} merge", e.id)]);
    assert_eq!(answered.status, "answered");
    assert_eq!(answered.answer.as_deref(), Some("merge"));
    assert_eq!(answered.answered_by.as_deref(), Some("notion"));
    assert!(r7.decided.is_empty() && r7.proposed.is_empty());
    assert_eq!(r7.created_rows, 0, "settled after one round");
    assert_eq!(r8.archived_rows, 1);
    assert_eq!(archived["archived"], true);
    assert!(
        left.is_empty(),
        "archiving the board forgets its ledger: {left:?}"
    );
}
