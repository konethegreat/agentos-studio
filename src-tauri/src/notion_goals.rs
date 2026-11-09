//! Two-way goal sync with Notion, through one database: "AgentOS goals board", next to the
//! missions board (`notion_sync.rs`).
//!
//! Studio owns the goal. Out, every goal is a row (ID, outcome, repositories, done checks,
//! autonomy, status, progress, spend against budget, the open question and the report), updated
//! only when its content hash changes. In, Notion may do exactly two things:
//!
//! - **Propose:** a row with a Goal, Repos and Done checks and no ID becomes a goal through
//!   `goals::create`, the same path as the Goals form, and Studio writes the ID back. A trusted
//!   author's goal (`notion.trusted_authors`, the missions board list) is planned at once at the
//!   autonomy it asked for. Anyone else's goal is created at Watch: the plan and every mission wait
//!   for a click in Studio.
//! - **Decide:** the row shows the goal's oldest open escalation (`Escalation`, `Question`). A
//!   `Decision` set by a trusted author answers it through `escalation::answer`, answered by
//!   `notion`; the goal loop applies it on its next pass, exactly like an answer from Studio or the
//!   phone. A Decision from anyone else, for another escalation, or outside the allowed answers is
//!   refused with a note and cleared.
//!
//! Status, checks, missions, merges and spend never come from Notion: an edit to a Studio-owned
//! column is overwritten on the next sync. Every row Studio wrote is in `sync_ledger`
//! (`notion-goal:<goal id>`), so an unchanged goal costs no request, and the row of a goal that no
//! longer exists is archived.

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Mutex;

use crate::escalation::{self, Escalation};
use crate::goals::{self, Check, Goal, NewGoal};
use crate::missions::Mission;
use crate::notion::{
    hash, ledger_forget, ledger_forget_prefix, ledger_get, ledger_in_for_page, ledger_keys,
    ledger_put, ledger_put_in, request, FieldOwner,
};
use crate::vault::Vault;

pub const KV_DB: &str = "notion.goals_db";
pub const DB_TITLE: &str = "AgentOS goals board";
const LEDGER_PREFIX: &str = "notion-goal:";
/// Every answer an escalation can take (`goal_loop` and `completion` raise these).
pub const DECISIONS: [&str; 5] = ["merge", "skip", "retry", "stop", "raise"];

/// One sync at a time: the scheduler and the button must not both create the same goal.
static SYNC: Mutex<()> = Mutex::new(());

#[derive(Serialize, Debug, Default, Clone)]
pub struct SyncReport {
    pub connected: bool,
    pub dry_run: bool,
    pub database: Option<String>,
    pub created_rows: usize,
    pub updated_rows: usize,
    pub unchanged_rows: usize,
    /// Rows archived because their goal no longer exists in Studio.
    pub archived_rows: usize,
    /// Goals created from Notion proposals.
    pub proposed: Vec<String>,
    /// Of those, the ones created at Watch because their author is not trusted.
    pub held_at_watch: Vec<String>,
    /// Escalations answered from the Decision column, as `E-n answer`.
    pub decided: Vec<String>,
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
        "Repos": { "rich_text": {} },
        "Done checks": { "rich_text": {} },
        "Autonomy": { "select": opts(&goals::LEVELS) },
        "Budget USD": { "number": { "format": "dollar" } },
        "Deadline": { "rich_text": {} },
        "Status": { "select": opts(&goals::STATUSES) },
        "Progress": { "rich_text": {} },
        "Spent USD": { "number": { "format": "dollar" } },
        "Status note": { "rich_text": {} },
        "Escalation": { "rich_text": {} },
        "Question": { "rich_text": {} },
        "Decision": { "select": opts(&DECISIONS) },
        "Report": { "rich_text": {} },
        "Studio note": { "rich_text": {} }
    })
}

/// The one owner of every column (`schema`). Notion-owned and proposal columns are the only
/// ones Studio reads; Studio-owned columns are overwritten on every sync.
pub const OWNERS: [(&str, FieldOwner); 16] = [
    ("Goal", FieldOwner::ProposalThenStudio),
    ("ID", FieldOwner::Studio),
    ("Repos", FieldOwner::ProposalThenStudio),
    ("Done checks", FieldOwner::ProposalThenStudio),
    ("Autonomy", FieldOwner::ProposalThenStudio),
    ("Budget USD", FieldOwner::ProposalThenStudio),
    ("Deadline", FieldOwner::ProposalThenStudio),
    ("Status", FieldOwner::Studio),
    ("Progress", FieldOwner::Studio),
    ("Spent USD", FieldOwner::Studio),
    ("Status note", FieldOwner::Studio),
    ("Escalation", FieldOwner::Studio),
    ("Question", FieldOwner::Studio),
    ("Decision", FieldOwner::Notion),
    ("Report", FieldOwner::Studio),
    ("Studio note", FieldOwner::Studio),
];

const DECISION_PREFIX: &str = "notion-decision:";

/// Checks as the board shows them and a proposal writes them: one `kind: target` per line.
pub fn checks_text(checks: &[Check]) -> String {
    checks
        .iter()
        .map(|c| match c.repo.as_deref().filter(|r| !r.is_empty()) {
            Some(r) => format!("{}: {} @{r}", c.kind, c.target),
            None => format!("{}: {}", c.kind, c.target),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse the Done checks a person typed in Notion. Each line is `command: npm test`,
/// `url: https://...` or `file: docs/X.md`, optionally ending in `@repo`. Pure.
pub fn parse_checks(s: &str) -> std::result::Result<Vec<Check>, String> {
    let mut out = Vec::new();
    for line in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let line = line.trim_start_matches(['-', '*']).trim();
        let Some((kind, rest)) = line.split_once(':') else {
            return Err(format!(
                "Done check \"{line}\" needs a kind: command:, url: or file:"
            ));
        };
        let kind = kind.trim().to_lowercase();
        if !goals::CHECK_KINDS.contains(&kind.as_str()) {
            return Err(format!(
                "Done check kind \"{kind}\" is not one of command, url, file"
            ));
        }
        let mut target = rest.trim().to_string();
        let mut repo = None;
        if let Some(at) = target.rfind(" @") {
            let r = target[at + 2..].trim().to_string();
            if !r.is_empty() && !r.contains(char::is_whitespace) {
                repo = Some(r);
                target = target[..at].trim().to_string();
            }
        }
        let c = Check { kind, target, repo };
        goals::validate_check(&c).map_err(|e| e.to_string())?;
        out.push(c);
    }
    Ok(out)
}

/// Repositories from a comma, space or newline separated list. Pure.
pub fn parse_repos(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in s.split(|c: char| c == ',' || c.is_whitespace()) {
        let r = r.trim();
        if !r.is_empty() && !out.iter().any(|o| o == r) {
            out.push(r.to_string());
        }
    }
    out
}

/// The goal's oldest open escalation: the one question the row shows and Decision answers.
pub fn open_question<'a>(goal_id: &str, open: &'a [Escalation]) -> Option<&'a Escalation> {
    open.iter()
        .filter(|e| e.status == "open" && e.goal_id.as_deref() == Some(goal_id))
        .min_by_key(|e| (e.created_at, e.id.clone()))
}

fn question_text(e: &Escalation) -> String {
    let deadline = chrono::DateTime::from_timestamp(e.deadline_at, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_default();
    format!(
        "{} Answers: {}. With no answer by {deadline} Studio takes \"{}\".",
        e.question,
        e.options.join(", "),
        e.safe_answer
    )
}

/// The Studio-owned columns of a goal's row. `Decision` is cleared once handled and
/// `Studio note` is written separately. Pure.
pub fn row_props(g: &Goal, ms: &[Mission], question: Option<&Escalation>) -> Value {
    let done = ms.iter().filter(|m| m.status == "done").count();
    let progress = if ms.is_empty() {
        "no missions yet".to_string()
    } else {
        format!("{done} of {} missions done", ms.len())
    };
    let cents = |x: f64| (x * 100.0).round() / 100.0;
    json!({
        "Goal": { "title": text(&g.outcome) },
        "ID": { "rich_text": text(&g.id) },
        "Repos": { "rich_text": text(&g.repos.join(", ")) },
        "Done checks": { "rich_text": text(&checks_text(&g.checks)) },
        "Autonomy": { "select": select(&g.autonomy) },
        "Budget USD": { "number": cents(g.budget_usd) },
        "Deadline": { "rich_text": text(g.deadline.as_deref().unwrap_or("")) },
        "Status": { "select": select(&g.status) },
        "Progress": { "rich_text": text(&progress) },
        "Spent USD": { "number": cents(goals::spent(ms)) },
        "Status note": { "rich_text": text(&g.note) },
        "Escalation": { "rich_text": text(question.map(|e| e.id.as_str()).unwrap_or("")) },
        "Question": { "rich_text": text(&question.map(question_text).unwrap_or_default()) },
        "Report": { "rich_text": text(g.report.as_deref().unwrap_or("")) }
    })
}

pub fn ledger_key(id: &str) -> String {
    format!("{LEDGER_PREFIX}{id}")
}

/// One row as Notion returns it, reduced to what Studio reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Row {
    pub page_id: String,
    pub id: String,
    pub outcome: String,
    pub repos: String,
    pub checks: String,
    pub autonomy: String,
    pub budget: Option<f64>,
    pub deadline: String,
    pub escalation: String,
    pub decision: String,
    pub note: String,
    /// Notion user id of whoever created the row (the trust check for a proposal).
    pub author: String,
    /// Notion user id of whoever edited the row last (the trust check for a Decision).
    pub editor: String,
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

fn user(page: &Value, key: &str) -> String {
    page.get(key)
        .and_then(|c| c.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase()
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
        outcome: get("Goal"),
        repos: get("Repos"),
        checks: get("Done checks"),
        autonomy: sel("Autonomy").to_lowercase(),
        budget: props
            .get("Budget USD")
            .and_then(|p| p.get("number"))
            .and_then(Value::as_f64),
        deadline: get("Deadline"),
        escalation: get("Escalation").to_uppercase(),
        decision: sel("Decision").to_lowercase(),
        note: get("Studio note"),
        author: user(page, "created_by"),
        editor: user(page, "last_edited_by"),
    })
}

/// What Studio will do with a row coming back from Notion. Pure.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Propose(NewGoal),
    /// A proposal that cannot become a goal yet, with the note to write back.
    Incomplete(String),
    /// A Decision on the escalation the row names: `(escalation id, answer)`.
    Decide(String, String),
    Nothing,
}

pub fn classify(r: &Row) -> Inbound {
    if r.id.is_empty() {
        if r.outcome.is_empty() {
            return Inbound::Nothing;
        }
        let repos = parse_repos(&r.repos);
        let mut missing = Vec::new();
        if repos.is_empty() {
            missing.push("Repos");
        }
        if r.checks.trim().is_empty() {
            missing.push("Done checks");
        }
        if !missing.is_empty() {
            return Inbound::Incomplete(format!(
                "Needs {} before Studio can create the goal",
                missing.join(" and ")
            ));
        }
        let checks = match parse_checks(&r.checks) {
            Ok(c) => c,
            Err(e) => return Inbound::Incomplete(e),
        };
        if !r.autonomy.is_empty() && !goals::LEVELS.contains(&r.autonomy.as_str()) {
            return Inbound::Incomplete(format!(
                "Autonomy {} is not one of watch, ask, full",
                r.autonomy
            ));
        }
        if r.budget.is_some_and(|b| b < 0.0) {
            return Inbound::Incomplete("Budget USD cannot be negative".into());
        }
        return Inbound::Propose(NewGoal {
            outcome: r.outcome.clone(),
            repos,
            checks,
            budget_usd: r.budget.unwrap_or(0.0),
            deadline: Some(r.deadline.clone()).filter(|d| !d.is_empty()),
            autonomy: Some(r.autonomy.clone()).filter(|a| !a.is_empty()),
        });
    }
    if !r.decision.is_empty() {
        return Inbound::Decide(r.escalation.clone(), r.decision.clone());
    }
    Inbound::Nothing
}

/// Whether a Decision on goal `goal_id`'s row may answer `esc`. Pure: the escalation must exist,
/// belong to this goal, still be open, take this answer, and the last editor must be trusted.
pub fn decision_refusal(
    goal_id: &str,
    esc_id: &str,
    answer: &str,
    esc: Option<&Escalation>,
    editor: &str,
    trusted: &[String],
) -> Option<String> {
    if esc_id.is_empty() {
        return Some(format!(
            "{goal_id} has no open question; Decision {answer} was not applied"
        ));
    }
    let Some(e) = esc else {
        return Some(format!("{esc_id} is not a Studio escalation"));
    };
    if e.goal_id.as_deref() != Some(goal_id) {
        return Some(format!("{esc_id} belongs to another goal"));
    }
    if e.status != "open" {
        return Some(format!(
            "{esc_id} is already {} ({})",
            e.status,
            e.answer.as_deref().unwrap_or("no answer")
        ));
    }
    if !e.options.iter().any(|o| o == answer) {
        return Some(format!(
            "{answer} is not an answer to {esc_id}; use one of: {}",
            e.options.join(", ")
        ));
    }
    if editor.is_empty() || !trusted.iter().any(|t| t == editor) {
        let who = if editor.is_empty() {
            "an unknown editor"
        } else {
            editor
        };
        return Some(format!(
            "Decision by {who}, who is not a trusted Notion author; answer {esc_id} in Studio or from the phone"
        ));
    }
    None
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
    json!({ "Studio note": { "rich_text": text(note) }, "Decision": { "select": Value::Null } })
}

/// Pull proposals and decisions from the board, then push every goal's row.
pub fn sync(v: &Vault, dry_run: bool) -> SyncReport {
    sync_inner(
        v,
        dry_run,
        crate::notion::token(),
        crate::notion::parent_id(),
        true,
    )
}

/// `plan = false` creates proposed goals without starting the planner (live tests).
pub fn sync_inner(
    v: &Vault,
    dry_run: bool,
    tok: Option<String>,
    parent: Option<String>,
    plan: bool,
) -> SyncReport {
    let _one = SYNC.lock().unwrap_or_else(|p| p.into_inner());
    let mut r = SyncReport {
        connected: tok.is_some() && parent.is_some(),
        dry_run: dry_run || tok.is_none() || parent.is_none(),
        ..Default::default()
    };
    let (Some(tok), Some(parent)) = (tok, parent) else {
        // Nothing can be read without a token; report what a first sync would publish.
        r.created_rows = goals::list(200).map(|l| l.len()).unwrap_or(0);
        return r;
    };
    if let Err(e) = sync_connected(&mut r, v, &tok, &parent, plan) {
        r.error = Some(e.to_string().chars().take(300).collect());
    }
    r
}

fn sync_connected(
    r: &mut SyncReport,
    v: &Vault,
    tok: &str,
    parent: &str,
    plan: bool,
) -> Result<()> {
    let dry = r.dry_run;
    let Some(db) = db_id(tok, parent, dry)? else {
        r.created_rows = goals::list(200)?.len();
        return Ok(());
    };
    r.database = Some(db.clone());

    // In: the two things Notion may ask for.
    let trusted = crate::notion_sync::trusted_authors();
    for row in query_rows(tok, &db)? {
        match classify(&row) {
            Inbound::Propose(mut input) => {
                if dry {
                    r.proposed.push(format!("(dry run) {}", row.outcome));
                    continue;
                }
                // A page that already produced a goal (the ID write-back failed last time) is
                // repaired, never turned into a second goal.
                if let Some((key, _)) = ledger_in_for_page(LEDGER_PREFIX, &row.page_id) {
                    let id = key[LEDGER_PREFIX.len()..].to_string();
                    match goals::get(&id)? {
                        Some(g) => {
                            let ms = goals::missions_of(&g.id).unwrap_or_default();
                            let props = row_props(&g, &ms, None);
                            patch_row(tok, &row.page_id, props.clone())?;
                            ledger_put(&key, &row.page_id, &hash(&props.to_string()))?;
                        }
                        None => {
                            let note = format!("{id} came from this row and was deleted in Studio");
                            if row.note != note {
                                patch_row(tok, &row.page_id, note_props(&note))?;
                            }
                            r.declined.push(note);
                        }
                    }
                    continue;
                }
                let read = hash(&format!("{input:?}|{}", row.author));
                let is_trusted = !row.author.is_empty() && trusted.contains(&row.author);
                if !is_trusted {
                    input.autonomy = Some("watch".into());
                }
                match goals::create(&input) {
                    Ok(g) => {
                        let who = if row.author.is_empty() {
                            "an unknown author"
                        } else {
                            row.author.as_str()
                        };
                        let note = if is_trusted {
                            format!("Created by Studio as {} from Notion", g.id)
                        } else {
                            let why = format!(
                                "Proposed in Notion by {who}, not a trusted author: held at Watch, so the plan and every mission wait for approval in Studio"
                            );
                            goals::set_note(&g.id, &why)?;
                            format!("Created as {} at Watch (author not trusted)", g.id)
                        };
                        let _ = goals::write_note(v, &g.id);
                        if plan {
                            crate::planner::plan_in_background(v.root.clone(), g.id.clone());
                        }
                        ledger_put_in(&ledger_key(&g.id), &row.page_id, &read)?;
                        let g = goals::get(&g.id)?.unwrap_or(g);
                        let props = row_props(&g, &[], None);
                        let mut first = props.clone();
                        first["Studio note"] = json!({ "rich_text": text(&note) });
                        patch_row(tok, &row.page_id, first)?;
                        ledger_put(&ledger_key(&g.id), &row.page_id, &hash(&props.to_string()))?;
                        if !is_trusted {
                            r.held_at_watch.push(g.id.clone());
                        }
                        r.proposed.push(g.id);
                    }
                    Err(e) => {
                        let note = format!("Not created: {e}");
                        if row.note != note {
                            patch_row(tok, &row.page_id, note_props(&note))?;
                        }
                        r.declined.push(format!("{}: {e}", row.outcome));
                    }
                }
            }
            Inbound::Incomplete(note) => {
                if !dry && row.note != note {
                    patch_row(tok, &row.page_id, note_props(&note))?;
                }
                r.declined.push(format!("{}: {note}", row.outcome));
            }
            Inbound::Decide(esc_id, answer) => {
                let esc = if esc_id.is_empty() {
                    None
                } else {
                    escalation::get(&esc_id)?
                };
                match decision_refusal(
                    &row.id,
                    &esc_id,
                    &answer,
                    esc.as_ref(),
                    &row.editor,
                    &trusted,
                ) {
                    Some(why) => {
                        if !dry {
                            patch_row(tok, &row.page_id, note_props(&why))?;
                        }
                        r.declined.push(why);
                    }
                    None if dry => r.decided.push(format!("(dry run) {esc_id} {answer}")),
                    None => {
                        let note = match escalation::answer(&esc_id, &answer, "notion") {
                            Ok(_) => {
                                ledger_put_in(
                                    &format!("{DECISION_PREFIX}{esc_id}"),
                                    &row.page_id,
                                    &hash(&format!("{esc_id}|{answer}|{}", row.editor)),
                                )?;
                                r.decided.push(format!("{esc_id} {answer}"));
                                format!("{esc_id} answered {answer} from Notion; Studio applies it on its next pass")
                            }
                            Err(e) => {
                                r.declined.push(e.to_string());
                                e.to_string()
                            }
                        };
                        patch_row(tok, &row.page_id, note_props(&note))?;
                    }
                }
            }
            Inbound::Nothing => {}
        }
    }

    // Out: one row per goal, only when it changed.
    let open = escalation::open_list().unwrap_or_default();
    let all = goals::list(200)?;
    for g in &all {
        let ms = goals::missions_of(&g.id).unwrap_or_default();
        let props = row_props(g, &ms, open_question(&g.id, &open));
        let h = hash(&props.to_string());
        match ledger_get(&ledger_key(&g.id)) {
            Some((_, old)) if old == h => r.unchanged_rows += 1,
            Some((page, _)) => {
                if !dry {
                    patch_row(tok, &page, props)?;
                    ledger_put(&ledger_key(&g.id), &page, &h)?;
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
                        .ok_or_else(|| anyhow!("Notion returned no page id for {}", g.id))?;
                    ledger_put(&ledger_key(&g.id), pid, &h)?;
                }
                r.created_rows += 1;
            }
        }
    }

    // A deleted goal's row is archived (recoverable from Notion's trash), so the board never
    // offers a Decision on something Studio no longer has.
    for (key, page) in ledger_keys(LEDGER_PREFIX) {
        let id = &key[LEDGER_PREFIX.len()..];
        if all.iter().any(|g| g.id == id) {
            continue;
        }
        if !dry {
            let _ = request(
                "PATCH",
                &format!("/pages/{page}"),
                tok,
                Some(&json!({ "archived": true })),
            );
            ledger_forget(&key)?;
        }
        r.archived_rows += 1;
    }
    Ok(())
}

/// Archive the goals board in Notion (recoverable from Notion's trash) and forget it locally,
/// with its ledger rows, so the next sync starts a fresh board.
pub fn archive_board() -> Result<()> {
    let tok = crate::notion::token().ok_or_else(|| anyhow!("Notion is not connected"))?;
    let Some(db) = crate::notion::setting(KV_DB) else {
        bail!("No goals board yet");
    };
    request(
        "PATCH",
        &format!("/databases/{db}"),
        &tok,
        Some(&json!({ "archived": true })),
    )?;
    crate::notion::set_setting(KV_DB, "");
    ledger_forget_prefix(LEDGER_PREFIX)?;
    ledger_forget_prefix(DECISION_PREFIX)?;
    Ok(())
}

/// Sync the board now when it exists and Notion is connected (goal completion, the digest), so
/// the row is current without waiting for the scheduler.
pub fn refresh(v: &Vault) {
    if crate::notion::setting(KV_DB).is_some() && crate::notion::token().is_some() {
        let _ = sync(v, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(props: Value) -> Value {
        json!({ "id": "p1", "created_by": { "id": "AUTHOR" }, "last_edited_by": { "id": "Editor" }, "properties": props })
    }
    fn rt(s: &str) -> Value {
        json!({ "rich_text": [{ "plain_text": s }] })
    }
    fn esc(id: &str, goal: &str, status: &str, created_at: i64) -> Escalation {
        Escalation {
            id: id.into(),
            goal_id: Some(goal.into()),
            mission_id: Some("M-001".into()),
            kind: "merge".into(),
            question: "Merge M-001?".into(),
            options: vec!["merge".into(), "skip".into()],
            safe_answer: "skip".into(),
            status: status.into(),
            answer: None,
            answered_by: None,
            created_at,
            deadline_at: created_at + 3600,
            answered_at: None,
        }
    }
    fn goal() -> Goal {
        Goal {
            id: "G-001".into(),
            outcome: "Add a probe endpoint".into(),
            repos: vec!["demo".into()],
            checks: vec![Check {
                kind: "command".into(),
                target: "npm test".into(),
                repo: None,
            }],
            budget_usd: 2.0,
            deadline: None,
            autonomy: "ask".into(),
            status: "running".into(),
            note: String::new(),
            report: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn notion_may_propose_a_goal_and_decide_and_nothing_else() {
        let proposal = parse_row(&page(json!({
            "Goal": { "title": [{ "plain_text": "Add a probe endpoint" }] },
            "Repos": rt("demo, other demo"),
            "Done checks": rt("command: npm test\nfile: docs/PROBE.md @other"),
            "Budget USD": { "number": 1.5 },
            "ID": rt("")
        })))
        .unwrap();
        assert_eq!(proposal.author, "author");
        assert_eq!(proposal.editor, "editor");
        match classify(&proposal) {
            Inbound::Propose(g) => {
                assert_eq!(g.repos, vec!["demo".to_string(), "other".to_string()]);
                assert_eq!(g.checks.len(), 2);
                assert_eq!(g.checks[1].repo.as_deref(), Some("other"));
                assert_eq!(g.budget_usd, 1.5);
                assert_eq!(g.autonomy, None, "empty autonomy means the repo default");
            }
            x => panic!("{x:?}"),
        }
        let half = Row {
            checks: String::new(),
            ..proposal.clone()
        };
        assert_eq!(
            classify(&half),
            Inbound::Incomplete("Needs Done checks before Studio can create the goal".into())
        );
        let bad = Row {
            checks: "run npm test".into(),
            ..proposal.clone()
        };
        assert!(matches!(classify(&bad), Inbound::Incomplete(n) if n.contains("needs a kind")));
        let bad_level = Row {
            autonomy: "yolo".into(),
            ..proposal.clone()
        };
        assert!(matches!(classify(&bad_level), Inbound::Incomplete(_)));

        let decided = parse_row(&page(json!({
            "Goal": { "title": [{ "plain_text": "x" }] }, "ID": rt("G-001"),
            "Escalation": rt("e-3"), "Decision": { "select": { "name": "Merge" } }
        })))
        .unwrap();
        assert_eq!(
            classify(&decided),
            Inbound::Decide("E-3".into(), "merge".into())
        );
        // A human setting Status to done or editing the checks in Notion is not a request.
        let edited = parse_row(&page(json!({
            "Goal": { "title": [{ "plain_text": "x" }] }, "ID": rt("G-001"),
            "Status": { "select": { "name": "done" } }, "Done checks": rt("file: x")
        })))
        .unwrap();
        assert_eq!(classify(&edited), Inbound::Nothing);
        assert_eq!(classify(&Row::default()), Inbound::Nothing);
    }

    #[test]
    fn a_decision_needs_the_open_question_an_allowed_answer_and_a_trusted_editor() {
        let trusted = vec!["owner".to_string()];
        let e = esc("E-1", "G-001", "open", 10);
        let ok = |a: &str, ed: &str| decision_refusal("G-001", "E-1", a, Some(&e), ed, &trusted);
        assert_eq!(ok("merge", "owner"), None);
        assert!(ok("merge", "stranger").unwrap().contains("not a trusted"));
        assert!(ok("merge", "").unwrap().contains("unknown editor"));
        assert!(ok("retry", "owner").unwrap().contains("not an answer"));
        let other = esc("E-1", "G-002", "open", 10);
        assert!(
            decision_refusal("G-001", "E-1", "merge", Some(&other), "owner", &trusted)
                .unwrap()
                .contains("another goal")
        );
        let done = Escalation {
            answer: Some("skip".into()),
            ..esc("E-1", "G-001", "expired", 10)
        };
        assert!(
            decision_refusal("G-001", "E-1", "merge", Some(&done), "owner", &trusted)
                .unwrap()
                .contains("already expired (skip)")
        );
        assert!(
            decision_refusal("G-001", "E-9", "merge", None, "owner", &trusted)
                .unwrap()
                .contains("not a Studio escalation")
        );
        assert!(
            decision_refusal("G-001", "", "merge", None, "owner", &trusted)
                .unwrap()
                .contains("no open question")
        );
    }

    #[test]
    fn the_row_shows_the_oldest_open_question_and_hashes_stably() {
        let open = vec![
            esc("E-3", "G-001", "open", 30),
            esc("E-2", "G-001", "open", 20),
            esc("E-1", "G-002", "open", 10),
            esc("E-0", "G-001", "answered", 5),
        ];
        assert_eq!(open_question("G-001", &open).unwrap().id, "E-2");
        assert!(open_question("G-009", &open).is_none());
        let g = goal();
        let p = row_props(&g, &[], open_question("G-001", &open));
        let schema = schema();
        for k in p.as_object().unwrap().keys() {
            assert!(schema.get(k).is_some(), "{k} is in the schema");
        }
        assert_eq!(p["Escalation"]["rich_text"][0]["text"]["content"], "E-2");
        assert!(p["Question"]["rich_text"][0]["text"]["content"]
            .as_str()
            .unwrap()
            .contains("Answers: merge, skip"));
        assert_eq!(
            p["Done checks"]["rich_text"][0]["text"]["content"],
            "command: npm test"
        );
        assert_eq!(
            hash(&p.to_string()),
            hash(&row_props(&g, &[], open_question("G-001", &open)).to_string())
        );
        assert_ne!(
            hash(&p.to_string()),
            hash(&row_props(&g, &[], None).to_string()),
            "a question being answered changes the row"
        );
        for d in DECISIONS {
            assert!(schema["Decision"]["select"]["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["name"] == d));
        }
    }

    #[test]
    fn every_column_has_one_owner_and_studio_never_writes_a_notion_column() {
        let schema = schema();
        let cols: Vec<&String> = schema.as_object().unwrap().keys().collect();
        assert_eq!(cols.len(), OWNERS.len());
        for c in &cols {
            assert_eq!(
                OWNERS.iter().filter(|(k, _)| k == c).count(),
                1,
                "{c} has exactly one owner"
            );
        }
        let notion_owned: Vec<&str> = OWNERS
            .iter()
            .filter(|(_, o)| *o == FieldOwner::Notion)
            .map(|(k, _)| *k)
            .collect();
        let props = row_props(&goal(), &[], None);
        for k in props.as_object().unwrap().keys() {
            assert!(!notion_owned.contains(&k.as_str()), "Studio writes {k}");
        }
        // Handling a request clears it and writes nothing else of Notion's.
        let n = note_props("x");
        for (k, v) in n.as_object().unwrap() {
            if notion_owned.contains(&k.as_str()) {
                assert!(v["select"].is_null(), "{k} is only cleared");
            }
        }
    }

    #[test]
    fn checks_round_trip_through_the_board_text() {
        let checks = vec![
            Check {
                kind: "url".into(),
                target: "http://127.0.0.1:8080/probe".into(),
                repo: None,
            },
            Check {
                kind: "file".into(),
                target: "docs/PROBE.md".into(),
                repo: Some("demo".into()),
            },
        ];
        assert_eq!(parse_checks(&checks_text(&checks)).unwrap(), checks);
        assert!(parse_checks("shell: rm -rf /")
            .unwrap_err()
            .contains("not one of"));
        assert_eq!(
            parse_checks("- command: npm test").unwrap()[0].target,
            "npm test"
        );
    }

    #[test]
    fn without_a_token_nothing_is_sent() {
        let tmp = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(tmp.path().join("studio.db")));
        let r = sync_inner(&Vault::new(tmp.path()), false, None, None, false);
        crate::store::set_test_db(None);
        assert!(r.dry_run && !r.connected && r.database.is_none());
        assert_eq!(r.created_rows, 0);
    }
}
