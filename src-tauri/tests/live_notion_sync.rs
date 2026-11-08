//! Two-way mission sync against the real Notion workspace, with a temporary studio.db. Creates the
//! board under NOTION_PARENT_PAGE, plays a human in Notion (a proposal, an incomplete proposal, a
//! cancel), syncs, checks both sides, then archives the board (recoverable from Notion's trash).
//! Run: cargo test --test live_notion_sync -- --ignored --nocapture
use agentos_studio_lib::{missions, notion, notion_sync};
use serde_json::json;

#[test]
#[ignore]
fn missions_round_trip_through_notion() {
    let tmp = std::env::temp_dir().join(format!("notion-sync-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    std::env::set_var("AGENTOS_STUDIO_DB", tmp.join("studio.db"));
    let tok = notion::token().expect("NOTION_API_KEY in the Hermes env");

    let first = missions::create(
        "demo",
        "Studio-side mission",
        "it exists",
        "implementer",
        "claude-code",
        "",
        "",
        0.0,
    )
    .unwrap();
    let r1 = notion_sync::sync(false);
    println!("sync 1: {r1:?}");
    assert!(r1.error.is_none(), "{:?}", r1.error);
    let db = r1.database.clone().expect("board created");
    assert_eq!(r1.created_rows, 1);

    let r2 = notion_sync::sync(false);
    println!("sync 2: {r2:?}");
    assert_eq!(
        (r2.unchanged_rows, r2.created_rows, r2.updated_rows),
        (1, 0, 0),
        "unchanged costs nothing"
    );

    // A human in Notion: propose one mission, half-propose another, cancel the first.
    let row = |goal: &str, repo: &str, acc: &str| {
        json!({ "parent": { "database_id": db }, "properties": {
            "Goal": { "title": [{ "text": { "content": goal } }] },
            "Repo": { "rich_text": [{ "text": { "content": repo } }] },
            "Acceptance": { "rich_text": [{ "text": { "content": acc } }] },
            "Agent": { "select": { "name": "claude-code" } }
        }})
    };
    let proposal = notion::request(
        "POST",
        "/pages",
        &tok,
        Some(&row("Proposed in Notion", "demo", "a file exists")),
    )
    .unwrap();
    let pid = proposal["id"].as_str().unwrap().to_string();
    notion::request("POST", "/pages", &tok, Some(&row("Half an idea", "", ""))).unwrap();
    let (first_page, _) = notion::ledger_get(&format!("notion-row:{}", first.id)).unwrap();
    notion::request(
        "PATCH",
        &format!("/pages/{first_page}"),
        &tok,
        Some(&json!({ "properties": { "Request": { "select": { "name": "Cancel" } } } })),
    )
    .unwrap();

    let r3 = notion_sync::sync(false);
    println!("sync 3: {r3:?}");
    let proposed = missions::list(10)
        .unwrap()
        .into_iter()
        .find(|m| m.goal == "Proposed in Notion");
    let back = notion::request("GET", &format!("/pages/{pid}"), &tok, None).unwrap();
    let written_id = back["properties"]["ID"]["rich_text"][0]["plain_text"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let r4 = notion_sync::sync(false);
    println!("sync 4: {r4:?}");
    let cancelled = missions::get(&first.id).unwrap().unwrap();

    // Trust: the proposal above came from the integration itself, which is not trusted, so it is
    // blocked. Trust that author, propose again, and it queues. An unknown agent is declined.
    let me = notion::request("GET", "/users/me", &tok, None).unwrap();
    let bot = me["id"].as_str().unwrap().to_string();
    notion_sync::set_trusted_authors(std::slice::from_ref(&bot)).unwrap();
    notion::request(
        "POST",
        "/pages",
        &tok,
        Some(&row("Trusted proposal", "demo", "a file exists")),
    )
    .unwrap();
    let mut bad = row("Wrong agent", "demo", "x");
    bad["properties"]["Agent"] = json!({ "select": { "name": "cursor" } });
    notion::request("POST", "/pages", &tok, Some(&bad)).unwrap();
    let r5 = notion_sync::sync(false);
    println!("sync 5: {r5:?}");
    let trusted = missions::list(20)
        .unwrap()
        .into_iter()
        .find(|m| m.goal == "Trusted proposal");
    let approved = notion_sync::approve(&proposed.as_ref().unwrap().id, false).unwrap();

    notion_sync::archive_board().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(r3.error.is_none(), "{:?}", r3.error);
    let proposed = proposed.expect("proposal became a mission");
    assert_eq!(
        proposed.status, "blocked",
        "an untrusted proposal waits for approval"
    );
    assert!(proposed
        .review_note
        .as_deref()
        .unwrap_or("")
        .contains("needs approval in Studio"));
    assert_eq!(r3.awaiting_approval, vec![proposed.id.clone()]);
    assert_eq!(approved.status, "queued", "Approve in Studio releases it");
    let trusted = trusted.expect("trusted proposal became a mission");
    assert_eq!(
        trusted.status, "queued",
        "a trusted author's proposal queues directly"
    );
    assert!(r5.awaiting_approval.is_empty());
    assert!(
        r5.declined
            .iter()
            .any(|d| d.contains("not one Studio knows")),
        "{:?}",
        r5.declined
    );
    assert!(missions::list(20)
        .unwrap()
        .iter()
        .all(|m| m.goal != "Wrong agent"));
    assert_eq!(r3.proposed, vec![proposed.id.clone()]);
    assert_eq!(
        written_id, proposed.id,
        "the ID was written back to the Notion row"
    );
    assert_eq!(r3.cancelled, vec![first.id.clone()]);
    assert_eq!(cancelled.status, "failed");
    assert!(r3
        .declined
        .iter()
        .any(|d| d.contains("Needs Repo and Acceptance")));
    assert_eq!(
        (r4.created_rows, r4.updated_rows),
        (0, 0),
        "settled after one round"
    );
    assert!(r4.proposed.is_empty() && r4.cancelled.is_empty());
}
