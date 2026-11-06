//! Hermes projects, and the sessions that belong to them.
//!
//! Hermes owns projects (`projects.db`: a named workspace with one or more folders) and owns
//! sessions (`state.db`). Studio adds exactly one thing of its own: a link table recording that
//! a session was spun off from another one, so a project can show a small tree of related chats.
//! Nothing here invents a second session store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::procs;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ProjectFolder {
    pub path: String,
    pub label: Option<String>,
    pub primary: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub primary_path: Option<String>,
    pub archived: bool,
    pub folders: Vec<ProjectFolder>,
    pub session_count: usize,
    pub last_activity: f64,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProjectSession {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub started_at: f64,
    pub last_activity: f64,
    pub message_count: i64,
    pub parent_session_id: Option<String>,
    pub branch_title: Option<String>,
}

pub fn projects_db() -> PathBuf {
    procs::hermes_home().join("projects.db")
}

fn open_ro(path: &Path, what: &str) -> Result<Option<Connection>> {
    if !path.exists() {
        return Ok(None);
    }
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {what} at {}", path.display()))?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(Some(c))
}

/// Normalise a path for prefix comparison: forward slashes, lowercase, no trailing slash.
pub fn norm(path: &str) -> String {
    let p = path.trim().replace('\\', "/").to_lowercase();
    p.trim_end_matches('/').to_string()
}

/// A folder owns itself and everything under it.
pub fn under(cwd: &str, folder: &str) -> bool {
    let (c, f) = (norm(cwd), norm(folder));
    if c.is_empty() || f.is_empty() {
        return false;
    }
    c == f || c.starts_with(&format!("{f}/"))
}

/// The project that owns this working directory, longest folder wins. Mirrors Hermes's own rule.
pub fn owner<'a>(cwd: &str, projects: &'a [Project]) -> Option<&'a Project> {
    let mut best: Option<(&Project, usize)> = None;
    for p in projects.iter().filter(|p| !p.archived) {
        for f in &p.folders {
            if under(cwd, &f.path) {
                let len = norm(&f.path).len();
                if best.map(|(_, l)| len > l).unwrap_or(true) {
                    best = Some((p, len));
                }
            }
        }
    }
    best.map(|(p, _)| p)
}

fn read_projects(c: &Connection) -> Result<Vec<Project>> {
    let mut stmt = c.prepare(
        "SELECT id, slug, name, description, icon, color, primary_path, archived FROM projects ORDER BY created_at",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Project {
                id: r.get(0)?,
                slug: r.get(1)?,
                name: r.get(2)?,
                description: r.get(3)?,
                icon: r.get(4)?,
                color: r.get(5)?,
                primary_path: r.get(6)?,
                archived: r.get::<_, i64>(7).unwrap_or(0) != 0,
                folders: Vec::new(),
                session_count: 0,
                last_activity: 0.0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn read_folders(c: &Connection) -> Result<HashMap<String, Vec<ProjectFolder>>> {
    let mut stmt = c.prepare("SELECT project_id, path, label, is_primary FROM project_folders")?;
    let mut out: HashMap<String, Vec<ProjectFolder>> = HashMap::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            ProjectFolder {
                path: r.get(1)?,
                label: r.get(2)?,
                primary: r.get::<_, i64>(3).unwrap_or(0) != 0,
            },
        ))
    })?;
    for row in rows {
        let (id, f) = row?;
        out.entry(id).or_default().push(f);
    }
    Ok(out)
}

struct SessionRow {
    id: String,
    title: String,
    cwd: String,
    started_at: f64,
    last_activity: f64,
    message_count: i64,
    parent: Option<String>,
}

fn read_sessions() -> Result<Vec<SessionRow>> {
    let Some(c) = open_ro(&crate::hermes_engine::state_db(), "state.db")? else {
        return Ok(Vec::new());
    };
    let mut stmt = c.prepare(
        "SELECT id, COALESCE(title, ''), COALESCE(cwd, ''), started_at,
                COALESCE(last_activity_at, ended_at, started_at), COALESCE(message_count, 0),
                parent_session_id
         FROM sessions WHERE COALESCE(message_count, 0) > 0 ORDER BY started_at DESC LIMIT 4000",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(SessionRow {
                id: r.get(0)?,
                title: r.get(1)?,
                cwd: r.get(2)?,
                started_at: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                last_activity: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                message_count: r.get(5)?,
                parent: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Parent session, project slug and branch title for one child session.
type BranchLink = (Option<String>, Option<String>, Option<String>);

/// Studio-side branch links: child session id -> its link.
fn read_links() -> HashMap<String, BranchLink> {
    let mut out = HashMap::new();
    let Ok(c) = crate::store::open() else {
        return out;
    };
    let Ok(mut stmt) = c.prepare(
        "SELECT child_session_id, parent_session_id, project_slug, title FROM session_links",
    ) else {
        return out;
    };
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
    });
    if let Ok(rows) = rows {
        for row in rows.flatten() {
            out.insert(row.0, row.1);
        }
    }
    out
}

/// Every project with its folders, session count and last activity.
pub fn list() -> Result<Vec<Project>> {
    let Some(c) = open_ro(&projects_db(), "projects.db")? else {
        return Ok(Vec::new());
    };
    let mut projects = read_projects(&c)?;
    let folders = read_folders(&c)?;
    for p in projects.iter_mut() {
        p.folders = folders.get(&p.id).cloned().unwrap_or_default();
    }
    let sessions = read_sessions()?;
    for p in projects.iter_mut() {
        let mine: Vec<&SessionRow> = sessions
            .iter()
            .filter(|s| p.folders.iter().any(|f| under(&s.cwd, &f.path)))
            .collect();
        p.session_count = mine.len();
        p.last_activity = mine.iter().map(|s| s.last_activity).fold(0.0_f64, f64::max);
    }
    Ok(projects)
}

/// Sessions inside a project, newest first, with branch parents resolved.
pub fn sessions(slug: &str) -> Result<Vec<ProjectSession>> {
    let projects = list()?;
    let project = projects
        .iter()
        .find(|p| p.slug.eq_ignore_ascii_case(slug) || p.id == slug)
        .ok_or_else(|| anyhow::anyhow!("No project '{slug}'"))?;
    let links = read_links();
    let rows = read_sessions()?;
    let mut out: Vec<ProjectSession> = rows
        .into_iter()
        .filter(|s| {
            project.folders.iter().any(|f| under(&s.cwd, &f.path))
                || links
                    .get(&s.id)
                    .map(|(_, p, _)| p.as_deref() == Some(project.slug.as_str()))
                    .unwrap_or(false)
        })
        .map(|s| {
            let link = links.get(&s.id);
            ProjectSession {
                id: s.id.clone(),
                title: if s.title.trim().is_empty() {
                    "Untitled session".to_string()
                } else {
                    s.title
                },
                cwd: s.cwd,
                started_at: s.started_at,
                last_activity: s.last_activity,
                message_count: s.message_count,
                parent_session_id: s.parent.or_else(|| link.and_then(|l| l.0.clone())),
                branch_title: link.and_then(|l| l.2.clone()),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.last_activity
            .partial_cmp(&a.last_activity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

/// Record that `child` was spun off from `parent`, inside a project. Idempotent.
pub fn link_branch(
    parent: &str,
    project_slug: Option<&str>,
    title: &str,
    child: &str,
) -> Result<()> {
    link_branch_at(&crate::store::open()?, parent, project_slug, title, child)
}

fn link_branch_at(
    c: &Connection,
    parent: &str,
    project_slug: Option<&str>,
    title: &str,
    child: &str,
) -> Result<()> {
    if parent == child {
        bail!("A session cannot branch from itself");
    }
    c.execute(
        "INSERT INTO session_links (child_session_id, parent_session_id, project_slug, title, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(child_session_id) DO UPDATE SET parent_session_id = excluded.parent_session_id,
             project_slug = excluded.project_slug, title = excluded.title",
        rusqlite::params![child, parent, project_slug, title, crate::store::now()],
    )?;
    Ok(())
}

pub fn children_of(parent: &str) -> Vec<String> {
    read_links()
        .into_iter()
        .filter(|(_, (p, _, _))| p.as_deref() == Some(parent))
        .map(|(child, _)| child)
        .collect()
}

fn cli(args: &[&str]) -> Result<String> {
    let (code, out, err) = procs::run_with_timeout(
        "hermes",
        args,
        Some("y\n"),
        None,
        std::time::Duration::from_secs(60),
    )?;
    let text = format!("{out}\n{err}")
        .lines()
        .map(procs::clean_line)
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if code != Some(0) {
        bail!(
            "hermes {} failed: {text}",
            args.first().copied().unwrap_or("")
        );
    }
    Ok(text.lines().last().unwrap_or("done").trim().to_string())
}

pub fn create(name: &str, folders: &[String]) -> Result<String> {
    if name.trim().is_empty() {
        bail!("Give the project a name");
    }
    let mut args: Vec<String> = vec!["project".into(), "create".into(), name.trim().into()];
    args.extend(folders.iter().filter(|f| !f.trim().is_empty()).cloned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    cli(&refs)
}

pub fn rename(slug: &str, name: &str) -> Result<String> {
    cli(&["project", "rename", slug, name])
}

pub fn set_archived(slug: &str, archived: bool) -> Result<String> {
    cli(&[
        "project",
        if archived { "archive" } else { "restore" },
        slug,
    ])
}

pub fn add_folder(slug: &str, path: &str) -> Result<String> {
    cli(&["project", "add-folder", slug, path])
}

/// Skills attached to a chat, used to preload them on the next turn.
pub fn chat_skills(session_id: &str) -> Vec<String> {
    match crate::store::open() {
        Ok(c) => chat_skills_at(&c, session_id),
        Err(_) => Vec::new(),
    }
}

fn chat_skills_at(c: &Connection, session_id: &str) -> Vec<String> {
    c.prepare("SELECT skill FROM chat_skills WHERE session_id = ?1 ORDER BY skill")
        .and_then(|mut s| {
            let rows = s.query_map([session_id], |r| r.get::<_, String>(0))?;
            Ok(rows.flatten().collect::<Vec<_>>())
        })
        .unwrap_or_default()
}

pub fn set_chat_skills(session_id: &str, skills: &[String]) -> Result<()> {
    let mut c = crate::store::open()?;
    set_chat_skills_at(&mut c, session_id, skills)
}

fn set_chat_skills_at(c: &mut Connection, session_id: &str, skills: &[String]) -> Result<()> {
    let tx = c.transaction()?;
    tx.execute(
        "DELETE FROM chat_skills WHERE session_id = ?1",
        [session_id],
    )?;
    for s in skills {
        tx.execute(
            "INSERT OR IGNORE INTO chat_skills (session_id, skill) VALUES (?1, ?2)",
            rusqlite::params![session_id, s],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(slug: &str, folders: &[&str]) -> Project {
        Project {
            id: format!("p_{slug}"),
            slug: slug.into(),
            name: slug.into(),
            description: None,
            icon: None,
            color: None,
            primary_path: folders.first().map(|f| f.to_string()),
            archived: false,
            folders: folders
                .iter()
                .map(|f| ProjectFolder {
                    path: f.to_string(),
                    label: None,
                    primary: false,
                })
                .collect(),
            session_count: 0,
            last_activity: 0.0,
        }
    }

    #[test]
    fn path_prefixes_ignore_case_and_slashes() {
        assert!(under("C:\\Work\\app\\src", "c:/work/app"));
        assert!(under("C:/Work/app", "C:/Work/app"));
        assert!(
            !under("C:/Work/apple", "C:/Work/app"),
            "not a prefix, a sibling"
        );
        assert!(!under("", "C:/Work/app"));
        assert!(!under("C:/Work/app", ""));
    }

    #[test]
    fn owner_picks_the_longest_matching_folder() {
        let projects = vec![
            p("outer", &["C:/Work"]),
            p("inner", &["C:/Work/app", "C:/Other"]),
        ];
        assert_eq!(
            owner("C:/Work/app/src/main.rs", &projects).unwrap().slug,
            "inner"
        );
        assert_eq!(
            owner("C:/Work/reports/x.md", &projects).unwrap().slug,
            "outer"
        );
        assert!(owner("D:/elsewhere/x", &projects).is_none());
    }

    #[test]
    fn archived_projects_do_not_claim_folders() {
        let mut archived = p("old", &["C:/Old"]);
        archived.archived = true;
        let projects = vec![archived];
        assert!(owner("C:/Old/project", &projects).is_none());
    }

    #[test]
    fn branch_links_and_chat_skills_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let mut c = crate::store::open_at(&d.path().join("studio.db")).unwrap();
        link_branch_at(&c, "parent-1", Some("demo"), "Separate thread", "child-1").unwrap();
        link_branch_at(&c, "parent-1", Some("demo"), "Separate thread", "child-1").unwrap();
        let children: Vec<String> = c
            .prepare(
                "SELECT child_session_id FROM session_links WHERE parent_session_id = 'parent-1'",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(children, vec!["child-1".to_string()]);
        assert!(link_branch_at(&c, "same", None, "x", "same").is_err());
        set_chat_skills_at(&mut c, "s1", &["alpha".into(), "beta".into()]).unwrap();
        assert_eq!(
            chat_skills_at(&c, "s1"),
            vec!["alpha".to_string(), "beta".to_string()]
        );
        set_chat_skills_at(&mut c, "s1", &["beta".into()]).unwrap();
        assert_eq!(chat_skills_at(&c, "s1"), vec!["beta".to_string()]);
    }
}
