//! Registered-agent reads over one existing Studio store snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use ed25519_dalek::{Signature, Signer, SigningKey};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{constitution_gate, goals, hub_identity, missions, permissions, store};

pub const TOOLS: [&str; 6] = [
    "studio_status",
    "list_goals",
    "get_goal",
    "list_missions",
    "get_mission",
    "list_notices",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadError {
    pub code: &'static str,
    pub message: &'static str,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ReadError {}

const INVALID: ReadError = ReadError {
    code: "INVALID_ARGUMENT",
    message: "Invalid Studio read arguments or cursor.",
};
const UNAVAILABLE: ReadError = ReadError {
    code: "STORE_UNAVAILABLE",
    message: "Studio store is unavailable or contains invalid data.",
};
const NARROW: ReadError = ReadError {
    code: "STORE_UNAVAILABLE",
    message: "Studio read scan limit reached; narrow the filters.",
};
const FORBIDDEN: ReadError = ReadError {
    code: "FORBIDDEN",
    message: "Studio read access is forbidden.",
};
const APPROVAL: ReadError = ReadError {
    code: "APPROVAL_REQUIRED",
    message: "Studio read access requires approval.",
};
const NOT_FOUND: ReadError = ReadError {
    code: "NOT_FOUND",
    message: "Studio item was not found.",
};
const MAX_SCAN: usize = 2000;
const MAX_EPOCH: i64 = 253_402_300_799;
const WEEK: i64 = 7 * 24 * 3600;

type ReadResult<T> = Result<T, ReadError>;

fn checked<T, E>(r: Result<T, E>) -> ReadResult<T> {
    r.map_err(|_| UNAVAILABLE)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdArgs {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoalListArgs {
    limit: Option<u32>,
    cursor: Option<String>,
    repo: Option<String>,
    status: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MissionListArgs {
    limit: Option<u32>,
    cursor: Option<String>,
    repo: Option<String>,
    status: Option<String>,
    goal_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoticeListArgs {
    limit: Option<u32>,
    cursor: Option<String>,
    repo: Option<String>,
}

#[derive(Default, Serialize)]
struct Filters {
    repo: Option<String>,
    status: Option<String>,
    goal_id: Option<String>,
}

struct PageArgs {
    limit: usize,
    cursor: Option<String>,
    filters: Filters,
}

enum Request {
    Status,
    Goal(String),
    Mission(String),
    List(PageArgs),
}

fn decode<T: DeserializeOwned>(v: &Value) -> ReadResult<T> {
    // A JSON object is required even for empty arguments.
    if !v.is_object() {
        return Err(INVALID);
    }
    serde_json::from_value(v.clone()).map_err(|_| INVALID)
}

fn identifier(s: &str, max: usize) -> bool {
    !s.is_empty() && s == s.trim() && s.chars().count() <= max && !s.chars().any(char::is_control)
}

fn input_id(s: String) -> ReadResult<String> {
    let s = s.trim();
    if !identifier(s, 256) {
        return Err(INVALID);
    }
    Ok(s.to_owned())
}

fn page_args(
    limit: Option<u32>,
    cursor: Option<String>,
    mut filters: Filters,
    statuses: &[&str],
) -> ReadResult<PageArgs> {
    let limit = limit.unwrap_or(25);
    if !(1..=100).contains(&limit) || cursor.as_ref().is_some_and(|c| c.len() > 4096) {
        return Err(INVALID);
    }
    filters.repo = filters.repo.map(input_id).transpose()?;
    filters.goal_id = filters.goal_id.map(input_id).transpose()?;
    if filters
        .status
        .as_ref()
        .is_some_and(|s| !statuses.contains(&s.as_str()))
    {
        return Err(INVALID);
    }
    Ok(PageArgs {
        limit: limit as usize,
        cursor,
        filters,
    })
}

fn request(name: &str, args: &Value) -> ReadResult<Request> {
    Ok(match name {
        "studio_status" => {
            let _: EmptyArgs = decode(args)?;
            Request::Status
        }
        "get_goal" => Request::Goal(input_id(decode::<IdArgs>(args)?.id)?),
        "get_mission" => Request::Mission(input_id(decode::<IdArgs>(args)?.id)?),
        "list_goals" => {
            let a: GoalListArgs = decode(args)?;
            Request::List(page_args(
                a.limit,
                a.cursor,
                Filters {
                    repo: a.repo,
                    status: a.status,
                    goal_id: None,
                },
                &goals::STATUSES,
            )?)
        }
        "list_missions" => {
            let a: MissionListArgs = decode(args)?;
            Request::List(page_args(
                a.limit,
                a.cursor,
                Filters {
                    repo: a.repo,
                    status: a.status,
                    goal_id: a.goal_id,
                },
                &missions::STATUSES,
            )?)
        }
        "list_notices" => {
            let a: NoticeListArgs = decode(args)?;
            Request::List(page_args(
                a.limit,
                a.cursor,
                Filters {
                    repo: a.repo,
                    ..Filters::default()
                },
                &[],
            )?)
        }
        _ => return Err(INVALID),
    })
}

/// Read one existing-store snapshot with redacted output; null optionals are absent (limit 25).
pub fn execute(name: &str, args: &Value, token: Option<&str>) -> Result<Value, ReadError> {
    let req = request(name, args)?;
    let token = token
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(FORBIDDEN)?;
    let mut c = checked(store::open_existing_read_only())?;
    let tx = checked(c.transaction())?;
    // Pin the schema check, token, policies and data to the same SQLite read snapshot.
    let version: i64 = checked(tx.query_row("PRAGMA user_version", [], |r| r.get(0)))?;
    if version != store::SCHEMA_VERSION {
        return Err(UNAVAILABLE);
    }
    let agent = checked(hub_identity::agent_for_in(&tx, token))?.ok_or(FORBIDDEN)?;
    let mut ctx = Context {
        c: &tx,
        tool: name,
        agent: &agent,
        scanned: 0,
        parent_scopes: BTreeMap::new(),
        policy_decisions: BTreeMap::new(),
    };
    let now = chrono::Utc::now().timestamp();
    let result = match req {
        Request::Status => status(&mut ctx, now)?,
        Request::Goal(id) => {
            let e = ctx.goal_entry(&id, true)?.ok_or(NOT_FOUND)?;
            ctx.authorize(&e.repos)?;
            payload(json!({"goal":e.value}), 16384)
        }
        Request::Mission(id) => {
            let e = ctx.mission_entry(&id, true)?.ok_or(NOT_FOUND)?;
            ctx.authorize(&e.repos)?;
            payload(json!({"mission":e.value}), 16384)
        }
        Request::List(a) => list(&mut ctx, a, now)?,
    };
    checked(tx.commit())?;
    Ok(result)
}

struct Context<'a> {
    c: &'a Connection,
    tool: &'a str,
    agent: &'a str,
    scanned: usize,
    parent_scopes: BTreeMap<String, (Vec<String>, Vec<String>)>,
    policy_decisions: BTreeMap<Option<String>, ReadResult<()>>,
}

impl Context<'_> {
    fn charge(&mut self) -> ReadResult<()> {
        if self.scanned >= MAX_SCAN {
            return Err(NARROW);
        }
        self.scanned += 1;
        Ok(())
    }

    fn authorize(&mut self, repos: &[String]) -> ReadResult<()> {
        let mut denied = false;
        let mut asks = false;
        let scopes: Vec<Option<&str>> = if repos.is_empty() {
            vec![None]
        } else {
            repos.iter().map(|s| Some(s.as_str())).collect()
        };
        for repo in scopes {
            let key = repo.map(str::to_owned);
            let decision = if let Some(decision) = self.policy_decisions.get(&key) {
                *decision
            } else {
                self.charge()?;
                let decision = match checked(permissions::decide_scoped_checked_in(
                    self.c,
                    "agentos-hub",
                    self.tool,
                    Some(self.agent),
                    repo,
                ))?
                .as_str()
                {
                    "allow" => Ok(()),
                    "deny" => Err(FORBIDDEN),
                    "ask" => Err(APPROVAL),
                    _ => return Err(UNAVAILABLE),
                };
                self.policy_decisions.insert(key, decision);
                decision
            };
            match decision {
                Ok(()) => {}
                Err(FORBIDDEN) => denied = true,
                Err(APPROVAL) => asks = true,
                Err(e) => return Err(e),
            }
        }
        if denied {
            Err(FORBIDDEN)
        } else if asks {
            Err(APPROVAL)
        } else {
            Ok(())
        }
    }
}

fn status(ctx: &mut Context<'_>, now: i64) -> ReadResult<Value> {
    ctx.authorize(&[])?;
    let raw = checked(store::kv_get(ctx.c, "fleet.paused"))?;
    let fleet = match raw {
        None => json!({"paused":false,"since":null,"reason":""}),
        Some(s) => {
            let (at, reason) = s.split_once('|').ok_or(UNAVAILABLE)?;
            if at.is_empty() || !at.bytes().all(|b| b.is_ascii_digit()) {
                return Err(UNAVAILABLE);
            }
            let at = at.parse::<i64>().map_err(|_| UNAVAILABLE)?;
            epoch(at)?;
            json!({"paused":true,"since":at,"reason":reason})
        }
    };
    Ok(payload(
        json!({"server_version":env!("CARGO_PKG_VERSION"),"schema_version":store::SCHEMA_VERSION,
        "observed_at":now,"identity":ctx.agent,"fleet":fleet,"gui_liveness":"unavailable"}),
        4096,
    ))
}

fn sanitize(v: &mut Value, limit: usize, truncated: &mut bool) {
    match v {
        Value::String(s) => {
            let redacted = constitution_gate::redact(s);
            *truncated |= redacted.chars().count() > limit;
            *s = redacted.chars().take(limit).collect();
        }
        Value::Array(a) => {
            for v in a {
                sanitize(v, limit, truncated);
            }
        }
        Value::Object(o) => {
            for v in o.values_mut() {
                sanitize(v, limit, truncated);
            }
        }
        _ => {}
    }
}

fn payload(mut v: Value, limit: usize) -> Value {
    let mut truncated = false;
    sanitize(&mut v, limit, &mut truncated);
    v["truncated"] = json!(truncated);
    v
}

fn epoch(at: i64) -> ReadResult<()> {
    if !(0..=MAX_EPOCH).contains(&at) {
        return Err(UNAVAILABLE);
    }
    Ok(())
}

fn stored_id(id: &str) -> ReadResult<()> {
    if !identifier(id, 256) || constitution_gate::redact(id) != id {
        return Err(UNAVAILABLE);
    }
    Ok(())
}

fn stored_json<T: DeserializeOwned>(s: &str) -> ReadResult<T> {
    checked(serde_json::from_str(s))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Check {
    kind: String,
    target: String,
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Serialize)]
struct Goal {
    id: String,
    outcome: String,
    repos: Vec<String>,
    checks: Vec<Check>,
    budget_usd: f64,
    deadline: Option<String>,
    autonomy: String,
    status: String,
    note: String,
    report: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl Context<'_> {
    fn goal(&mut self, id: &str) -> ReadResult<Option<Goal>> {
        self.charge()?;
        let raw = checked(self.c.query_row(
            "SELECT id,outcome,repos,checks,budget_usd,deadline,autonomy,status,note,report,created_at,updated_at FROM goals WHERE id=?1",
            [id], |r| Ok((Goal {
                id:r.get(0)?,outcome:r.get(1)?,repos:Vec::new(),checks:Vec::new(),budget_usd:r.get(4)?,
                deadline:r.get(5)?,autonomy:r.get(6)?,status:r.get(7)?,note:r.get(8)?,report:r.get(9)?,created_at:r.get(10)?,updated_at:r.get(11)?,
            },r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional())?;
        let Some((mut g, repos, checks)) = raw else {
            return Ok(None);
        };
        stored_id(&g.id)?;
        epoch(g.created_at)?;
        epoch(g.updated_at)?;
        if !g.budget_usd.is_finite()
            || !goals::STATUSES.contains(&g.status.as_str())
            || !goals::LEVELS.contains(&g.autonomy.as_str())
        {
            return Err(UNAVAILABLE);
        }
        g.repos = stored_json(&repos)?;
        g.checks = stored_json(&checks)?;
        if g.repos.len() > MAX_SCAN || g.checks.len() > MAX_SCAN {
            return Err(NARROW);
        }
        for r in &g.repos {
            self.charge()?;
            if !r.trim().is_empty() {
                stored_id(r)?;
            }
        }
        for c in &g.checks {
            self.charge()?;
            if !goals::CHECK_KINDS.contains(&c.kind.as_str()) || c.target.trim().is_empty() {
                return Err(UNAVAILABLE);
            }
            if let Some(r) = c.repo.as_deref().filter(|r| !r.trim().is_empty()) {
                stored_id(r)?;
            }
        }
        Ok(Some(g))
    }

    fn goal_scopes(&mut self, g: &Goal) -> ReadResult<(Vec<String>, Vec<String>)> {
        if let Some(scopes) = self.parent_scopes.get(&g.id) {
            return Ok(scopes.clone());
        }
        let mut repos = BTreeSet::new();
        for r in &g.repos {
            self.charge()?;
            if !r.trim().is_empty() {
                repos.insert(r.clone());
            }
        }
        for c in &g.checks {
            self.charge()?;
            if let Some(r) = c.repo.as_ref().filter(|r| !r.trim().is_empty()) {
                repos.insert(r.clone());
            }
        }
        let mut ids = Vec::new();
        // Match Rust's Unicode White_Space trimming, then reject noncanonical parent links.
        const WHITESPACE: &str = "\t\n\u{b}\u{c}\r \u{85}\u{a0}\u{1680}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}";
        let mut st = checked(self.c.prepare(
            "SELECT id,repo,goal_id FROM missions WHERE trim(goal_id,?2)=?1 ORDER BY id LIMIT 2001",
        ))?;
        let mut rows = checked(st.query(params![g.id, WHITESPACE]))?;
        while let Some(r) = checked(rows.next())? {
            self.charge()?;
            let id: String = checked(r.get(0))?;
            let repo: String = checked(r.get(1))?;
            let parent: String = checked(r.get(2))?;
            stored_id(&id)?;
            stored_id(&repo)?;
            if parent != g.id {
                return Err(UNAVAILABLE);
            }
            ids.push(id);
            repos.insert(repo);
        }
        if repos.is_empty() {
            return Err(UNAVAILABLE);
        }
        let scopes = (repos.into_iter().collect(), ids);
        self.parent_scopes.insert(g.id.clone(), scopes.clone());
        Ok(scopes)
    }
}

#[derive(Serialize)]
struct Mission {
    id: String,
    repo: String,
    goal: String,
    acceptance: String,
    role: String,
    agent: String,
    status: String,
    provider: String,
    model: String,
    budget_usd: f64,
    spent_usd: f64,
    task_id: Option<String>,
    reviewer: Option<String>,
    review_verdict: Option<String>,
    review_note: Option<String>,
    created_at: i64,
    updated_at: i64,
    pr_url: Option<String>,
    verify: Option<String>,
    kind: String,
    goal_id: Option<String>,
}

impl Context<'_> {
    fn mission(&mut self, id: &str) -> ReadResult<Option<Mission>> {
        self.charge()?;
        let m = checked(self.c.query_row("SELECT id,repo,goal,acceptance,role,agent,status,provider,model,budget_usd,spent_usd,task_id,reviewer,review_verdict,review_note,created_at,updated_at,pr_url,verify_json,kind,goal_id FROM missions WHERE id=?1",[id],|r|Ok(Mission {
            id:r.get(0)?,repo:r.get(1)?,goal:r.get(2)?,acceptance:r.get(3)?,role:r.get(4)?,agent:r.get(5)?,status:r.get(6)?,provider:r.get(7)?,model:r.get(8)?,budget_usd:r.get(9)?,spent_usd:r.get(10)?,task_id:r.get(11)?,reviewer:r.get(12)?,review_verdict:r.get(13)?,review_note:r.get(14)?,created_at:r.get(15)?,updated_at:r.get(16)?,pr_url:r.get(17)?,verify:r.get(18)?,kind:r.get(19)?,goal_id:r.get(20)?,
        })).optional())?;
        if let Some(m) = &m {
            stored_id(&m.id)?;
            stored_id(&m.repo)?;
            epoch(m.created_at)?;
            epoch(m.updated_at)?;
            if let Some(g) = &m.goal_id {
                stored_id(g)?;
            }
            if !missions::STATUSES.contains(&m.status.as_str())
                || !matches!(m.kind.as_str(), "work" | "decision")
                || !m.budget_usd.is_finite()
                || !m.spent_usd.is_finite()
            {
                return Err(UNAVAILABLE);
            }
            if let Some(v) = &m.verify {
                let _: Value = stored_json(v)?;
            }
        }
        Ok(m)
    }

    fn mission_scopes(&mut self, m: &Mission) -> ReadResult<Vec<String>> {
        let mut repos = BTreeSet::from([m.repo.clone()]);
        if let Some(id) = &m.goal_id {
            let parent_repos = if let Some((repos, _)) = self.parent_scopes.get(id) {
                repos.clone()
            } else {
                let g = self.goal(id)?.ok_or(UNAVAILABLE)?;
                self.goal_scopes(&g)?.0
            };
            repos.extend(parent_repos);
        }
        Ok(repos.into_iter().collect())
    }
}

struct Entry {
    id: String,
    at: i64,
    repos: Vec<String>,
    fingerprint: String,
    value: Value,
}

fn hash(v: &impl Serialize) -> ReadResult<String> {
    let bytes = checked(serde_json::to_vec(v))?;
    Ok(hex_bytes(&Sha256::digest(bytes)))
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Context<'_> {
    fn goal_entry(&mut self, id: &str, detail: bool) -> ReadResult<Option<Entry>> {
        let Some(g) = self.goal(id)? else {
            return Ok(None);
        };
        let (repos, ids) = self.goal_scopes(&g)?;
        let fingerprint = hash(&(&g, &repos, &ids))?;
        let mut value = if detail {
            checked(serde_json::to_value(&g))?
        } else {
            json!({"id":g.id,"outcome":g.outcome,"repos":g.repos,"status":g.status,"created_at":g.created_at,"updated_at":g.updated_at})
        };
        if detail {
            value["mission_ids"] = json!(ids);
        }
        Ok(Some(Entry {
            id: g.id,
            at: g.created_at,
            repos,
            fingerprint,
            value,
        }))
    }

    fn mission_entry(&mut self, id: &str, detail: bool) -> ReadResult<Option<Entry>> {
        let Some(m) = self.mission(id)? else {
            return Ok(None);
        };
        let repos = self.mission_scopes(&m)?;
        let fingerprint = hash(&(&m, &repos))?;
        let value = if detail {
            checked(serde_json::to_value(&m))?
        } else {
            json!({"id":m.id,"repo":m.repo,"goal":m.goal,"goal_id":m.goal_id,"status":m.status,"created_at":m.created_at,"updated_at":m.updated_at})
        };
        Ok(Some(Entry {
            id: m.id,
            at: m.created_at,
            repos,
            fingerprint,
            value,
        }))
    }
}

// Attributed notices use a cursor-pinned week; open work has no lower age bound.
const NOTICE_ROWS: &str = "
    SELECT 'goal.' || id AS id, updated_at AS at FROM goals
    WHERE status IN ('done','failed','awaiting') AND updated_at<=?1 AND (status='awaiting' OR updated_at>=?2)
    UNION ALL SELECT 'mission.' || id, updated_at FROM missions
    WHERE status IN ('blocked','failed','review') AND updated_at<=?1 AND (status!='failed' OR updated_at>=?2)
    UNION ALL SELECT 'question.' || id, created_at FROM escalations
    WHERE status='open' AND created_at<=?1 AND (goal_id IS NOT NULL OR mission_id IS NOT NULL)";

#[derive(Serialize)]
struct Escalation {
    id: String,
    goal_id: Option<String>,
    mission_id: Option<String>,
    kind: String,
    question: String,
    options: String,
    status: String,
    created_at: i64,
}

impl Context<'_> {
    fn notice_entry(&mut self, id: &str, window: i64) -> ReadResult<Option<Entry>> {
        let (source, key) = id.split_once('.').ok_or(INVALID)?;
        let (repos, kind, title, detail, at, open, goal_id, mission_id, escalation_id, fingerprint) =
            match source {
                "goal" => {
                    let Some(g) = self.goal(key)? else {
                        return Ok(None);
                    };
                    let open = g.status == "awaiting";
                    if !matches!(g.status.as_str(), "done" | "failed" | "awaiting")
                        || g.updated_at > window
                        || (!open && g.updated_at < window - WEEK)
                    {
                        return Ok(None);
                    }
                    let (repos, ids) = self.goal_scopes(&g)?;
                    let fingerprint = hash(&(&g, &repos, &ids))?;
                    let (kind, title) = match g.status.as_str() {
                        "done" => ("goal_done", "Goal is finished"),
                        "failed" => ("goal_failed", "Goal failed"),
                        _ => ("goal_plan", "Goal has a plan to approve"),
                    };
                    let detail = if g.note.trim().is_empty() {
                        g.outcome
                    } else {
                        g.note
                    };
                    (
                        repos,
                        kind.to_string(),
                        title.to_string(),
                        detail,
                        g.updated_at,
                        open,
                        Some(g.id),
                        None,
                        None,
                        fingerprint,
                    )
                }
                "mission" => {
                    let Some(m) = self.mission(key)? else {
                        return Ok(None);
                    };
                    let open = m.status != "failed";
                    if !matches!(m.status.as_str(), "blocked" | "failed" | "review")
                        || m.updated_at > window
                        || (!open && m.updated_at < window - WEEK)
                    {
                        return Ok(None);
                    }
                    let repos = self.mission_scopes(&m)?;
                    let fingerprint = hash(&(&m, &repos))?;
                    let (kind, title) = match m.status.as_str() {
                        "blocked" => ("mission_blocked", "Mission is blocked"),
                        "failed" => ("mission_failed", "Mission failed"),
                        _ => ("review", "Mission is ready for review"),
                    };
                    let detail = if m.status == "blocked" {
                        m.review_note
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or(m.goal)
                    } else {
                        m.goal
                    };
                    (
                        repos,
                        kind.to_string(),
                        title.to_string(),
                        detail,
                        m.updated_at,
                        open,
                        m.goal_id,
                        Some(m.id),
                        None,
                        fingerprint,
                    )
                }
                "question" => {
                    self.charge()?;
                    let e = checked(self.c.query_row("SELECT id,goal_id,mission_id,kind,question,options,status,created_at FROM escalations WHERE id=?1",[key],|r|Ok(Escalation {
                    id:r.get(0)?,goal_id:r.get(1)?,mission_id:r.get(2)?,kind:r.get(3)?,question:r.get(4)?,options:r.get(5)?,status:r.get(6)?,created_at:r.get(7)?,
                })).optional())?;
                    let Some(e) = e else {
                        return Ok(None);
                    };
                    stored_id(&e.id)?;
                    epoch(e.created_at)?;
                    if e.status != "open"
                        || e.created_at > window
                        || (e.goal_id.is_none() && e.mission_id.is_none())
                    {
                        return Ok(None);
                    }
                    let options: Vec<String> = stored_json(&e.options)?;
                    if options.len() > MAX_SCAN {
                        return Err(NARROW);
                    }
                    let mut repos = BTreeSet::new();
                    if let Some(gid) = &e.goal_id {
                        stored_id(gid)?;
                        let g = self.goal(gid)?.ok_or(UNAVAILABLE)?;
                        repos.extend(self.goal_scopes(&g)?.0);
                    }
                    let mut goal_id = e.goal_id.clone();
                    if let Some(mid) = &e.mission_id {
                        stored_id(mid)?;
                        let m = self.mission(mid)?.ok_or(UNAVAILABLE)?;
                        if let (Some(a), Some(b)) = (&e.goal_id, &m.goal_id) {
                            if a != b {
                                return Err(UNAVAILABLE);
                            }
                        }
                        if goal_id.is_none() {
                            goal_id = m.goal_id.clone();
                        }
                        repos.extend(self.mission_scopes(&m)?);
                    }
                    let repos: Vec<String> = repos.into_iter().collect();
                    let fingerprint = hash(&(&e, &repos, &goal_id))?;
                    (
                        repos,
                        "question".to_string(),
                        "A question waits for you".to_string(),
                        format!("{}: {}", e.kind, e.question),
                        e.created_at,
                        true,
                        goal_id,
                        e.mission_id,
                        Some(e.id),
                        fingerprint,
                    )
                }
                _ => return Err(INVALID),
            };
        let value = json!({"id":id,"kind":kind,"title":title,"detail":detail,"at":at,"open":open,
            "source":{"goal_id":goal_id,"mission_id":mission_id,"escalation_id":escalation_id,"repos":repos}});
        Ok(Some(Entry {
            id: id.to_string(),
            at,
            repos,
            fingerprint,
            value,
        }))
    }

    fn entry(&mut self, id: &str, window: Option<i64>) -> ReadResult<Option<Entry>> {
        match self.tool {
            "list_goals" => self.goal_entry(id, false),
            "list_missions" => self.mission_entry(id, false),
            "list_notices" => self.notice_entry(id, window.ok_or(INVALID)?),
            _ => Err(INVALID),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    tool: String,
    caller: String,
    filters: String,
    window: Option<i64>,
    at: i64,
    id: String,
    fingerprint: String,
    signature: Vec<u8>,
}

fn cursor_key() -> &'static SigningKey {
    static KEY: OnceLock<SigningKey> = OnceLock::new();
    KEY.get_or_init(|| {
        SigningKey::from_bytes(&Sha256::digest(hub_identity::generate().as_bytes()).into())
    })
}

fn cursor_payload(c: &Cursor) -> ReadResult<Vec<u8>> {
    // Fixed field order authenticates all metadata without the signature itself.
    checked(serde_json::to_vec(&(
        &c.tool,
        &c.caller,
        &c.filters,
        c.window,
        c.at,
        &c.id,
        &c.fingerprint,
    )))
}

fn cursor_decode(s: &str) -> ReadResult<Cursor> {
    if s.is_empty()
        || s.len() > 4096
        || s.len() % 2 != 0
        || !s.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(INVALID);
    }
    let bytes: Result<Vec<_>, _> = s
        .as_bytes()
        .chunks_exact(2)
        .map(|p| {
            u8::from_str_radix(std::str::from_utf8(p).map_err(|_| INVALID)?, 16)
                .map_err(|_| INVALID)
        })
        .collect();
    let cursor: Cursor = serde_json::from_slice(&bytes?).map_err(|_| INVALID)?;
    let signature = Signature::from_slice(&cursor.signature).map_err(|_| INVALID)?;
    cursor_key()
        .verifying_key()
        .verify_strict(&cursor_payload(&cursor).map_err(|_| INVALID)?, &signature)
        .map_err(|_| INVALID)?;
    Ok(cursor)
}

fn matches_filters(tool: &str, e: &Entry, f: &Filters) -> bool {
    f.repo.as_ref().map_or(true, |r| {
        if tool == "list_missions" {
            e.value["repo"].as_str() == Some(r.as_str())
        } else {
            e.repos.contains(r)
        }
    }) && f
        .status
        .as_ref()
        .map_or(true, |s| e.value["status"].as_str() == Some(s.as_str()))
        && f.goal_id
            .as_ref()
            .map_or(true, |g| e.value["goal_id"].as_str() == Some(g.as_str()))
}

fn authorized(ctx: &mut Context<'_>, e: &Entry) -> ReadResult<bool> {
    match ctx.authorize(&e.repos) {
        Ok(()) => Ok(true),
        Err(e) if e == FORBIDDEN || e == APPROVAL => Ok(false),
        Err(e) => Err(e),
    }
}

fn list(ctx: &mut Context<'_>, args: PageArgs, now: i64) -> ReadResult<Value> {
    let filter_hash = hash(&(args.limit, &args.filters))?;
    let cursor = args.cursor.as_deref().map(cursor_decode).transpose()?;
    let window = if ctx.tool == "list_notices" {
        Some(cursor.as_ref().and_then(|c| c.window).unwrap_or(now))
    } else {
        None
    };
    if let Some(c) = &cursor {
        if c.tool != ctx.tool
            || c.caller != ctx.agent
            || c.filters != filter_hash
            || c.window != window
            || c.window.is_some_and(|w| w < 0 || w > now)
            || epoch(c.at).is_err()
            || !identifier(&c.id, 300)
            || c.fingerprint.len() != 64
        {
            return Err(INVALID);
        }
        // Never trust the cursor's authority, timestamp, filter membership or row fingerprint.
        let anchor = ctx.entry(&c.id, window)?.ok_or(INVALID)?;
        if anchor.at != c.at
            || anchor.fingerprint != c.fingerprint
            || !matches_filters(ctx.tool, &anchor, &args.filters)
            || !authorized(ctx, &anchor)?
        {
            return Err(INVALID);
        }
    }
    let at = cursor.as_ref().map(|c| c.at);
    let id = cursor.as_ref().map(|c| c.id.as_str());
    // Only fixed SQL fragments are interpolated. All caller-controlled values are parameters.
    let sql = if ctx.tool == "list_notices" {
        format!("SELECT id,at FROM ({NOTICE_ROWS}) WHERE (?3 IS NULL OR at<?3 OR (at=?3 AND id<?4)) ORDER BY at DESC,id DESC LIMIT 2001")
    } else {
        let (table, extra) = if ctx.tool == "list_goals" {
            ("goals", "")
        } else {
            (
                "missions",
                " AND (?4 IS NULL OR repo=?4) AND (?5 IS NULL OR goal_id=?5)",
            )
        };
        format!("SELECT id,created_at FROM {table} WHERE (?1 IS NULL OR created_at<?1 OR (created_at=?1 AND id<?2)) AND (?3 IS NULL OR status=?3){extra} ORDER BY created_at DESC,id DESC LIMIT 2001")
    };
    let mut st = checked(ctx.c.prepare(&sql))?;
    let mut rows = match ctx.tool {
        "list_notices" => checked(st.query(params![window, window.map(|w| w - WEEK), at, id]))?,
        "list_goals" => checked(st.query(params![at, id, args.filters.status]))?,
        _ => checked(st.query(params![
            at,
            id,
            args.filters.status,
            args.filters.repo,
            args.filters.goal_id
        ]))?,
    };
    let mut entries = Vec::new();
    let mut has_more = false;
    while let Some(row) = checked(rows.next())? {
        let id: String = checked(row.get(0))?;
        let at: i64 = checked(row.get(1))?;
        let e = ctx.entry(&id, window)?.ok_or(UNAVAILABLE)?;
        if e.at != at {
            return Err(UNAVAILABLE);
        }
        if !matches_filters(ctx.tool, &e, &args.filters) || !authorized(ctx, &e)? {
            continue;
        }
        if entries.len() == args.limit {
            has_more = true;
            break;
        }
        entries.push(e);
    }
    let next_cursor = if has_more {
        let e = entries.last().ok_or(UNAVAILABLE)?;
        let mut c = Cursor {
            tool: ctx.tool.to_string(),
            caller: ctx.agent.to_string(),
            filters: filter_hash,
            window,
            at: e.at,
            id: e.id.clone(),
            fingerprint: e.fingerprint.clone(),
            signature: Vec::new(),
        };
        c.signature = cursor_key().sign(&cursor_payload(&c)?).to_bytes().to_vec();
        let encoded = hex_bytes(&checked(serde_json::to_vec(&c))?);
        if encoded.len() > 4096 {
            return Err(UNAVAILABLE);
        }
        Some(encoded)
    } else {
        None
    };
    let key = match ctx.tool {
        "list_goals" => "goals",
        "list_missions" => "missions",
        _ => "notices",
    };
    let mut result =
        json!({key:entries.into_iter().map(|e|e.value).collect::<Vec<_>>(),"has_more":has_more});
    if let Some(w) = window {
        result["window"] = json!(w);
    }
    let mut result = payload(result, 4096);
    // Cursors are protocol data, not display strings. Do not truncate or redact their encoding.
    result["next_cursor"] = json!(next_cursor);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;
    use rusqlite::Connection;
    use serde_json::json;

    struct Fixture {
        c: Connection,
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("studio.db");
            let c = store::open_at(&path).unwrap();
            store::set_test_db(Some(path));
            store::kv_set(
                &c,
                "hub.token.qoder",
                &crate::hub_identity::hash("fixture-token"),
            )
            .unwrap();
            Self { c, dir }
        }
        fn call(&self, tool: &str, args: Value) -> Result<Value, ReadError> {
            execute(tool, &args, Some("fixture-token"))
        }
    }

    impl Fixture {
        fn goal(&self, id: &str, repos: Value, status: &str, at: i64) {
            self.c.execute("INSERT INTO goals (id,outcome,repos,checks,status,created_at,updated_at) VALUES (?1,'Ship feature',?2,'[]',?3,?4,?4)", rusqlite::params![id, repos.to_string(), status, at]).unwrap();
        }
        fn mission(&self, id: &str, repo: &str, goal: Option<&str>, status: &str, at: i64) {
            self.c.execute("INSERT INTO missions (id,repo,goal,role,goal_id,status,created_at,updated_at) VALUES (?1,?2,'Implement feature','implementer',?3,?4,?5,?5)", rusqlite::params![id, repo, goal, status, at]).unwrap();
        }
        fn rule(&self, scope: &str, key: &str, tool: &str, policy: &str) {
            crate::permissions::set_scoped_in(
                &self.c,
                scope,
                key,
                "agentos-hub",
                tool,
                Some(policy),
            )
            .unwrap();
        }
        fn escalation(&self, id: &str, goal: Option<&str>, mission: Option<&str>, at: i64) {
            self.c.execute("INSERT INTO escalations (id,goal_id,mission_id,kind,question,safe_answer,created_at,deadline_at) VALUES (?1,?2,?3,'budget','Continue?','stop',?4,?4)", rusqlite::params![id, goal, mission, at]).unwrap();
        }
    }

    fn ids(v: &Value, key: &str) -> Vec<String> {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_owned())
            .collect()
    }

    fn unhex(s: &str) -> Value {
        let b: Vec<u8> = s
            .as_bytes()
            .chunks_exact(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect();
        serde_json::from_slice(&b).unwrap()
    }

    fn hex(v: &Value) -> String {
        v.to_string().bytes().map(|b| format!("{b:02x}")).collect()
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            store::set_test_db(None);
        }
    }

    #[test]
    fn unknown_or_empty_token_is_forbidden() {
        let _f = Fixture::new();
        for token in [None, Some(""), Some(" "), Some("unknown")] {
            assert_eq!(
                execute("studio_status", &json!({}), token)
                    .unwrap_err()
                    .code,
                "FORBIDDEN"
            );
        }
    }

    #[test]
    fn status_reports_persisted_state_not_gui_liveness() {
        let f = Fixture::new();
        store::kv_set(&f.c, "fleet.paused", "1700000000|maintenance").unwrap();
        let v = f.call("studio_status", json!({})).unwrap();
        assert_eq!(v["identity"], "qoder");
        assert_eq!(v["schema_version"], store::SCHEMA_VERSION);
        assert_eq!(v["fleet"]["paused"], true);
        assert_eq!(v["fleet"]["since"], 1700000000_i64);
        assert_eq!(v["fleet"]["reason"], "maintenance");
        assert_eq!(v["gui_liveness"], "unavailable");
        assert!(v.get("running").is_none());
        assert!(v["observed_at"].is_i64());
    }

    #[test]
    fn registered_token_is_revalidated_on_every_call() {
        let f = Fixture::new();
        assert!(f.call("studio_status", json!({})).is_ok());
        store::kv_delete(&f.c, "hub.token.qoder").unwrap();
        assert_eq!(
            f.call("studio_status", json!({})).unwrap_err().code,
            "FORBIDDEN"
        );
    }

    #[test]
    fn checked_errors_and_incompatible_schema_are_sanitized() {
        let f = Fixture::new();
        for (key, value) in [
            ("perm.tool.agent.qoder", "private invalid JSON"),
            ("hub.token.hermes", "private corrupt hash"),
        ] {
            store::kv_set(&f.c, key, value).unwrap();
            let e = f.call("studio_status", json!({})).unwrap_err();
            assert_eq!(e.code, "STORE_UNAVAILABLE");
            assert!(!e.to_string().contains("private"));
            store::kv_delete(&f.c, key).unwrap();
        }
        f.c.execute_batch("DROP TABLE tool_policy").unwrap();
        assert_eq!(
            f.call("studio_status", json!({})).unwrap_err().code,
            "STORE_UNAVAILABLE"
        );
        for version in [0, store::SCHEMA_VERSION - 1, store::SCHEMA_VERSION + 1] {
            f.c.pragma_update(None, "user_version", version).unwrap();
            assert_eq!(
                f.call("list_goals", json!({})).unwrap_err().code,
                "STORE_UNAVAILABLE"
            );
        }
    }

    #[test]
    fn strict_inputs_and_null_optionals() {
        let f = Fixture::new();
        for (tool, args) in [
            ("studio_status", json!({"repo":"r"})),
            ("studio_status", json!([])),
            ("list_goals", json!({"goal_id":"G"})),
            ("list_notices", json!({"status":"open"})),
            ("get_goal", json!({"id":"  "})),
            ("get_mission", json!({"id":"x".repeat(257)})),
            ("list_goals", json!({"repo":"r".repeat(257)})),
            ("list_goals", json!({"limit":0})),
            ("list_goals", json!({"limit":101})),
            ("list_goals", json!({"limit":-1})),
            ("list_goals", json!({"limit":1.2})),
            ("list_goals", json!({"cursor":"f".repeat(4097)})),
            ("list_goals", json!({"status":"queued"})),
            ("list_missions", json!({"status":"planning"})),
            ("get_goal", json!({"id":null})),
            ("list_missions", json!({"goal_id":""})),
        ] {
            assert_eq!(
                f.call(tool, args).unwrap_err().code,
                "INVALID_ARGUMENT",
                "{tool}"
            );
        }
        assert!(f
            .call(
                "list_goals",
                json!({"limit":null,"repo":null,"cursor":null,"status":null})
            )
            .is_ok());
        f.goal("G", json!(["r"]), "running", 10);
        assert_eq!(
            f.call("get_goal", json!({"id":" G "})).unwrap()["goal"]["id"],
            "G"
        );
        assert_eq!(
            f.call("get_goal", json!({"id":"absent"})).unwrap_err().code,
            "NOT_FOUND"
        );
    }

    #[test]
    fn actual_repo_policies_override_agent_and_global_for_objects() {
        let f = Fixture::new();
        f.goal("G", json!(["actual"]), "running", 10);
        f.mission("M", "actual", None, "queued", 10);
        for tool in ["list_goals", "get_goal", "list_missions", "get_mission"] {
            crate::policy::set_in(&f.c, "agentos-hub", tool, "deny").unwrap();
            f.rule("agent", "qoder", tool, "ask");
            f.rule("repo", "actual", tool, "allow");
            f.rule("repo", "environment-decoy", tool, "deny");
        }
        assert_eq!(
            ids(&f.call("list_goals", json!({})).unwrap(), "goals"),
            ["G"]
        );
        assert_eq!(
            ids(&f.call("list_missions", json!({})).unwrap(), "missions"),
            ["M"]
        );
        assert!(f.call("get_goal", json!({"id":"G"})).is_ok());
        assert!(f.call("get_mission", json!({"id":"M"})).is_ok());
        f.rule("agent", "qoder", "studio_status", "ask");
        assert_eq!(
            f.call("studio_status", json!({})).unwrap_err().code,
            "APPROVAL_REQUIRED"
        );
        f.rule("repo", "actual", "get_goal", "deny");
        assert_eq!(
            f.call("get_goal", json!({"id":"G"})).unwrap_err().code,
            "FORBIDDEN"
        );
        f.rule("repo", "actual", "get_mission", "ask");
        assert_eq!(
            f.call("get_mission", json!({"id":"M"})).unwrap_err().code,
            "APPROVAL_REQUIRED"
        );
    }

    #[test]
    fn full_goal_scopes_cover_checks_siblings_and_parent_links() {
        let f = Fixture::new();
        f.goal("G", json!(["a", "b"]), "running", 10);
        f.c.execute(
            "UPDATE goals SET checks = ?1",
            [r#"[{"kind":"command","target":"test","repo":"c"}]"#],
        )
        .unwrap();
        f.mission("M", "a", Some("G"), "queued", 10);
        f.mission("sibling", "d", Some("G"), "queued", 11);
        for scope in ["b", "c", "d"] {
            for tool in ["get_goal", "get_mission", "list_goals", "list_missions"] {
                f.rule("repo", scope, tool, "deny");
            }
            assert_eq!(
                f.call("get_goal", json!({"id":"G"})).unwrap_err().code,
                "FORBIDDEN"
            );
            assert_eq!(
                f.call("get_mission", json!({"id":"M"})).unwrap_err().code,
                "FORBIDDEN"
            );
            assert!(ids(&f.call("list_goals", json!({})).unwrap(), "goals").is_empty());
            assert!(ids(&f.call("list_missions", json!({})).unwrap(), "missions").is_empty());
            for tool in ["get_goal", "get_mission", "list_goals", "list_missions"] {
                f.rule("repo", scope, tool, "allow");
            }
        }
        assert_eq!(
            f.call("get_goal", json!({"id":"G"})).unwrap()["goal"]["mission_ids"],
            json!(["M", "sibling"])
        );
        let m = f.call("get_mission", json!({"id":"M"})).unwrap();
        assert_eq!(m["mission"]["kind"], "work");
        assert_eq!(m["mission"]["goal_id"], "G");
    }

    #[test]
    fn malformed_json_and_orphan_links_fail_closed() {
        let f = Fixture::new();
        f.goal("G", json!(["a"]), "running", 10);
        for malformed in [
            "null",
            "{}",
            "[1]",
            "[\"a\",null]",
            "[\"a\",\" \"",
            "garbage",
        ] {
            f.c.execute("UPDATE goals SET repos=?1", [malformed])
                .unwrap();
            assert_eq!(
                f.call("get_goal", json!({"id":"G"})).unwrap_err().code,
                "STORE_UNAVAILABLE"
            );
        }
        f.c.execute("UPDATE goals SET repos='[\"a\"]'", []).unwrap();
        for checks in [
            "null",
            "{}",
            "[null]",
            r#"[{"kind":"file","target":"x","repo":3}]"#,
            r#"[{"kind":"file","target":"x","surprise":true}]"#,
        ] {
            f.c.execute("UPDATE goals SET checks=?1", [checks]).unwrap();
            assert_eq!(
                f.call("get_goal", json!({"id":"G"})).unwrap_err().code,
                "STORE_UNAVAILABLE"
            );
        }
        f.c.execute("UPDATE goals SET checks='[]'", []).unwrap();
        f.mission("orphan", "a", Some("missing"), "review", 10);
        assert_eq!(
            f.call("get_mission", json!({"id":"orphan"}))
                .unwrap_err()
                .code,
            "STORE_UNAVAILABLE"
        );
        f.c.execute("UPDATE missions SET goal_id='G', repo=' '", [])
            .unwrap();
        assert_eq!(
            f.call("get_goal", json!({"id":"G"})).unwrap_err().code,
            "STORE_UNAVAILABLE"
        );
    }

    #[test]
    fn keyset_ties_hide_denied_rows_and_only_encode_returned_anchor() {
        let f = Fixture::new();
        for (id, repo) in [
            ("z-denied", "secret"),
            ("c", "r"),
            ("b-ask", "ask"),
            ("b", "r"),
            ("a", "r"),
        ] {
            f.goal(id, json!([repo]), "running", 10);
        }
        f.rule("repo", "secret", "list_goals", "deny");
        f.rule("repo", "ask", "list_goals", "ask");
        let first = f.call("list_goals", json!({"limit":1})).unwrap();
        assert_eq!(ids(&first, "goals"), ["c"]);
        assert_eq!(first["has_more"], true);
        let cursor = first["next_cursor"].as_str().unwrap();
        let decoded = unhex(cursor);
        assert_eq!(decoded["id"], "c");
        assert!(!decoded.to_string().contains("denied"));
        assert!(!first.to_string().contains("total"));
        assert!(!first.to_string().contains("count"));
        f.goal("new", json!(["r"]), "running", 11);
        let second = f
            .call("list_goals", json!({"limit":1,"cursor":cursor}))
            .unwrap();
        assert_eq!(ids(&second, "goals"), ["b"]);
        let third = f
            .call(
                "list_goals",
                json!({"limit":1,"cursor":second["next_cursor"]}),
            )
            .unwrap();
        assert_eq!(ids(&third, "goals"), ["a"]);
        assert_eq!(third["has_more"], false);
        assert!(third["next_cursor"].is_null());
    }

    #[test]
    fn cursors_bind_filters_caller_endpoint_and_live_anchor_authorization() {
        let f = Fixture::new();
        f.goal("b", json!(["r"]), "running", 10);
        f.goal("a", json!(["r"]), "running", 10);
        let page = f.call("list_goals", json!({"limit":1})).unwrap();
        let cursor = page["next_cursor"].clone();
        for (tool, args) in [
            ("list_goals", json!({"limit":1,"cursor":cursor,"repo":"r"})),
            (
                "list_goals",
                json!({"limit":1,"cursor":cursor,"status":"running"}),
            ),
            ("list_missions", json!({"limit":1,"cursor":cursor})),
            ("list_notices", json!({"limit":1,"cursor":cursor})),
        ] {
            assert_eq!(f.call(tool, args).unwrap_err().code, "INVALID_ARGUMENT");
        }
        store::kv_set(
            &f.c,
            "hub.token.hermes",
            &crate::hub_identity::hash("other"),
        )
        .unwrap();
        assert_eq!(
            execute(
                "list_goals",
                &json!({"limit":1,"cursor":cursor}),
                Some("other")
            )
            .unwrap_err()
            .code,
            "INVALID_ARGUMENT"
        );
        f.rule("repo", "r", "list_goals", "ask");
        assert_eq!(
            f.call("list_goals", json!({"limit":1,"cursor":cursor}))
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
        f.rule("repo", "r", "list_goals", "allow");
        f.c.execute("UPDATE goals SET created_at=9 WHERE id='b'", [])
            .unwrap();
        assert_eq!(
            f.call("list_goals", json!({"limit":1,"cursor":cursor}))
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
        f.c.execute("DELETE FROM goals WHERE id='b'", []).unwrap();
        assert_eq!(
            f.call("list_goals", json!({"limit":1,"cursor":cursor}))
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn mission_filters_and_summary_fields_are_useful() {
        let f = Fixture::new();
        f.goal("G", json!(["r"]), "running", 10);
        f.mission("M2", "r", Some("G"), "queued", 10);
        f.mission("M1", "r", Some("G"), "queued", 10);
        f.mission("other", "x", None, "queued", 11);
        let p = f
            .call(
                "list_missions",
                json!({"repo":" r ","goal_id":"G","status":"queued","limit":1}),
            )
            .unwrap();
        assert_eq!(ids(&p, "missions"), ["M2"]);
        let m = &p["missions"][0];
        for key in [
            "repo",
            "goal",
            "status",
            "goal_id",
            "created_at",
            "updated_at",
        ] {
            assert!(!m[key].is_null());
        }
        let p2 = f.call("list_missions",json!({"repo":"r","goal_id":"G","status":"queued","limit":1,"cursor":p["next_cursor"]})).unwrap();
        assert_eq!(ids(&p2, "missions"), ["M1"]);
    }

    #[test]
    fn notices_require_both_attributions_and_keep_old_open_items() {
        let f = Fixture::new();
        f.goal("good", json!(["r"]), "awaiting", 1);
        f.goal("old-finished", json!(["r"]), "done", 1);
        f.goal("hidden", json!(["hidden"]), "running", 1);
        f.mission("review", "r", None, "review", 1);
        f.mission("blocked", "r", None, "blocked", 1);
        f.mission("old-failed", "r", None, "failed", 1);
        f.escalation("good-question", Some("good"), Some("review"), 1);
        f.escalation("hidden-question", Some("hidden"), Some("review"), 1);
        f.escalation("global", None, None, 1);
        f.rule("repo", "hidden", "list_notices", "deny");
        store::kv_set(&f.c, "notices.read_until", "9999999999").unwrap();
        store::kv_set(&f.c, "provider.health", "private provider diagnostic").unwrap();
        let p = f.call("list_notices", json!({"repo":"r"})).unwrap();
        let ns = p["notices"].as_array().unwrap();
        assert_eq!(ns.len(), 4);
        assert!(ns.iter().all(|n| n["open"] == true));
        assert!(ns
            .iter()
            .any(|n| n["source"]["escalation_id"] == "good-question"));
        for n in ns {
            assert!(n.get("unread").is_none());
            assert_eq!(n["source"]["repos"], json!(["r"]));
        }
        for secret in [
            "hidden",
            "global",
            "old-finished",
            "old-failed",
            "private provider",
        ] {
            assert!(!p.to_string().contains(secret));
        }
        f.escalation("orphan", Some("missing"), None, 2);
        assert_eq!(
            f.call("list_notices", json!({})).unwrap_err().code,
            "STORE_UNAVAILABLE"
        );
    }

    #[test]
    fn notice_cursor_keeps_the_first_window_and_rejects_bad_order_keys() {
        let f = Fixture::new();
        let now = chrono::Utc::now().timestamp();
        f.goal("G2", json!(["r"]), "done", now);
        f.goal("G1", json!(["r"]), "failed", now - 1);
        let p = f.call("list_notices", json!({"limit":1})).unwrap();
        let cursor = p["next_cursor"].as_str().unwrap();
        let p2 = f
            .call("list_notices", json!({"limit":1,"cursor":cursor}))
            .unwrap();
        assert_eq!(p["window"], p2["window"]);
        assert_ne!(p["notices"][0]["id"], p2["notices"][0]["id"]);
        for bad in [
            "0".to_string(),
            "xx".to_string(),
            hex(&json!({})),
            "f".repeat(4096),
        ] {
            assert_eq!(
                f.call("list_notices", json!({"cursor":bad}))
                    .unwrap_err()
                    .code,
                "INVALID_ARGUMENT"
            );
        }
        for (key, value) in [("at", json!(-1)), ("id", json!("")), ("window", json!(-1))] {
            let mut c = unhex(cursor);
            c[key] = value;
            assert_eq!(
                f.call("list_notices", json!({"limit":1,"cursor":hex(&c)}))
                    .unwrap_err()
                    .code,
                "INVALID_ARGUMENT"
            );
        }
    }

    #[test]
    fn redaction_precedes_character_truncation_in_summaries_and_details() {
        let f = Fixture::new();
        f.goal("G", json!(["r"]), "running", 10);
        for (tool, field, bound) in [
            ("list_goals", "outcome", 4096),
            ("get_goal", "report", 16384),
        ] {
            let text = format!(
                "{} ghp_{} remaining",
                "é".repeat(bound - 10),
                "A".repeat(40)
            );
            let sql = format!("UPDATE goals SET {field}=?1");
            f.c.execute(&sql, [text]).unwrap();
            let p = f
                .call(
                    tool,
                    if tool == "get_goal" {
                        json!({"id":"G"})
                    } else {
                        json!({})
                    },
                )
                .unwrap();
            let rendered = if tool == "get_goal" {
                &p["goal"][field]
            } else {
                &p["goals"][0][field]
            };
            let text = rendered.as_str().unwrap();
            assert_eq!(text.chars().count(), bound);
            assert!(!text.contains("ghp_"));
            assert_eq!(p["truncated"], true);
        }
    }

    #[test]
    fn scan_exhaustion_never_claims_a_complete_empty_result() {
        let f = Fixture::new();
        f.c.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2001) INSERT INTO goals (id,outcome,repos,created_at,updated_at) SELECT printf('G%04d',x),'hidden','[\"deny\"]',x,x FROM n").unwrap();
        f.rule("repo", "deny", "list_goals", "deny");
        let e = f.call("list_goals", json!({})).unwrap_err();
        assert_eq!(e.code, "STORE_UNAVAILABLE");
        assert!(e.message.contains("narrow"));
        assert!(f.call("list_goals", json!({"status":"done"})).is_ok());
    }

    #[test]
    fn related_mission_collection_is_bounded() {
        let f = Fixture::new();
        f.goal("G", json!(["r"]), "running", 10);
        f.c.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2001) INSERT INTO missions (id,repo,goal,role,goal_id,created_at,updated_at) SELECT printf('M%04d',x),'r','linked','implementer','G',x,x FROM n").unwrap();
        let e = f.call("get_goal", json!({"id":"G"})).unwrap_err();
        assert_eq!(e.code, "STORE_UNAVAILABLE");
        assert!(e.message.contains("narrow"));
    }

    #[test]
    fn reads_leave_all_logical_state_unchanged() {
        let f = Fixture::new();
        f.goal("G", json!(["r"]), "awaiting", 10);
        f.mission("M", "r", Some("G"), "review", 10);
        f.escalation("E", Some("G"), Some("M"), 10);
        let version: i64 =
            f.c.query_row("PRAGMA data_version", [], |r| r.get(0))
                .unwrap();
        let changes = f.c.total_changes();
        for tool in TOOLS {
            let args = match tool {
                "get_goal" => json!({"id":"G"}),
                "get_mission" => json!({"id":"M"}),
                _ => json!({}),
            };
            assert!(f.call(tool, args).unwrap().is_object());
        }
        assert_eq!(f.c.total_changes(), changes);
        assert_eq!(
            f.c.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            version
        );
        assert_eq!(
            f.c.query_row("SELECT count(*) FROM audit", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn pause_state_rejects_malformed_persisted_values() {
        let f = Fixture::new();
        for value in [
            "",
            "1700000000",
            "-1|reason",
            "+1|reason",
            " 1|reason",
            "bad|reason",
            "999999999999999999999|reason",
        ] {
            store::kv_set(&f.c, "fleet.paused", value).unwrap();
            assert_eq!(
                f.call("studio_status", json!({})).unwrap_err().code,
                "STORE_UNAVAILABLE"
            );
        }
        store::kv_delete(&f.c, "fleet.paused").unwrap();
        let p = f.call("studio_status", json!({})).unwrap();
        assert_eq!(p["fleet"]["paused"], false);
        assert!(p["fleet"].get("running").is_none());
    }

    #[test]
    fn a_mission_cursor_anchor_must_match_its_own_repo_filter() {
        let f = Fixture::new();
        f.goal("G", json!(["r"]), "running", 10);
        for (id, repo) in [("R2", "r"), ("R1", "r"), ("S2", "s"), ("S1", "s")] {
            f.mission(id, repo, Some("G"), "queued", 10);
        }
        let r = f
            .call("list_missions", json!({"limit":1,"repo":"r"}))
            .unwrap();
        let s = f
            .call("list_missions", json!({"limit":1,"repo":"s"}))
            .unwrap();
        let r_cursor = unhex(r["next_cursor"].as_str().unwrap());
        let mut offset = unhex(s["next_cursor"].as_str().unwrap());
        offset["filters"] = r_cursor["filters"].clone();
        assert_eq!(
            f.call(
                "list_missions",
                json!({"limit":1,"repo":"r","cursor":hex(&offset)})
            )
            .unwrap_err()
            .code,
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn object_policy_and_json_errors_never_fall_back_to_empty_or_allow() {
        let f = Fixture::new();
        f.goal("G", json!(["r"]), "running", 10);
        f.mission("M", "r", None, "queued", 10);
        for tool in ["get_goal", "list_goals", "get_mission", "list_missions"] {
            store::kv_set(&f.c, "perm.tool.repo.r", "corrupt scope policy").unwrap();
            let args = match tool {
                "get_goal" => json!({"id":"G"}),
                "get_mission" => json!({"id":"M"}),
                _ => json!({}),
            };
            assert_eq!(f.call(tool, args).unwrap_err().code, "STORE_UNAVAILABLE");
        }
        store::kv_delete(&f.c, "perm.tool.repo.r").unwrap();
        f.c.execute("UPDATE missions SET verify_json='not JSON'", [])
            .unwrap();
        assert_eq!(
            f.call("get_mission", json!({"id":"M"})).unwrap_err().code,
            "STORE_UNAVAILABLE"
        );
        f.c.execute("UPDATE missions SET verify_json='{}',goal_id=' G '", [])
            .unwrap();
        assert_eq!(
            f.call("get_goal", json!({"id":"G"})).unwrap_err().code,
            "STORE_UNAVAILABLE"
        );
        assert_eq!(
            f.call("get_mission", json!({"id":"M"})).unwrap_err().code,
            "STORE_UNAVAILABLE"
        );
    }

    #[test]
    fn notice_ties_use_unique_authorized_anchors_and_detect_changed_rows() {
        let f = Fixture::new();
        f.goal("G2", json!(["r"]), "awaiting", 1);
        f.goal("G1", json!(["r"]), "awaiting", 1);
        f.goal("hidden", json!(["s"]), "awaiting", 1);
        f.rule("repo", "s", "list_notices", "ask");
        let first = f.call("list_notices", json!({"limit":1})).unwrap();
        assert_eq!(first["notices"][0]["source"]["goal_id"], "G2");
        let cursor = first["next_cursor"].clone();
        assert!(!unhex(cursor.as_str().unwrap())
            .to_string()
            .contains("hidden"));
        let second = f
            .call("list_notices", json!({"limit":1,"cursor":cursor}))
            .unwrap();
        assert_eq!(second["notices"][0]["source"]["goal_id"], "G1");
        assert_eq!(second["has_more"], false);
        f.c.execute("UPDATE goals SET note='changed' WHERE id='G2'", [])
            .unwrap();
        assert_eq!(
            f.call("list_notices", json!({"limit":1,"cursor":cursor}))
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn notice_mission_attribution_cannot_be_bypassed_by_an_allowed_goal() {
        let f = Fixture::new();
        f.goal("G", json!(["r"]), "running", 1);
        f.mission("M", "secret", None, "running", 1);
        f.escalation("E", Some("G"), Some("M"), 1);
        for rule in ["deny", "ask"] {
            f.rule("repo", "secret", "list_notices", rule);
            let p = f.call("list_notices", json!({})).unwrap();
            assert!(p["notices"].as_array().unwrap().is_empty());
            assert_eq!(p["has_more"], false);
            assert!(p["next_cursor"].is_null());
        }
        f.rule("repo", "secret", "list_notices", "allow");
        assert_eq!(
            f.call("list_notices", json!({})).unwrap()["notices"][0]["source"]["repos"],
            json!(["r", "secret"])
        );
        f.c.execute("UPDATE escalations SET mission_id='absent'", [])
            .unwrap();
        assert_eq!(
            f.call("list_notices", json!({})).unwrap_err().code,
            "STORE_UNAVAILABLE"
        );
    }

    #[test]
    fn process_repo_is_not_an_authorization_scope() {
        let _lock = store::test_env_lock();
        struct RestoreRepo(Option<std::ffi::OsString>);
        impl Drop for RestoreRepo {
            fn drop(&mut self) {
                match &self.0 {
                    Some(value) => std::env::set_var("AGENTOS_REPO", value),
                    None => std::env::remove_var("AGENTOS_REPO"),
                }
            }
        }
        let _restore = RestoreRepo(std::env::var_os("AGENTOS_REPO"));
        std::env::set_var("AGENTOS_REPO", "decoy");
        let f = Fixture::new();
        f.mission("M", "actual", None, "queued", 10);
        f.rule("repo", "actual", "get_mission", "deny");
        f.rule("repo", "decoy", "get_mission", "allow");
        assert_eq!(
            f.call("get_mission", json!({"id":"M"})).unwrap_err().code,
            "FORBIDDEN"
        );
        f.rule("repo", "actual", "get_mission", "allow");
        f.rule("repo", "decoy", "get_mission", "deny");
        assert_eq!(
            f.call("get_mission", json!({"id":"M"})).unwrap()["mission"]["repo"],
            "actual"
        );
    }

    #[test]
    fn default_and_maximum_page_sizes_and_bound_parameters() {
        let f = Fixture::new();
        for n in 0..26 {
            f.goal(&format!("G{n:02}"), json!(["r"]), "running", 10);
        }
        let p = f.call("list_goals", json!({})).unwrap();
        assert_eq!(p["goals"].as_array().unwrap().len(), 25);
        assert_eq!(p["has_more"], true);
        assert_eq!(unhex(p["next_cursor"].as_str().unwrap())["id"], "G01");
        let all = f.call("list_goals", json!({"limit":100})).unwrap();
        assert_eq!(all["goals"].as_array().unwrap().len(), 26);
        assert_eq!(all["has_more"], false);
        assert_eq!(
            f.call("get_goal", json!({"id":"' OR 1=1 --"}))
                .unwrap_err()
                .code,
            "NOT_FOUND"
        );
    }

    #[test]
    fn page_scanning_stops_after_one_extra_authorized_row() {
        let f = Fixture::new();
        f.goal("G2", json!(["r"]), "running", 10);
        f.goal("G1", json!(["r"]), "running", 9);
        f.c.execute("INSERT INTO goals (id,outcome,repos,created_at,updated_at) VALUES (X'ff','bad key','[\"r\"]',1,1)",[]).unwrap();
        let p = f.call("list_goals", json!({"limit":1})).unwrap();
        assert_eq!(ids(&p, "goals"), ["G2"]);
        assert_eq!(p["has_more"], true);
        assert_eq!(
            f.call("list_goals", json!({"limit":1,"cursor":p["next_cursor"]}))
                .unwrap_err()
                .code,
            "STORE_UNAVAILABLE"
        );
    }

    #[test]
    fn cursor_metadata_edits_cannot_rebind_valid_anchors() {
        let f = Fixture::new();
        for id in ["a", "b", "c"] {
            f.goal(id, json!(["r"]), "running", 10);
            f.mission(id, "r", None, "queued", 10);
        }
        store::kv_set(&f.c, "hub.token.hermes", &hub_identity::hash("other")).unwrap();
        let first = f.call("list_goals", json!({"limit":1})).unwrap();
        let cursor = first["next_cursor"].as_str().unwrap();
        let original = unhex(cursor);
        let second = f
            .call("list_goals", json!({"limit":1,"cursor":cursor}))
            .unwrap();
        assert_eq!(ids(&second, "goals"), ["b"]);
        let filtered = f.call("list_goals", json!({"limit":1,"repo":"r"})).unwrap();
        let missions = f.call("list_missions", json!({"limit":1})).unwrap();
        let mut changes = Vec::new();
        let mut edited = original.clone();
        edited["filters"] = unhex(filtered["next_cursor"].as_str().unwrap())["filters"].clone();
        changes.push((
            "filters",
            "list_goals",
            "fixture-token",
            json!({"limit":1,"repo":"r","cursor":hex(&edited)}),
        ));
        let mut edited = original.clone();
        edited["caller"] = json!("hermes");
        changes.push((
            "caller",
            "list_goals",
            "other",
            json!({"limit":1,"cursor":hex(&edited)}),
        ));
        let mut edited = original.clone();
        let mission_cursor = unhex(missions["next_cursor"].as_str().unwrap());
        for key in ["tool", "fingerprint"] {
            edited[key] = mission_cursor[key].clone();
        }
        changes.push((
            "tool",
            "list_missions",
            "fixture-token",
            json!({"limit":1,"cursor":hex(&edited)}),
        ));
        let mut edited = original.clone();
        let later = unhex(second["next_cursor"].as_str().unwrap());
        for key in ["id", "at", "fingerprint"] {
            edited[key] = later[key].clone();
        }
        changes.push((
            "anchor",
            "list_goals",
            "fixture-token",
            json!({"limit":1,"cursor":hex(&edited)}),
        ));
        let mut edited = original;
        edited.as_object_mut().unwrap().remove("signature");
        changes.push((
            "unsigned",
            "list_goals",
            "fixture-token",
            json!({"limit":1,"cursor":hex(&edited)}),
        ));
        let errors: Vec<_> = changes
            .into_iter()
            .map(|(case, tool, token, args)| (case, execute(tool, &args, Some(token)).err()))
            .collect();
        assert_eq!(
            errors,
            vec![
                ("filters", Some(INVALID)),
                ("caller", Some(INVALID)),
                ("tool", Some(INVALID)),
                ("anchor", Some(INVALID)),
                ("unsigned", Some(INVALID))
            ]
        );
    }

    #[test]
    fn old_open_notice_anchor_cannot_change_the_authenticated_window() {
        let f = Fixture::new();
        f.goal("open2", json!(["r"]), "awaiting", 2);
        f.goal("open1", json!(["r"]), "awaiting", 1);
        f.goal("old-done", json!(["r"]), "done", 1);
        let first = f.call("list_notices", json!({"limit":1})).unwrap();
        let cursor = first["next_cursor"].as_str().unwrap();
        let next = f
            .call("list_notices", json!({"limit":1,"cursor":cursor}))
            .unwrap();
        assert_eq!(ids(&next, "notices"), ["goal.open1"]);
        assert_eq!(next["window"], first["window"]);
        let mut edited = unhex(cursor);
        edited["window"] = json!(10);
        assert_eq!(
            f.call("list_notices", json!({"limit":1,"cursor":hex(&edited)}))
                .err(),
            Some(INVALID)
        );
    }

    #[test]
    fn cursor_authentication_precedes_anchor_loading() {
        let f = Fixture::new();
        f.goal("b", json!(["r"]), "awaiting", 2);
        f.goal("a", json!(["r"]), "awaiting", 1);
        let page = f.call("list_notices", json!({"limit":1})).unwrap();
        let mut edited = unhex(page["next_cursor"].as_str().unwrap());
        edited["window"] = json!(10);
        f.c.execute("UPDATE goals SET repos='not JSON' WHERE id='b'", [])
            .unwrap();
        assert_eq!(
            f.call("list_notices", json!({"limit":1,"cursor":hex(&edited)}))
                .err(),
            Some(INVALID)
        );
    }

    #[test]
    fn repositories_and_checks_share_the_related_work_budget() {
        let f = Fixture::new();
        f.goal("G", json!(vec!["r"; 600]), "running", 1);
        assert!(f.call("get_goal", json!({"id":"G"})).is_ok());
        let checks = json!(vec![
            json!({"kind":"command","target":"test","repo":"r"});
            600
        ]);
        f.c.execute(
            "UPDATE goals SET repos='[]',checks=?1",
            [checks.to_string()],
        )
        .unwrap();
        assert!(f.call("get_goal", json!({"id":"G"})).is_ok());
        f.c.execute(
            "UPDATE goals SET repos=?1",
            [json!(vec!["r"; 600]).to_string()],
        )
        .unwrap();
        assert_eq!(f.call("get_goal", json!({"id":"G"})).err(), Some(NARROW));
    }

    #[test]
    fn uncached_policy_resolution_uses_the_related_work_budget() {
        let f = Fixture::new();
        let repos: Vec<_> = (0..600).map(|n| format!("r{n}")).collect();
        f.goal("G", json!(repos), "running", 1);
        assert!(f.call("get_goal", json!({"id":"G"})).is_ok());
        let checks = json!(vec![json!({"kind":"command","target":"test"}); 200]);
        f.c.execute("UPDATE goals SET checks=?1", [checks.to_string()])
            .unwrap();
        assert_eq!(f.call("get_goal", json!({"id":"G"})).err(), Some(NARROW));
    }

    #[test]
    fn minimum_mission_pages_reuse_validated_parent_and_sibling_scopes() {
        let f = Fixture::new();
        f.goal("G", json!(["r"]), "running", 1);
        f.c.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000) INSERT INTO missions (id,repo,goal,role,goal_id,status,created_at,updated_at) SELECT printf('M%04d',x),'r','linked','implementer','G','queued',x,x FROM n").unwrap();
        let mut args = json!({"limit":1,"repo":"r","goal_id":"G","status":"queued"});
        for expected in ["M1000", "M0999", "M0998"] {
            let page = f.call("list_missions", args.clone()).unwrap();
            assert_eq!(ids(&page, "missions"), [expected]);
            assert_eq!(page["has_more"], true);
            args["cursor"] = page["next_cursor"].clone();
        }
        f.c.execute(
            "UPDATE missions SET repo='hidden',status='running' WHERE id='M0001'",
            [],
        )
        .unwrap();
        f.c.execute("UPDATE missions SET status='running' WHERE id<'M0998'", [])
            .unwrap();
        f.rule("repo", "hidden", "list_missions", "deny");
        args.as_object_mut().unwrap().remove("cursor");
        assert!(ids(&f.call("list_missions", args).unwrap(), "missions").is_empty());
    }

    #[test]
    fn credential_shaped_goal_anchors_are_rejected() {
        let f = Fixture::new();
        let credential = format!("ghp_{}", "x".repeat(40));
        f.goal(&credential, json!(["r"]), "running", 10);
        f.goal("safe", json!(["r"]), "running", 1);
        assert_eq!(
            f.call("list_goals", json!({"limit":1})).err(),
            Some(UNAVAILABLE)
        );
        assert_eq!(
            f.call("get_goal", json!({"id":credential})).err(),
            Some(UNAVAILABLE)
        );
    }

    #[test]
    fn credential_shaped_mission_anchors_are_rejected() {
        let f = Fixture::new();
        let credential = format!("ghp_{}", "x".repeat(40));
        f.mission(&credential, "r", None, "queued", 10);
        f.mission("safe", "r", None, "queued", 1);
        assert_eq!(
            f.call("list_missions", json!({"limit":1})).err(),
            Some(UNAVAILABLE)
        );
        assert_eq!(
            f.call("get_mission", json!({"id":credential})).err(),
            Some(UNAVAILABLE)
        );
    }

    #[test]
    fn credential_shaped_notice_anchors_are_rejected_for_every_source() {
        let credential = format!("ghp_{}", "x".repeat(40));
        for source in ["goal", "mission", "question"] {
            let f = Fixture::new();
            f.goal("parent", json!(["r"]), "running", 1);
            f.goal("safe", json!(["r"]), "awaiting", 1);
            match source {
                "goal" => f.goal(&credential, json!(["r"]), "awaiting", 10),
                "mission" => f.mission(&credential, "r", None, "review", 10),
                _ => f.escalation(&credential, Some("parent"), None, 10),
            }
            assert_eq!(
                f.call("list_notices", json!({"limit":1})).err(),
                Some(UNAVAILABLE),
                "{source}"
            );
        }
    }

    #[test]
    fn credential_shaped_repository_scopes_are_rejected() {
        let credential = format!("ghp_{}", "x".repeat(40));
        for source in ["repos", "checks", "sibling", "mission"] {
            let f = Fixture::new();
            f.goal("G", json!(["r"]), "running", 1);
            match source {
                "repos" => {
                    f.c.execute(
                        "UPDATE goals SET repos=?1",
                        [json!([credential]).to_string()],
                    )
                    .unwrap();
                }
                "checks" => {
                    f.c.execute(
                        "UPDATE goals SET checks=?1",
                        [
                            json!([{"kind":"command","target":"test","repo":credential}])
                                .to_string(),
                        ],
                    )
                    .unwrap();
                }
                _ => f.mission("M", &credential, Some("G"), "queued", 1),
            }
            let (tool, id) = if source == "mission" {
                ("get_mission", "M")
            } else {
                ("get_goal", "G")
            };
            assert_eq!(
                f.call(tool, json!({"id":id})).err(),
                Some(UNAVAILABLE),
                "{source}"
            );
        }
    }

    #[test]
    fn rust_whitespace_padded_parent_links_cannot_hide_sibling_scopes() {
        let f = Fixture::new();
        f.goal("G", json!(["r"]), "awaiting", 1);
        f.mission("valid", "r", Some("G"), "queued", 1);
        f.mission("padded", "hidden", Some("G"), "queued", 1);
        assert!(f.call("get_goal", json!({"id":"G"})).is_ok());
        for ch in [
            '\t', '\n', '\u{b}', '\u{c}', '\r', ' ', '\u{85}', '\u{a0}', '\u{1680}', '\u{2000}',
            '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
            '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}',
            '\u{3000}',
        ] {
            f.c.execute(
                "UPDATE missions SET goal_id=?1 WHERE id='padded'",
                [format!("{ch}G{ch}")],
            )
            .unwrap();
            for (tool, args) in [
                ("get_goal", json!({"id":"G"})),
                ("get_mission", json!({"id":"valid"})),
                ("list_goals", json!({})),
                ("list_notices", json!({})),
            ] {
                assert_eq!(
                    f.call(tool, args).err(),
                    Some(UNAVAILABLE),
                    "{tool}: {ch:?}"
                );
            }
        }
    }

    #[test]
    fn missing_store_is_not_created() {
        let f = Fixture::new();
        let missing = f.dir.path().join("missing").join("studio.db");
        store::set_test_db(Some(missing.clone()));
        assert_eq!(
            f.call("studio_status", json!({})).unwrap_err().code,
            "STORE_UNAVAILABLE"
        );
        assert!(!missing.exists());
        assert!(!missing.parent().unwrap().exists());
    }
}
