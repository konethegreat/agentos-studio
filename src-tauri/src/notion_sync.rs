//! Two-way mission sync with Notion, through one database: "AgentOS missions board".
//!
//! Studio owns the mission. Out, every mission is a row (ID, goal, repository, status, verdict,
//! PR, spend), updated only when its content hash changes. In, Notion may do exactly two things:
//!
//! - **Propose:** a row with a Goal, Repo and Acceptance and no ID becomes a queued mission, with
//!   the same model precedence as the New mission form; Studio writes the ID back to the row.
//! - **Cancel:** `Request = Cancel` on a queued or blocked mission marks it failed. A running or
//!   in-review mission is refused with a note, because only Studio can end its run.
//!
//! Status, verdict and merge never come from Notion: the review gate stays the only way to merge,
//! and an edit to a Studio-owned column is overwritten on the next sync. Every row Studio wrote is
//! in `sync_ledger` (`notion-row:<mission id>`), so an unchanged mission costs no request.

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::missions::{self, Mission};
use crate::notion::{hash, ledger_get, ledger_put, request};

pub const KV_DB: &str = "notion.missions_db";
pub const DB_TITLE: &str = "AgentOS missions board";

#[derive(Serialize, Debug, Default, Clone)]
pub struct SyncReport {
    pub connected: bool,
    pub dry_run: bool,
    pub database: Option<String>,
    pub created_rows: usize,
    pub updated_rows: usize,
    pub unchanged_rows: usize,
    /// Missions created from Notion proposals.
    pub proposed: Vec<String>,
    /// Missions cancelled from Notion.
    pub cancelled: Vec<String>,
    /// Requests Studio declined, with the reason written back to the row.
    pub declined: Vec<String>,
    pub error: Option<String>,
}

fn text(v: &str) -> Value {
    json!([{ "type": "text", "text": { "content": v.chars().take(1900).collect::<String>() } }])
}

fn select(v: &str) -> Value {
    if v.is_empty() {
        Value::Null
    } else {
        json!({ "name": v })
    }
}

/// The database schema Studio creates. Pure.
pub fn schema() -> Value {
    let opts = |names: &[&str]| json!({ "options": names.iter().map(|n| json!({ "name": n })).collect::<Vec<_>>() });
    json!({
        "Goal": { "title": {} },
        "ID": { "rich_text": {} },
        "Repo": { "rich_text": {} },
        "Acceptance": { "rich_text": {} },
        "Role": { "select": opts(&missions::ROLES) },
        "Agent": { "select": opts(&["hermes", "claude-code", "antigravity"]) },
        "Status": { "select": opts(&missions::STATUSES) },
        "Verdict": { "rich_text": {} },
        "PR": { "url": {} },
        "Spent USD": { "number": { "format": "dollar" } },
        "Request": { "select": opts(&["Cancel"]) },
        "Studio note": { "rich_text": {} }
    })
}

/// The Studio-owned columns of a mission's row. `Request` is cleared once handled and
/// `Studio note` is written separately. Pure.
pub fn row_props(m: &Mission) -> Value {
    json!({
        "Goal": { "title": text(&m.goal) },
        "ID": { "rich_text": text(&m.id) },
        "Repo": { "rich_text": text(&m.repo) },
        "Acceptance": { "rich_text": text(&m.acceptance) },
        "Role": { "select": select(&m.role) },
        "Agent": { "select": select(&m.agent) },
        "Status": { "select": select(&m.status) },
        "Verdict": { "rich_text": text(m.review_verdict.as_deref().unwrap_or("")) },
        "PR": { "url": m.pr_url.clone().map(Value::String).unwrap_or(Value::Null) },
        "Spent USD": { "number": (m.spent_usd * 100.0).round() / 100.0 }
    })
}

fn ledger_key(id: &str) -> String {
    format!("notion-row:{id}")
}

/// One row as Notion returns it, reduced to what Studio reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Row {
    pub page_id: String,
    pub id: String,
    pub goal: String,
    pub repo: String,
    pub acceptance: String,
    pub role: String,
    pub agent: String,
    pub request: String,
    pub note: String,
}

fn plain(p: &Value) -> String {
    let arr = p
        .get("title")
        .or_else(|| p.get("rich_text"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    arr.iter()
        .filter_map(|t| t.get("plain_text").and_then(Value::as_str))
        .collect::<String>()
        .trim()
        .to_string()
}

/// Parse one page from a database query. Pure.
pub fn parse_row(page: &Value) -> Option<Row> {
    let props = page.get("properties")?;
    let get = |k: &str| props.get(k).map(plain).unwrap_or_default();
    let sel = |k: &str| {
        props
            .get(k)
            .and_then(|p| p.get("select"))
            .and_then(|s| s.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    Some(Row {
        page_id: page.get("id")?.as_str()?.to_string(),
        id: get("ID"),
        goal: get("Goal"),
        repo: get("Repo"),
        acceptance: get("Acceptance"),
        role: sel("Role"),
        agent: sel("Agent"),
        request: sel("Request"),
        note: get("Studio note"),
    })
}

/// What Studio will do with a row coming back from Notion. Pure.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Propose,
    /// A proposal that cannot become a mission yet, with the note to write back.
    Incomplete(String),
    Cancel(String),
    Nothing,
}

pub fn classify(r: &Row) -> Inbound {
    if r.id.is_empty() {
        if r.goal.is_empty() {
            return Inbound::Nothing;
        }
        let mut missing = Vec::new();
        if r.repo.is_empty() {
            missing.push("Repo");
        }
        if r.acceptance.is_empty() {
            missing.push("Acceptance");
        }
        return if missing.is_empty() {
            Inbound::Propose
        } else {
            Inbound::Incomplete(format!(
                "Needs {} before Studio can queue it",
                missing.join(" and ")
            ))
        };
    }
    if r.request.eq_ignore_ascii_case("cancel") {
        return Inbound::Cancel(r.id.clone());
    }
    Inbound::Nothing
}

fn db_id(tok: &str, parent: &str, dry_run: bool) -> Result<Option<String>> {
    if let Some(id) = crate::notion::setting(KV_DB) {
        if request("GET", &format!("/databases/{id}"), tok, None).is_ok() {
            return Ok(Some(id));
        }
    }
    if dry_run {
        return Ok(None);
    }
    let created = request(
        "POST",
        "/databases",
        tok,
        Some(&json!({
            "parent": { "type": "page_id", "page_id": parent },
            "title": text(DB_TITLE),
            "properties": schema()
        })),
    )?;
    let id = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Notion did not return a database id"))?
        .to_string();
    crate::notion::set_setting(KV_DB, &id);
    Ok(Some(id))
}

fn query_rows(tok: &str, db: &str) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let mut body = json!({ "page_size": 100 });
        if let Some(c) = &cursor {
            body["start_cursor"] = json!(c);
        }
        let res = request("POST", &format!("/databases/{db}/query"), tok, Some(&body))?;
        rows.extend(
            res.get("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(parse_row),
        );
        cursor = res
            .get("next_cursor")
            .and_then(Value::as_str)
            .map(String::from);
        if res.get("has_more").and_then(Value::as_bool) != Some(true) || cursor.is_none() {
            break;
        }
    }
    Ok(rows)
}

fn patch_row(tok: &str, page: &str, props: Value) -> Result<()> {
    request(
        "PATCH",
        &format!("/pages/{page}"),
        tok,
        Some(&json!({ "properties": props })),
    )
    .map(|_| ())
}

fn note_props(note: &str) -> Value {
    json!({ "Studio note": { "rich_text": text(note) }, "Request": { "select": Value::Null } })
}

/// Pull proposals and cancels from the board, then push every mission's row.
pub fn sync(dry_run: bool) -> SyncReport {
    sync_inner(dry_run, crate::notion::token(), crate::notion::parent_id())
}

pub fn sync_inner(dry_run: bool, tok: Option<String>, parent: Option<String>) -> SyncReport {
    let mut r = SyncReport {
        connected: tok.is_some() && parent.is_some(),
        dry_run: dry_run || tok.is_none() || parent.is_none(),
        ..Default::default()
    };
    let (Some(tok), Some(parent)) = (tok, parent) else {
        // Nothing can be read without a token; report what a first sync would publish.
        r.created_rows = missions::list(200).map(|l| l.len()).unwrap_or(0);
        return r;
    };
    if let Err(e) = sync_connected(&mut r, &tok, &parent) {
        r.error = Some(e.to_string().chars().take(300).collect());
    }
    r
}

fn sync_connected(r: &mut SyncReport, tok: &str, parent: &str) -> Result<()> {
    let dry = r.dry_run;
    let Some(db) = db_id(tok, parent, dry)? else {
        r.created_rows = missions::list(200)?.len();
        return Ok(());
    };
    r.database = Some(db.clone());

    // In: the two things Notion may ask for.
    for row in query_rows(tok, &db)? {
        match classify(&row) {
            Inbound::Propose => {
                if dry {
                    r.proposed.push(format!("(dry run) {}", row.goal));
                    continue;
                }
                let role = if row.role.is_empty() {
                    "implementer"
                } else {
                    row.role.as_str()
                };
                let agent = if row.agent.is_empty() {
                    "hermes"
                } else {
                    row.agent.as_str()
                };
                match crate::router::create_mission(
                    &row.repo,
                    &row.goal,
                    &row.acceptance,
                    role,
                    agent,
                    "",
                    "",
                    0.0,
                ) {
                    Ok(m) => {
                        let mut props = row_props(&m);
                        props["Studio note"] =
                            json!({ "rich_text": text(&format!("Queued by Studio as {}", m.id)) });
                        patch_row(tok, &row.page_id, props)?;
                        ledger_put(
                            &ledger_key(&m.id),
                            &row.page_id,
                            &hash(&row_props(&m).to_string()),
                        )?;
                        r.proposed.push(m.id);
                    }
                    Err(e) => {
                        let note = format!("Not queued: {e}");
                        if row.note != note {
                            patch_row(tok, &row.page_id, note_props(&note))?;
                        }
                        r.declined.push(format!("{}: {e}", row.goal));
                    }
                }
            }
            Inbound::Incomplete(note) => {
                if !dry && row.note != note {
                    patch_row(tok, &row.page_id, note_props(&note))?;
                }
                r.declined.push(format!("{}: {note}", row.goal));
            }
            Inbound::Cancel(id) => {
                let status = missions::get(&id)?.map(|m| m.status);
                let outcome = match status.as_deref() {
                    None => Err(format!("{id} is not a Studio mission")),
                    Some("queued") | Some("blocked") if dry => Ok(format!("(dry run) {id}")),
                    Some("queued") | Some("blocked") => {
                        missions::cancel(&id, "Cancelled from Notion")
                            .map(|_| id.clone())
                            .map_err(|e| e.to_string())
                    }
                    Some(s) => Err(format!(
                        "{id} is {s}; stop it in Studio, where the run can be ended"
                    )),
                };
                match outcome {
                    Ok(done) => {
                        if !dry {
                            patch_row(tok, &row.page_id, note_props("Cancelled from Notion"))?;
                        }
                        r.cancelled.push(done);
                    }
                    Err(why) => {
                        if !dry {
                            patch_row(tok, &row.page_id, note_props(&why))?;
                        }
                        r.declined.push(why);
                    }
                }
            }
            Inbound::Nothing => {}
        }
    }

    // Out: one row per mission, only when it changed.
    for m in missions::list(200)? {
        let props = row_props(&m);
        let h = hash(&props.to_string());
        match ledger_get(&ledger_key(&m.id)) {
            Some((_, old)) if old == h => r.unchanged_rows += 1,
            Some((page, _)) => {
                if !dry {
                    patch_row(tok, &page, props)?;
                    ledger_put(&ledger_key(&m.id), &page, &h)?;
                }
                r.updated_rows += 1;
            }
            None => {
                if !dry {
                    let page = request(
                        "POST",
                        "/pages",
                        tok,
                        Some(&json!({ "parent": { "database_id": db }, "properties": props })),
                    )?;
                    let pid = page
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("Notion returned no page id for {}", m.id))?;
                    ledger_put(&ledger_key(&m.id), pid, &h)?;
                }
                r.created_rows += 1;
            }
        }
    }
    Ok(())
}

/// Archive the board in Notion (recoverable from Notion's trash) and forget it locally.
pub fn archive_board() -> Result<()> {
    let tok = crate::notion::token().ok_or_else(|| anyhow!("Notion is not connected"))?;
    let Some(db) = crate::notion::setting(KV_DB) else {
        bail!("No missions board yet");
    };
    request(
        "PATCH",
        &format!("/databases/{db}"),
        &tok,
        Some(&json!({ "archived": true })),
    )?;
    crate::notion::set_setting(KV_DB, "");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(props: Value) -> Value {
        json!({ "id": "p1", "properties": props })
    }
    fn rt(s: &str) -> Value {
        json!({ "rich_text": [{ "plain_text": s }] })
    }

    #[test]
    fn notion_may_propose_and_cancel_and_nothing_else() {
        let proposal = parse_row(&page(json!({
            "Goal": { "title": [{ "plain_text": "Add a README" }] },
            "Repo": rt("demo"), "Acceptance": rt("README exists"), "ID": rt("")
        })))
        .unwrap();
        assert_eq!(classify(&proposal), Inbound::Propose);
        let half = Row {
            acceptance: String::new(),
            ..proposal.clone()
        };
        assert_eq!(
            classify(&half),
            Inbound::Incomplete("Needs Acceptance before Studio can queue it".into())
        );
        let cancel = parse_row(&page(json!({
            "Goal": { "title": [{ "plain_text": "x" }] }, "ID": rt("M-004"),
            "Request": { "select": { "name": "Cancel" } }
        })))
        .unwrap();
        assert_eq!(classify(&cancel), Inbound::Cancel("M-004".into()));
        // A human setting Status to done in Notion is not a request.
        let edited = parse_row(&page(json!({
            "Goal": { "title": [{ "plain_text": "x" }] }, "ID": rt("M-004"),
            "Status": { "select": { "name": "done" } }
        })))
        .unwrap();
        assert_eq!(classify(&edited), Inbound::Nothing);
        assert_eq!(classify(&Row::default()), Inbound::Nothing);
    }

    #[test]
    fn row_props_cover_the_schema_and_hash_stably() {
        let m = Mission {
            id: "M-001".into(),
            repo: "demo".into(),
            goal: "g".into(),
            acceptance: "a".into(),
            role: "implementer".into(),
            agent: "hermes".into(),
            status: "queued".into(),
            provider: String::new(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.123,
            task_id: None,
            reviewer: None,
            review_verdict: None,
            pr_url: None,
            review_note: None,
            created_at: 0,
            updated_at: 0,
            verify: None,
        };
        let p = row_props(&m);
        let schema = schema();
        for k in p.as_object().unwrap().keys() {
            assert!(schema.get(k).is_some(), "{k} is in the schema");
        }
        assert_eq!(p["Spent USD"]["number"], 0.12);
        assert_eq!(hash(&p.to_string()), hash(&row_props(&m).to_string()));
        let mut m2 = m.clone();
        m2.status = "running".into();
        assert_ne!(hash(&p.to_string()), hash(&row_props(&m2).to_string()));
    }

    #[test]
    fn without_a_token_nothing_is_sent() {
        let r = sync_inner(false, None, None);
        assert!(r.dry_run && !r.connected && r.database.is_none());
    }
}
