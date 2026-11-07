//! Where a new chat starts by default.
//!
//! A GUI process can be launched with any working directory, and on Windows that is sometimes
//! `C:\Windows\System32` (shortcuts, scheduled tasks, service hosts). Starting an agent there is
//! both useless and dangerous, so the folder is resolved from saved state instead of inherited
//! from the process.
//!
//! Order: last folder used, then the active project's root, then the user's home directory.
//! Anything under `%WINDIR%`, or named `system32`, is rejected at every step.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::Serialize;

use crate::projects;
use crate::settings;
use crate::store;

pub const KV_KEY: &str = "chat.cwd";

/// `%WINDIR%`, or a sensible stand-in when it is missing.
pub fn windir() -> PathBuf {
    std::env::var("WINDIR")
        .or_else(|_| std::env::var("SystemRoot"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Windows"))
}

fn norm(p: &Path) -> String {
    p.display()
        .to_string()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// A directory an agent must never be pointed at.
pub fn is_forbidden(path: &Path, windir: &Path) -> bool {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return true;
    }
    let p = norm(path);
    if p.is_empty() {
        return true;
    }
    let w = norm(windir);
    let under_windows = !w.is_empty() && (p == w || p.starts_with(&format!("{w}/")));
    // Case and separator independent, so `C:\WINDOWS\system32` and `c:/windows/System32/x` both hit.
    let system = p == format!("{w}/system32")
        || p.starts_with(&format!("{w}/system32/"))
        || p.split('/').any(|seg| seg == "system32");
    under_windows || system
}

/// Pick the first acceptable candidate. Pure, so the rule is testable without the environment.
pub fn pick(
    last: Option<&str>,
    project: Option<&str>,
    home: &Path,
    windir: &Path,
) -> (PathBuf, &'static str) {
    for (value, source) in [(last, "last"), (project, "project")] {
        let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        let path = PathBuf::from(raw);
        if !is_forbidden(&path, windir) && path.is_dir() {
            return (path, source);
        }
    }
    (home.to_path_buf(), "home")
}

/// The active project's primary folder, if Hermes records one.
fn active_project_root() -> Option<String> {
    let db = projects::projects_db();
    if !db.exists() {
        return None;
    }
    let c = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    for key in ["active_project", "active", "project"] {
        if let Ok(Some(slug)) = c
            .query_row(
                "SELECT value FROM project_meta WHERE key = ?1",
                [key],
                |r| r.get::<_, String>(0),
            )
            .map(Some)
            .or_else(|_| Ok::<_, rusqlite::Error>(None))
        {
            if let Ok(list) = projects::list() {
                if let Some(p) = list.iter().find(|p| p.slug == slug || p.id == slug) {
                    if let Some(path) = p
                        .primary_path
                        .clone()
                        .or_else(|| p.folders.first().map(|f| f.path.clone()))
                    {
                        return Some(path);
                    }
                }
            }
        }
    }
    // No explicit pointer: a single project is unambiguous.
    let list = projects::list().ok()?;
    let live: Vec<_> = list.iter().filter(|p| !p.archived).collect();
    if live.len() == 1 {
        return live[0]
            .primary_path
            .clone()
            .or_else(|| live[0].folders.first().map(|f| f.path.clone()));
    }
    None
}

fn saved() -> Option<String> {
    let c = store::open().ok()?;
    store::kv_get(&c, KV_KEY).ok().flatten()
}

#[derive(Serialize, Clone, Debug)]
pub struct WorkingDir {
    pub path: String,
    pub source: String,
    pub forbidden_process_cwd: bool,
}

/// Resolve the default folder, and report whether the inherited process cwd was rejected.
pub fn resolve() -> WorkingDir {
    let windir = windir();
    let cwd = std::env::current_dir().unwrap_or_default();
    let last = saved();
    let project = active_project_root();
    let (path, source) = pick(
        last.as_deref(),
        project.as_deref(),
        &settings::home(),
        &windir,
    );
    WorkingDir {
        path: path.display().to_string(),
        source: source.to_string(),
        forbidden_process_cwd: is_forbidden(&cwd, &windir),
    }
}

/// Persist the folder a chat is using, refusing anything unsafe or missing.
pub fn remember(path: &str) -> Result<String> {
    let p = PathBuf::from(path.trim());
    if path.trim().is_empty() {
        bail!("Pick a folder first");
    }
    if is_forbidden(&p, &windir()) {
        bail!(
            "{} is a system folder; choose a project or home folder",
            p.display()
        );
    }
    if !p.is_dir() {
        bail!("Folder not found: {}", p.display());
    }
    let c = store::open()?;
    store::kv_set(&c, KV_KEY, &p.display().to_string())?;
    Ok(p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_windows_and_system32_in_any_shape() {
        let w = Path::new("C:\\Windows");
        assert!(is_forbidden(Path::new("C:\\Windows"), w));
        assert!(is_forbidden(Path::new("C:\\Windows\\System32"), w));
        assert!(is_forbidden(Path::new("c:/windows/system32/drivers"), w));
        assert!(is_forbidden(Path::new("C:/Windows/Temp"), w));
        assert!(
            is_forbidden(Path::new("relative/path"), w),
            "relative paths are not a folder choice"
        );
        assert!(is_forbidden(Path::new(""), w));
        assert!(!is_forbidden(Path::new("C:/Users/Konet/agentos-studio"), w));
        assert!(!is_forbidden(Path::new("D:/Work/app"), w));
    }

    #[test]
    fn picks_last_then_project_then_home() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let proj = d.path().join("project");
        let last = d.path().join("last");
        for p in [&home, &proj, &last] {
            std::fs::create_dir_all(p).unwrap();
        }
        let w = Path::new("C:\\Windows");

        let (p, s) = pick(
            Some(&last.display().to_string()),
            Some(&proj.display().to_string()),
            &home,
            w,
        );
        assert_eq!((p, s), (last.clone(), "last"));

        // A forbidden saved folder is skipped, not trusted.
        let (p, s) = pick(
            Some("C:\\Windows\\System32"),
            Some(&proj.display().to_string()),
            &home,
            w,
        );
        assert_eq!((p, s), (proj.clone(), "project"));

        // A folder that no longer exists is skipped too.
        let (p, s) = pick(Some("C:/does/not/exist"), Some("C:/Windows"), &home, w);
        assert_eq!((p, s), (home.clone(), "home"));

        let (p, s) = pick(None, None, &home, w);
        assert_eq!((p, s), (home, "home"));
    }

    #[test]
    fn remember_refuses_system_folders() {
        assert!(remember("C:\\Windows\\System32").is_err());
        assert!(remember("").is_err());
        assert!(remember("C:/definitely/not/a/folder").is_err());
    }
}
