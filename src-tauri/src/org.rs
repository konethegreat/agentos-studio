//! Workspaces > folders > sub-folders > chats, plus tags and file attachments.
//!
//! Hermes sessions stay in Hermes's `state.db`; this module stores placement only. A chat
//! is in the folder it was explicitly moved to, otherwise in the folder whose directory is
//! the longest prefix of the session's working directory (the rule Hermes projects use),
//! otherwise Unfiled. Folders can be linked to Hermes projects (`hermes project create`).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::hermes_engine::SessionSummary;
use crate::{permissions, procs, store};

#[derive(Serialize, Clone, Debug)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub sort: i64,
    pub archived: bool,
    pub created_at: i64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Folder {
    pub id: String,
    pub workspace_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub path: Option<String>,
    pub project_slug: Option<String>,
    pub pinned: bool,
    pub archived: bool,
    pub sort: i64,
    pub created_at: i64,
    pub tags: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Attachment {
    pub id: String,
    pub owner_kind: String,
    pub owner_id: String,
    pub path: String,
    pub label: Option<String>,
    pub added_at: i64,
    pub exists: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct HermesProject {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub primary_path: Option<String>,
    pub archived: bool,
    pub folders: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct PlacedChat {
    #[serde(flatten)]
    pub session: SessionSummary,
    pub folder_id: Option<String>,
    pub workspace_id: Option<String>,
    /// Placed by working directory rather than an explicit move.
    pub auto: bool,
    pub studio_pinned: bool,
    pub archived: bool,
    pub tags: Vec<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct OrgSnapshot {
    pub workspaces: Vec<Workspace>,
    pub folders: Vec<Folder>,
    pub attachments: Vec<Attachment>,
    pub tags: Vec<String>,
    pub projects: Vec<HermesProject>,
    pub chats: Vec<PlacedChat>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct FolderPatch {
    pub name: Option<String>,
    /// `Some("")` clears the directory.
    pub path: Option<String>,
    pub pinned: Option<bool>,
    pub archived: Option<bool>,
    /// Move: new workspace (with `parent_id`), or a new parent in the same workspace.
    pub workspace_id: Option<String>,
    /// `Some("")` moves to the workspace root.
    pub parent_id: Option<String>,
    pub sort: Option<i64>,
}

fn clean_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        bail!("Name cannot be empty");
    }
    if n.chars().count() > 120 {
        bail!("Name is too long (120 characters at most)");
    }
    Ok(n.to_string())
}

/// Lower-cased, forward-slash, no trailing slash form used for prefix matching.
pub fn norm_path(p: &str) -> String {
    let mut s = p.trim().replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    s
}

pub fn path_contains(dir: &str, cwd: &str) -> bool {
    let (d, c) = (norm_path(dir), norm_path(cwd));
    !d.is_empty() && (c == d || c.starts_with(&format!("{d}/")))
}

// ------------------------------------------------------------------ reads

fn tags_for(c: &Connection, kind: &str) -> Result<HashMap<String, Vec<String>>> {
    let mut stmt =
        c.prepare("SELECT item_id, tag FROM item_tags WHERE item_kind = ?1 ORDER BY tag")?;
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for row in stmt.query_map([kind], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (id, tag) = row?;
        map.entry(id).or_default().push(tag);
    }
    Ok(map)
}

pub fn workspaces(c: &Connection) -> Result<Vec<Workspace>> {
    let mut stmt = c.prepare("SELECT id, name, color, sort, archived, created_at FROM workspaces ORDER BY sort, created_at")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Workspace {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
                sort: r.get(3)?,
                archived: r.get::<_, i64>(4)? != 0,
                created_at: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn folders(c: &Connection) -> Result<Vec<Folder>> {
    let tags = tags_for(c, "folder")?;
    let mut stmt = c.prepare(
        "SELECT id, workspace_id, parent_id, name, path, project_slug, pinned, archived, sort, created_at \
         FROM folders ORDER BY pinned DESC, sort, name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Folder {
                id: r.get(0)?,
                workspace_id: r.get(1)?,
                parent_id: r.get(2)?,
                name: r.get(3)?,
                path: r.get(4)?,
                project_slug: r.get(5)?,
                pinned: r.get::<_, i64>(6)? != 0,
                archived: r.get::<_, i64>(7)? != 0,
                sort: r.get(8)?,
                created_at: r.get(9)?,
                tags: vec![],
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|mut f| {
            f.tags = tags.get(&f.id).cloned().unwrap_or_default();
            f
        })
        .collect())
}

pub fn attachments(c: &Connection) -> Result<Vec<Attachment>> {
    let mut stmt = c.prepare(
        "SELECT id, owner_kind, owner_id, path, label, added_at FROM attachments ORDER BY added_at",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let path: String = r.get(3)?;
            Ok(Attachment {
                id: r.get(0)?,
                owner_kind: r.get(1)?,
                owner_id: r.get(2)?,
                exists: Path::new(&path).exists(),
                path,
                label: r.get(4)?,
                added_at: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn all_tags(c: &Connection) -> Result<Vec<String>> {
    let mut stmt = c.prepare("SELECT name FROM tags ORDER BY name COLLATE NOCASE")?;
    let rows = stmt
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(rows)
}

struct Meta {
    folder_id: Option<String>,
    workspace_id: Option<String>,
    pinned: bool,
    archived: bool,
}

fn chat_meta(c: &Connection) -> Result<HashMap<String, Meta>> {
    let mut stmt =
        c.prepare("SELECT session_id, folder_id, workspace_id, pinned, archived FROM chat_meta")?;
    let mut map = HashMap::new();
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Meta {
                folder_id: r.get(1)?,
                workspace_id: r.get(2)?,
                pinned: r.get::<_, i64>(3)? != 0,
                archived: r.get::<_, i64>(4)? != 0,
            },
        ))
    })? {
        let (k, v) = row?;
        map.insert(k, v);
    }
    Ok(map)
}

/// Folder whose directory is the longest prefix of `cwd`.
pub fn auto_folder<'a>(folders: &'a [Folder], cwd: &str) -> Option<&'a Folder> {
    if cwd.trim().is_empty() {
        return None;
    }
    folders
        .iter()
        .filter(|f| f.path.as_deref().is_some_and(|p| path_contains(p, cwd)))
        .max_by_key(|f| norm_path(f.path.as_deref().unwrap_or("")).len())
}

pub fn place(
    c: &Connection,
    sessions: Vec<SessionSummary>,
    folders: &[Folder],
) -> Result<Vec<PlacedChat>> {
    let meta = chat_meta(c)?;
    let tags = tags_for(c, "chat")?;
    let by_id: HashMap<&str, &Folder> = folders.iter().map(|f| (f.id.as_str(), f)).collect();
    Ok(sessions
        .into_iter()
        .map(|s| {
            let m = meta.get(&s.id);
            let explicit = m
                .and_then(|m| m.folder_id.clone())
                .filter(|id| by_id.contains_key(id.as_str()));
            let (folder_id, auto) = match explicit {
                Some(id) => (Some(id), false),
                None => match auto_folder(folders, &s.cwd) {
                    Some(f) if m.and_then(|m| m.workspace_id.as_ref()).is_none() => {
                        (Some(f.id.clone()), true)
                    }
                    _ => (None, false),
                },
            };
            let workspace_id = folder_id
                .as_deref()
                .and_then(|id| by_id.get(id).map(|f| f.workspace_id.clone()))
                .or_else(|| m.and_then(|m| m.workspace_id.clone()));
            PlacedChat {
                tags: tags.get(&s.id).cloned().unwrap_or_default(),
                folder_id,
                workspace_id,
                auto,
                studio_pinned: m.is_some_and(|m| m.pinned),
                archived: m.is_some_and(|m| m.archived),
                session: s,
            }
        })
        .collect())
}

/// Hermes projects from `projects.db` (read-only; written only through `hermes project`).
pub fn hermes_projects() -> Vec<HermesProject> {
    let path = procs::hermes_home().join("projects.db");
    if !path.exists() {
        return vec![];
    }
    let Ok(c) = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return vec![];
    };
    let _ = c.busy_timeout(std::time::Duration::from_secs(3));
    let mut folders: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(mut stmt) =
        c.prepare("SELECT project_id, path FROM project_folders ORDER BY is_primary DESC")
    {
        if let Ok(rows) =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            for (pid, p) in rows.flatten() {
                folders.entry(pid).or_default().push(p);
            }
        }
    }
    let Ok(mut stmt) =
        c.prepare("SELECT id, slug, name, primary_path, archived FROM projects ORDER BY name")
    else {
        return vec![];
    };
    let rows = stmt.query_map([], |r| {
        Ok(HermesProject {
            id: r.get(0)?,
            slug: r.get(1)?,
            name: r.get(2)?,
            primary_path: r.get(3)?,
            archived: r.get::<_, i64>(4)? != 0,
            folders: vec![],
        })
    });
    match rows {
        Ok(r) => r
            .flatten()
            .map(|mut p| {
                p.folders = folders.remove(&p.id).unwrap_or_default();
                p
            })
            .collect(),
        Err(_) => vec![],
    }
}

pub fn snapshot_in(
    c: &Connection,
    sessions: Vec<SessionSummary>,
    projects: Vec<HermesProject>,
) -> Result<OrgSnapshot> {
    let folders = folders(c)?;
    let chats = place(c, sessions, &folders)?;
    Ok(OrgSnapshot {
        workspaces: workspaces(c)?,
        attachments: attachments(c)?,
        tags: all_tags(c)?,
        projects,
        chats,
        folders,
    })
}

// ------------------------------------------------------------------ writes

pub fn create_workspace(c: &Connection, name: &str) -> Result<Workspace> {
    let name = clean_name(name)?;
    let id = store::new_id();
    let sort: i64 = c.query_row(
        "SELECT COALESCE(MAX(sort), 0) + 1 FROM workspaces",
        [],
        |r| r.get(0),
    )?;
    c.execute(
        "INSERT INTO workspaces (id, name, sort, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, name, sort, store::now()],
    )?;
    workspaces(c)?
        .into_iter()
        .find(|w| w.id == id)
        .ok_or_else(|| anyhow!("workspace vanished"))
}

pub fn update_workspace(
    c: &Connection,
    id: &str,
    name: Option<&str>,
    archived: Option<bool>,
    color: Option<&str>,
) -> Result<()> {
    let n = c.query_row("SELECT COUNT(*) FROM workspaces WHERE id = ?1", [id], |r| {
        r.get::<_, i64>(0)
    })?;
    if n == 0 {
        bail!("Workspace not found");
    }
    if let Some(name) = name {
        c.execute(
            "UPDATE workspaces SET name = ?2 WHERE id = ?1",
            params![id, clean_name(name)?],
        )?;
    }
    if let Some(a) = archived {
        c.execute(
            "UPDATE workspaces SET archived = ?2 WHERE id = ?1",
            params![id, a as i64],
        )?;
    }
    if let Some(col) = color {
        c.execute(
            "UPDATE workspaces SET color = ?2 WHERE id = ?1",
            params![id, Some(col).filter(|s| !s.is_empty())],
        )?;
    }
    Ok(())
}

pub fn delete_workspace(c: &Connection, id: &str) -> Result<()> {
    let n = c.execute("DELETE FROM workspaces WHERE id = ?1", [id])?;
    if n == 0 {
        bail!("Workspace not found");
    }
    c.execute("DELETE FROM item_tags WHERE item_kind = 'folder' AND item_id NOT IN (SELECT id FROM folders)", [])?;
    c.execute("DELETE FROM attachments WHERE owner_kind = 'folder' AND owner_id NOT IN (SELECT id FROM folders)", [])?;
    Ok(())
}

fn folder_ws(c: &Connection, id: &str) -> Result<String> {
    c.query_row(
        "SELECT workspace_id FROM folders WHERE id = ?1",
        [id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| anyhow!("Folder not found"))
}

fn check_dir(path: &str) -> Result<String> {
    let p = path.trim();
    if !Path::new(p).is_dir() {
        bail!("{p} is not a directory");
    }
    Ok(p.to_string())
}

pub fn create_folder(
    c: &Connection,
    workspace_id: &str,
    parent_id: Option<&str>,
    name: &str,
    path: Option<&str>,
) -> Result<String> {
    let name = clean_name(name)?;
    let n = c.query_row(
        "SELECT COUNT(*) FROM workspaces WHERE id = ?1",
        [workspace_id],
        |r| r.get::<_, i64>(0),
    )?;
    if n == 0 {
        bail!("Workspace not found");
    }
    let parent_id = parent_id.filter(|p| !p.is_empty());
    if let Some(p) = parent_id {
        if folder_ws(c, p)? != workspace_id {
            bail!("The parent folder is in another workspace");
        }
    }
    let path = match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => Some(check_dir(p)?),
        None => None,
    };
    let id = store::new_id();
    c.execute(
        "INSERT INTO folders (id, workspace_id, parent_id, name, path, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, workspace_id, parent_id, name, path, store::now()],
    )?;
    Ok(id)
}

/// Ids of `id` and every folder below it.
fn subtree(c: &Connection, id: &str) -> Result<BTreeSet<String>> {
    let mut stmt = c.prepare(
        "WITH RECURSIVE t(id) AS (SELECT ?1 UNION SELECT f.id FROM folders f JOIN t ON f.parent_id = t.id) SELECT id FROM t",
    )?;
    let rows = stmt
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    Ok(rows)
}

pub fn update_folder(c: &Connection, id: &str, patch: &FolderPatch) -> Result<()> {
    let current_ws = folder_ws(c, id)?;
    if let Some(n) = &patch.name {
        c.execute(
            "UPDATE folders SET name = ?2 WHERE id = ?1",
            params![id, clean_name(n)?],
        )?;
    }
    if let Some(p) = &patch.path {
        let v = if p.trim().is_empty() {
            None
        } else {
            Some(check_dir(p)?)
        };
        c.execute("UPDATE folders SET path = ?2 WHERE id = ?1", params![id, v])?;
    }
    if let Some(v) = patch.pinned {
        c.execute(
            "UPDATE folders SET pinned = ?2 WHERE id = ?1",
            params![id, v as i64],
        )?;
    }
    if let Some(v) = patch.archived {
        c.execute(
            "UPDATE folders SET archived = ?2 WHERE id = ?1",
            params![id, v as i64],
        )?;
    }
    if let Some(v) = patch.sort {
        c.execute("UPDATE folders SET sort = ?2 WHERE id = ?1", params![id, v])?;
    }
    if patch.parent_id.is_some() || patch.workspace_id.is_some() {
        let new_parent = patch.parent_id.as_deref().filter(|p| !p.is_empty());
        let target_ws = match (new_parent, patch.workspace_id.as_deref()) {
            (Some(p), _) => folder_ws(c, p)?,
            (None, Some(ws)) => ws.to_string(),
            (None, None) => current_ws.clone(),
        };
        let exists = c.query_row(
            "SELECT COUNT(*) FROM workspaces WHERE id = ?1",
            [&target_ws],
            |r| r.get::<_, i64>(0),
        )?;
        if exists == 0 {
            bail!("Workspace not found");
        }
        let tree = subtree(c, id)?;
        if let Some(p) = new_parent {
            if tree.contains(p) {
                bail!("A folder cannot be moved inside itself");
            }
        }
        c.execute(
            "UPDATE folders SET parent_id = ?2 WHERE id = ?1",
            params![id, new_parent],
        )?;
        for f in &tree {
            c.execute(
                "UPDATE folders SET workspace_id = ?2 WHERE id = ?1",
                params![f, target_ws],
            )?;
        }
    }
    Ok(())
}

pub fn delete_folder(c: &Connection, id: &str) -> Result<()> {
    let tree = subtree(c, id)?;
    // Chats inside go back to automatic placement rather than sticking to the workspace root.
    for f in &tree {
        c.execute(
            "UPDATE chat_meta SET workspace_id = NULL, folder_id = NULL WHERE folder_id = ?1",
            [f],
        )?;
    }
    let n = c.execute("DELETE FROM folders WHERE id = ?1", [id])?;
    if n == 0 {
        bail!("Folder not found");
    }
    for f in tree {
        c.execute(
            "DELETE FROM item_tags WHERE item_kind = 'folder' AND item_id = ?1",
            [&f],
        )?;
        c.execute(
            "DELETE FROM attachments WHERE owner_kind = 'folder' AND owner_id = ?1",
            [&f],
        )?;
    }
    Ok(())
}

fn ensure_meta(c: &Connection, session_id: &str) -> Result<()> {
    if session_id.trim().is_empty() || session_id.len() > 200 {
        bail!("Invalid session id");
    }
    c.execute(
        "INSERT OR IGNORE INTO chat_meta (session_id, updated_at) VALUES (?1, ?2)",
        params![session_id, store::now()],
    )?;
    Ok(())
}

/// Put a chat in a folder (`folder_id`), at a workspace root (`workspace_id` only), or
/// back to automatic placement (neither).
pub fn place_chat(
    c: &Connection,
    session_id: &str,
    folder_id: Option<&str>,
    workspace_id: Option<&str>,
) -> Result<()> {
    ensure_meta(c, session_id)?;
    let folder_id = folder_id.filter(|f| !f.is_empty());
    let ws = match folder_id {
        Some(f) => Some(folder_ws(c, f)?),
        None => workspace_id.filter(|w| !w.is_empty()).map(str::to_string),
    };
    c.execute(
        "UPDATE chat_meta SET folder_id = ?2, workspace_id = ?3, updated_at = ?4 WHERE session_id = ?1",
        params![session_id, folder_id, ws, store::now()],
    )?;
    Ok(())
}

pub fn set_chat_flags(
    c: &Connection,
    session_id: &str,
    pinned: Option<bool>,
    archived: Option<bool>,
) -> Result<()> {
    ensure_meta(c, session_id)?;
    if let Some(p) = pinned {
        c.execute(
            "UPDATE chat_meta SET pinned = ?2 WHERE session_id = ?1",
            params![session_id, p as i64],
        )?;
    }
    if let Some(a) = archived {
        c.execute(
            "UPDATE chat_meta SET archived = ?2 WHERE session_id = ?1",
            params![session_id, a as i64],
        )?;
    }
    Ok(())
}

fn check_kind(kind: &str) -> Result<()> {
    if matches!(kind, "chat" | "folder") {
        Ok(())
    } else {
        bail!("Kind must be chat or folder")
    }
}

pub fn set_tags(c: &Connection, kind: &str, item_id: &str, tags: &[String]) -> Result<Vec<String>> {
    check_kind(kind)?;
    let mut clean: Vec<String> = tags
        .iter()
        .map(|t| t.trim().trim_start_matches('#').to_lowercase())
        .filter(|t| !t.is_empty() && t.len() <= 40)
        .collect();
    clean.sort();
    clean.dedup();
    c.execute(
        "DELETE FROM item_tags WHERE item_kind = ?1 AND item_id = ?2",
        params![kind, item_id],
    )?;
    for t in &clean {
        c.execute("INSERT OR IGNORE INTO tags (name) VALUES (?1)", [t])?;
        c.execute(
            "INSERT OR IGNORE INTO item_tags (item_kind, item_id, tag) VALUES (?1, ?2, ?3)",
            params![kind, item_id, t],
        )?;
    }
    c.execute(
        "DELETE FROM tags WHERE name NOT IN (SELECT tag FROM item_tags)",
        [],
    )?;
    Ok(clean)
}

pub fn add_attachment(
    c: &Connection,
    kind: &str,
    owner_id: &str,
    path: &str,
    label: Option<&str>,
) -> Result<String> {
    check_kind(kind)?;
    let p = path.trim();
    if !Path::new(p).exists() {
        bail!("{p} does not exist");
    }
    let dup: Option<String> = c
        .query_row(
            "SELECT id FROM attachments WHERE owner_kind = ?1 AND owner_id = ?2 AND path = ?3",
            params![kind, owner_id, p],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = dup {
        return Ok(id);
    }
    let id = store::new_id();
    c.execute(
        "INSERT INTO attachments (id, owner_kind, owner_id, path, label, added_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, kind, owner_id, p, label.filter(|l| !l.trim().is_empty()), store::now()],
    )?;
    Ok(id)
}

pub fn remove_attachment(c: &Connection, id: &str) -> Result<()> {
    if c.execute("DELETE FROM attachments WHERE id = ?1", [id])? == 0 {
        bail!("Attachment not found");
    }
    Ok(())
}

/// Files attached to the chat and to every folder from the chat's folder up to the root.
pub fn context_files(
    c: &Connection,
    session_id: Option<&str>,
    folder_id: Option<&str>,
) -> Result<Vec<String>> {
    let mut owners: Vec<(String, String)> = Vec::new();
    if let Some(s) = session_id.filter(|s| !s.is_empty()) {
        owners.push(("chat".into(), s.to_string()));
    }
    let mut next = folder_id.filter(|f| !f.is_empty()).map(str::to_string);
    let mut guard = 0;
    while let Some(f) = next.take() {
        guard += 1;
        if guard > 64 {
            break;
        }
        owners.push(("folder".into(), f.clone()));
        next = c
            .query_row("SELECT parent_id FROM folders WHERE id = ?1", [&f], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten();
    }
    let mut out = Vec::new();
    for (kind, id) in owners {
        let mut stmt = c.prepare("SELECT path FROM attachments WHERE owner_kind = ?1 AND owner_id = ?2 ORDER BY added_at")?;
        for p in stmt.query_map(params![kind, id], |r| r.get::<_, String>(0))? {
            let p = p?;
            if Path::new(&p).exists() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// The block prepended to a prompt so the agent reads the attached files itself.
pub fn context_preamble(files: &[String]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let list: Vec<String> = files.iter().map(|f| format!("- {f}")).collect();
    format!(
        "Attached context files (read the ones relevant to this message with your file tools):\n{}\n\n",
        list.join("\n")
    )
}

pub fn slugify(name: &str) -> String {
    let mut s = String::new();
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() {
        "project".into()
    } else {
        s.chars().take(48).collect()
    }
}

/// Link a folder to a Hermes project: reuse a project that already contains the folder's
/// directory, otherwise create one with `hermes project create`.
pub fn link_project(folder_id: &str, approved: bool) -> Result<String> {
    let c = store::open()?;
    let (name, path): (String, Option<String>) = c
        .query_row(
            "SELECT name, path FROM folders WHERE id = ?1",
            [folder_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("Folder not found"))?;
    let path = path.ok_or_else(|| anyhow!("Map this folder to a directory first"))?;
    let existing = hermes_projects().into_iter().find(|p| {
        !p.archived
            && p.folders
                .iter()
                .chain(p.primary_path.iter())
                .any(|f| norm_path(f) == norm_path(&path))
    });
    let slug = match existing {
        Some(p) => p.slug,
        None => {
            let mut slug = slugify(&name);
            let taken: BTreeSet<String> = hermes_projects().into_iter().map(|p| p.slug).collect();
            let base = slug.clone();
            let mut i = 2;
            while taken.contains(&slug) {
                slug = format!("{base}-{i}");
                i += 1;
            }
            let summary = format!("hermes project create \"{name}\" --slug {slug} \"{path}\"");
            permissions::run_guarded("org.link_project", folder_id, &summary, approved, || {
                let (code, out, err) = procs::run_with_timeout(
                    "hermes",
                    // `--` ends option parsing, so a name or path starting with '-' stays positional.
                    &["project", "create", "--slug", &slug, "--", &name, &path],
                    None,
                    None,
                    std::time::Duration::from_secs(60),
                )?;
                if code != Some(0) {
                    bail!(
                        "hermes project create failed: {}",
                        format!("{out}\n{err}").trim()
                    );
                }
                Ok(())
            })?;
            slug
        }
    };
    c.execute(
        "UPDATE folders SET project_slug = ?2 WHERE id = ?1",
        params![folder_id, slug],
    )
    .context("saving the project link")?;
    Ok(slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, cwd: &str) -> SessionSummary {
        SessionSummary {
            id: id.into(),
            title: id.into(),
            cwd: cwd.into(),
            ..Default::default()
        }
    }

    #[test]
    fn nests_moves_and_prevents_cycles() {
        let c = store::test_conn();
        let ws = create_workspace(&c, "Work").unwrap();
        let ws2 = create_workspace(&c, "Personal").unwrap();
        let a = create_folder(&c, &ws.id, None, "A", None).unwrap();
        let b = create_folder(&c, &ws.id, Some(&a), "B", None).unwrap();
        let cc = create_folder(&c, &ws.id, Some(&b), "C", None).unwrap();
        let cycle = FolderPatch {
            parent_id: Some(cc.clone()),
            ..Default::default()
        };
        assert!(update_folder(&c, &a, &cycle).is_err());
        assert!(update_folder(
            &c,
            &a,
            &FolderPatch {
                parent_id: Some(a.clone()),
                ..Default::default()
            }
        )
        .is_err());
        // Move B (with C) to the other workspace root.
        update_folder(
            &c,
            &b,
            &FolderPatch {
                workspace_id: Some(ws2.id.clone()),
                parent_id: Some(String::new()),
                ..Default::default()
            },
        )
        .unwrap();
        let fs = folders(&c).unwrap();
        let get = |id: &str| fs.iter().find(|f| f.id == id).unwrap().clone();
        assert_eq!(get(&b).workspace_id, ws2.id);
        assert_eq!(get(&cc).workspace_id, ws2.id);
        assert_eq!(get(&b).parent_id, None);
        assert!(
            create_folder(&c, &ws.id, Some(&b), "X", None).is_err(),
            "parent in another workspace"
        );
        assert!(create_folder(&c, &ws.id, None, "  ", None).is_err());
    }

    #[test]
    fn places_chats_by_longest_prefix_and_explicit_moves() {
        let c = store::test_conn();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let sub = repo.join("crates").join("core");
        std::fs::create_dir_all(&sub).unwrap();
        let ws = create_workspace(&c, "Work").unwrap();
        let f_repo = create_folder(&c, &ws.id, None, "Repo", Some(repo.to_str().unwrap())).unwrap();
        let f_core = create_folder(
            &c,
            &ws.id,
            Some(&f_repo),
            "Core",
            Some(sub.to_str().unwrap()),
        )
        .unwrap();
        let f_other = create_folder(&c, &ws.id, None, "Notes", None).unwrap();
        let sessions = vec![
            session("s1", repo.to_str().unwrap()),
            session("s2", &format!("{}/x", sub.display())),
            session("s3", &format!("{}-other", repo.display())),
            session("s4", repo.to_str().unwrap()),
        ];
        place_chat(&c, "s4", Some(&f_other), None).unwrap();
        set_chat_flags(&c, "s1", Some(true), None).unwrap();
        set_tags(
            &c,
            "chat",
            "s1",
            &["#Bug".into(), "bug".into(), " ui ".into()],
        )
        .unwrap();
        let fs = folders(&c).unwrap();
        let placed = place(&c, sessions, &fs).unwrap();
        let p = |id: &str| placed.iter().find(|x| x.session.id == id).unwrap();
        assert_eq!(p("s1").folder_id.as_deref(), Some(f_repo.as_str()));
        assert!(p("s1").auto && p("s1").studio_pinned);
        assert_eq!(p("s1").tags, vec!["bug", "ui"]);
        assert_eq!(p("s2").folder_id.as_deref(), Some(f_core.as_str()));
        assert_eq!(
            p("s3").folder_id,
            None,
            "sibling with a shared name prefix is not inside"
        );
        assert_eq!(p("s4").folder_id.as_deref(), Some(f_other.as_str()));
        assert!(!p("s4").auto);
        // Deleting a folder keeps chats: they fall back to automatic placement.
        delete_folder(&c, &f_other).unwrap();
        let placed = place(
            &c,
            vec![session("s4", repo.to_str().unwrap())],
            &folders(&c).unwrap(),
        )
        .unwrap();
        assert_eq!(placed[0].folder_id.as_deref(), Some(f_repo.as_str()));
        assert!(placed[0].auto);
    }

    #[test]
    fn attachments_follow_the_folder_chain() {
        let c = store::test_conn();
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("spec.md");
        let f2 = dir.path().join("notes.md");
        std::fs::write(&f1, "a").unwrap();
        std::fs::write(&f2, "b").unwrap();
        let ws = create_workspace(&c, "W").unwrap();
        let top = create_folder(&c, &ws.id, None, "Top", None).unwrap();
        let child = create_folder(&c, &ws.id, Some(&top), "Child", None).unwrap();
        add_attachment(&c, "folder", &top, f1.to_str().unwrap(), None).unwrap();
        add_attachment(&c, "chat", "s1", f2.to_str().unwrap(), Some("notes")).unwrap();
        let again = add_attachment(&c, "chat", "s1", f2.to_str().unwrap(), None).unwrap();
        assert_eq!(
            attachments(&c).unwrap().len(),
            2,
            "duplicates are ignored: {again}"
        );
        assert!(add_attachment(&c, "chat", "s1", "Z:/does/not/exist", None).is_err());
        let files = context_files(&c, Some("s1"), Some(&child)).unwrap();
        assert_eq!(
            files,
            vec![f2.display().to_string(), f1.display().to_string()]
        );
        assert!(context_preamble(&files).contains("spec.md"));
        assert_eq!(context_preamble(&[]), "");
        delete_folder(&c, &top).unwrap();
        assert_eq!(attachments(&c).unwrap().len(), 1);
    }

    #[test]
    fn helpers() {
        assert_eq!(slugify("AgentOS Studio!"), "agentos-studio");
        assert_eq!(slugify("***"), "project");
        assert!(path_contains("C:/a/b", "C:/a/b/c"));
        assert!(path_contains("C:/a/b/", "C:/a/b"));
        assert!(!path_contains("C:/a/b", "C:/a/bc"));
        assert!(!path_contains("", "C:/a"));
        let c = store::test_conn();
        let ws = create_workspace(&c, "W").unwrap();
        update_workspace(&c, &ws.id, Some("Renamed"), Some(true), None).unwrap();
        let w = &workspaces(&c).unwrap()[0];
        assert!(w.archived && w.name == "Renamed");
        assert!(set_tags(&c, "nope", "x", &[]).is_err());
        delete_workspace(&c, &ws.id).unwrap();
        assert!(workspaces(&c).unwrap().is_empty());
    }
}
