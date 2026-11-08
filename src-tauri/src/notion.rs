//! Notion as a publishing surface.
//!
//! The vault is the source of truth. Notion is where other humans read it. Publishing is one way
//! except for a single inbound door, a database named "Inbox for agents".
//!
//! Every published object is recorded in `sync_ledger` with a content hash, so a second publish of
//! unchanged content costs nothing, and a changed note shows up as an update rather than a duplicate.
//!
//! No token is stored here. `NOTION_API_KEY` (or `NOTION_TOKEN`) comes from the environment, which
//! is where Hermes keeps secrets, and only the variable name is ever displayed.

use anyhow::{bail, Result};
use rusqlite::params;
use serde::Serialize;
use serde_json::{json, Value};

use crate::missions;
use crate::projects;
use crate::store;
use crate::vault::Vault;

const API: &str = "https://api.notion.com/v1";
const VERSION: &str = "2022-06-28";
pub const KV_PARENT: &str = "notion.parent";
pub const KV_INBOX: &str = "notion.inbox";

/// `KEY=value` from a dotenv file, without a dotenv dependency. Never logged.
pub fn parse_env(text: &str, key: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let (k, v) = l.split_once('=')?;
            if k.trim() != key {
                return None;
            }
            let v = v.trim().trim_matches('"').trim_matches('\'').trim();
            (!v.is_empty()).then(|| v.to_string())
        })
}

/// Hermes keeps secrets in `%LOCALAPPDATA%/hermes/.env`; Studio reads it by name, never copies it.
fn hermes_env(key: &str) -> Option<String> {
    let path = dirs::data_local_dir()?.join("hermes").join(".env");
    let text = std::fs::read_to_string(path).ok()?;
    parse_env(&text, key)
}

pub fn token() -> Option<String> {
    ["NOTION_API_KEY", "NOTION_TOKEN"]
        .iter()
        .find_map(|k| std::env::var(k).ok().or_else(|| hermes_env(k)))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn setting(key: &str) -> Option<String> {
    let c = store::open().ok()?;
    store::kv_get(&c, key)
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
}

pub fn set_setting(key: &str, value: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, key, value.trim());
    }
}

pub(crate) fn parent_id() -> Option<String> {
    std::env::var("NOTION_PARENT_PAGE")
        .ok()
        .or_else(|| hermes_env("NOTION_PARENT_PAGE"))
        .filter(|v| !v.trim().is_empty())
        .or_else(|| setting(KV_PARENT))
}

/// Stable content hash. Not cryptographic: it only has to notice that a page changed.
pub fn hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

#[derive(Debug, Clone, Serialize)]
pub struct PageStatus {
    pub title: String,
    pub hash: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PublishReport {
    pub connected: bool,
    pub dry_run: bool,
    pub pages: Vec<PageStatus>,
    pub error: Option<String>,
}

/// The vault content worth publishing, as title and plain text. Cheap to build, no network.
pub fn artifacts(v: &Vault) -> Vec<(String, String)> {
    let mut out = Vec::new();

    // Decision digest: the last seven days of decisions, grouped by project.
    let mut digest = String::from("Decisions in the last 7 days.\n\n");
    if let Ok(log) = std::fs::read_to_string(v.decision_log()) {
        let mut count = 0;
        for block in log.split("\n### ").skip(1) {
            let head = block.lines().next().unwrap_or("").trim();
            let recent = head
                .get(..10)
                .map(|d| d >= week_ago().as_str())
                .unwrap_or(false);
            if recent {
                digest.push_str(&format!("- {head}\n"));
                count += 1;
            }
        }
        if count == 0 {
            digest.push_str("- Nothing recorded this week.\n");
        }
    }
    out.push(("AgentOS decision digest".into(), digest));

    // Task status across the bus.
    let mut tasks = String::from("Agent Bus status.\n\n");
    let mut open = 0;
    for t in v.fetch_pending_tasks("all").unwrap_or_default() {
        tasks.push_str(&format!(
            "- {} [{}] {} ({})\n",
            t.id, t.status, t.title, t.assigned_to
        ));
        open += 1;
    }
    if open == 0 {
        tasks.push_str("- Nothing open on the bus.\n");
    }
    out.push(("AgentOS task status".into(), tasks));

    // Missions.
    let mut ms = String::from("Missions and their verdicts.\n\n");
    let list = missions::list(50).unwrap_or_default();
    if list.is_empty() {
        ms.push_str("- No missions yet.\n");
    }
    for m in list {
        ms.push_str(&format!(
            "- {} [{}] {} ({}), {}{}\n",
            m.id,
            m.status,
            m.goal,
            m.repo,
            m.review_verdict.unwrap_or_else(|| "no verdict".into()),
            m.pr_url.map(|u| format!(" {u}")).unwrap_or_default()
        ));
    }
    out.push(("AgentOS missions".into(), ms));

    // Projects and their folders, the closest thing to a fleet table that is cheap to read.
    let mut fleet = String::from("Projects, their folders and open work.\n\n");
    let plist = projects::list().unwrap_or_default();
    if plist.is_empty() {
        fleet.push_str("- No Hermes projects yet.\n");
    }
    for p in plist.iter().take(60) {
        fleet.push_str(&format!(
            "- {} ({}): {} folder(s), {} chat(s){}\n",
            p.name,
            p.slug,
            p.folders.len(),
            p.session_count,
            if p.archived { ", archived" } else { "" }
        ));
    }
    out.push(("AgentOS fleet".into(), fleet));

    out
}

fn week_ago() -> String {
    (chrono::Utc::now() - chrono::Duration::days(7))
        .format("%Y-%m-%d")
        .to_string()
}

fn blocks(text: &str) -> Value {
    let items: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(90)
        .map(|line| {
            let (kind, body) = if let Some(rest) = line.strip_prefix("- ") {
                ("bulleted_list_item", rest)
            } else if line.starts_with('#') {
                ("heading_2", line.trim_start_matches('#').trim())
            } else {
                ("paragraph", line)
            };
            json!({
                "object": "block",
                "type": kind,
                kind: { "rich_text": [{ "type": "text", "text": { "content": body.chars().take(1900).collect::<String>() } }] }
            })
        })
        .collect();
    Value::Array(items)
}

pub fn request(method: &str, path: &str, tok: &str, body: Option<&Value>) -> Result<Value> {
    let url = format!("{API}{path}");
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let req = ureq::http::Request::builder()
        .method(method)
        .uri(&url)
        .header("Authorization", format!("Bearer {tok}"))
        .header("Notion-Version", VERSION)
        .header("Content-Type", "application/json")
        .body(payload)?;
    match ureq::run(req) {
        Ok(res) => {
            let text = res.into_body().read_to_string()?;
            Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
        }
        Err(ureq::Error::StatusCode(code)) => bail!("Notion API {code}"),
        Err(e) => bail!("Notion API unreachable: {e}"),
    }
}

pub fn ledger_get(title: &str) -> Option<(String, String)> {
    let c = store::open().ok()?;
    c.query_row(
        "SELECT notion_id, content_hash FROM sync_ledger WHERE vault_path = ?1 AND direction = 'out'",
        params![title],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .ok()
}

pub(crate) fn ledger_put(title: &str, notion_id: &str, content_hash: &str) -> Result<()> {
    let c = store::open()?;
    c.execute(
        "INSERT INTO sync_ledger (vault_path, notion_id, content_hash, synced_at, direction)
         VALUES (?1, ?2, ?3, ?4, 'out')
         ON CONFLICT(vault_path, direction) DO UPDATE SET
           notion_id = excluded.notion_id, content_hash = excluded.content_hash,
           synced_at = excluded.synced_at",
        params![
            title,
            notion_id,
            content_hash,
            chrono::Utc::now().timestamp()
        ],
    )?;
    Ok(())
}

/// Replace a page's children, so a re-publish is an update and not an append.
fn replace_children(page: &str, text: &str, tok: &str) -> Result<()> {
    if let Ok(list) = request(
        "GET",
        &format!("/blocks/{page}/children?page_size=100"),
        tok,
        None,
    ) {
        if let Some(results) = list.get("results").and_then(|r| r.as_array()) {
            for b in results {
                if let Some(id) = b.get("id").and_then(|i| i.as_str()) {
                    let _ = request("DELETE", &format!("/blocks/{id}"), tok, None);
                }
            }
        }
    }
    request(
        "PATCH",
        &format!("/blocks/{page}/children"),
        tok,
        Some(&json!({ "children": blocks(text) })),
    )?;
    Ok(())
}

/// Publish every artifact. With `dry_run`, or with no token, nothing leaves the machine.
pub fn publish(v: &Vault, dry_run: bool) -> PublishReport {
    publish_inner(v, dry_run, token(), parent_id())
}

/// Split out so tests can prove the no-token path without depending on the machine's env file.
fn publish_inner(
    v: &Vault,
    dry_run: bool,
    tok: Option<String>,
    parent: Option<String>,
) -> PublishReport {
    let mut report = PublishReport {
        connected: tok.is_some() && parent.is_some(),
        dry_run: dry_run || tok.is_none() || parent.is_none(),
        ..Default::default()
    };

    for (title, text) in artifacts(v) {
        let hash_now = hash(&text);
        let existing = ledger_get(&title);
        let action = match &existing {
            Some((_, h)) if *h == hash_now => "unchanged",
            Some(_) => "updated",
            None => "created",
        };
        if report.dry_run || action == "unchanged" {
            report.pages.push(PageStatus {
                title,
                hash: hash_now,
                action: if action == "unchanged" {
                    "unchanged".into()
                } else {
                    format!("{action} (dry run)")
                },
            });
            continue;
        }
        let (Some(tok), Some(parent)) = (tok.as_deref(), parent.as_deref()) else {
            report.pages.push(PageStatus {
                title,
                hash: hash_now,
                action: "skipped: not connected".into(),
            });
            continue;
        };
        let res = match &existing {
            Some((page, _)) => replace_children(page, &text, tok).map(|_| page.clone()),
            None => request(
                "POST",
                "/pages",
                tok,
                Some(&json!({
                    "parent": { "page_id": parent },
                    "properties": { "title": { "title": [{ "text": { "content": title } }] } },
                    "children": blocks(&text)
                })),
            )
            .map(|v| {
                v.get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or_default()
                    .to_string()
            }),
        };
        match res {
            Ok(page) if !page.is_empty() => {
                let _ = ledger_put(&title, &page, &hash_now);
                report.pages.push(PageStatus {
                    title,
                    hash: hash_now,
                    action: action.into(),
                });
            }
            Ok(_) => report.error = Some(format!("{title}: Notion returned no page id")),
            Err(e) => report.error = Some(format!("{title}: {e}")),
        }
    }
    report
}

/// Pull the "Inbox for agents" database into `00_Inbox`. The only inbound path.
pub fn pull_inbox(v: &Vault) -> Result<usize> {
    let tok =
        token().ok_or_else(|| anyhow::anyhow!("Notion is not connected: set NOTION_API_KEY"))?;
    let db = setting(KV_INBOX)
        .or_else(|| std::env::var("NOTION_INBOX_DATABASE").ok())
        .ok_or_else(|| anyhow::anyhow!("No inbox database configured (notion.inbox)"))?;
    let res = request(
        "POST",
        &format!("/databases/{db}/query"),
        &tok,
        Some(&json!({ "page_size": 25 })),
    )?;
    let mut written = 0;
    for page in res
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let id = page.get("id").and_then(|i| i.as_str()).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let title = page
            .get("properties")
            .and_then(|p| p.as_object())
            .and_then(|p| {
                p.values()
                    .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("title"))
            })
            .and_then(|v| v.get("title"))
            .and_then(|t| t.as_array())
            .and_then(|a| a.first())
            .and_then(|t| t.get("plain_text"))
            .and_then(|t| t.as_str())
            .unwrap_or("Untitled")
            .to_string();
        let path = format!(
            "00_Inbox/{} ({}).md",
            slug_simple(&title),
            &id[..8.min(id.len())]
        );
        if v.read_note(&path).is_ok() {
            continue;
        }
        let body = format!(
            "---\ntitle: {title}\nnotion_id: {id}\nsource: notion-inbox\nstatus: new\n---\n\n# {title}\n\nPulled from the Notion inbox for an agent to pick up.\n"
        );
        v.write_note(&path, &body)?;
        written += 1;
    }
    Ok(written)
}

fn slug_simple(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
        .chars()
        .take(60)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_files_are_read_by_name_only() {
        let t = "# comment\nOTHER=1\nNOTION_API_KEY=\"secret\"\n";
        assert_eq!(parse_env(t, "NOTION_API_KEY").as_deref(), Some("secret"));
        assert_eq!(parse_env(t, "MISSING"), None);
        assert_eq!(parse_env("NOTION_API_KEY=\n", "NOTION_API_KEY"), None);
    }

    #[test]
    fn hash_notices_content_changes() {
        assert_eq!(hash("abc"), hash("abc"));
        assert_ne!(hash("abc"), hash("abd"));
    }

    #[test]
    fn blocks_become_notion_shapes() {
        let b = blocks("- one\n# Heading\nplain");
        let a = b.as_array().unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a[0]["type"], "bulleted_list_item");
        assert_eq!(a[1]["type"], "heading_2");
        assert_eq!(a[2]["type"], "paragraph");
    }

    #[test]
    fn artifacts_are_real_content_from_the_vault() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let v = Vault::new(d.path().join("vault"));
        v.scaffold().unwrap();
        let arts = artifacts(&v);
        let titles: Vec<&str> = arts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(titles.contains(&"AgentOS task status"));
        assert!(titles.contains(&"AgentOS missions"));
        assert!(arts.iter().all(|(_, body)| !body.trim().is_empty()));
        crate::store::set_test_db(None);
    }

    #[test]
    fn no_token_means_dry_run_and_nothing_is_sent() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        std::env::remove_var("NOTION_API_KEY");
        std::env::remove_var("NOTION_TOKEN");
        let v = Vault::new(d.path().join("vault"));
        v.scaffold().unwrap();
        let r = publish_inner(&v, false, None, None);
        assert!(r.dry_run);
        assert!(r.pages.iter().all(|p| p.action.contains("dry run")));
        crate::store::set_test_db(None);
    }
}
