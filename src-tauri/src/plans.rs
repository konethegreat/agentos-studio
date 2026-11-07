//! Plan mode: a turn sent as Hermes' own `/plan` writes a Markdown plan under
//! `<folder>/.hermes/plans/` without changing anything else. Studio shows that plan and runs it
//! only after the user approves. Studio reads plan files and never writes them.

use anyhow::{bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_PLAN_BYTES: u64 = 256 * 1024;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Plan {
    pub path: String,
    pub body: String,
    pub modified: i64,
}

pub fn plans_dir(cwd: &Path) -> PathBuf {
    cwd.join(".hermes").join("plans")
}

/// The prompt for a planning turn: Hermes' `/plan`, kept inside the chat's folder (without the
/// scope line the planner was seen exploring other repositories). Pure.
pub fn plan_prompt(task: &str, cwd: &Path) -> String {
    format!(
        "/plan Work only inside {}; do not read or plan for other folders. {}",
        cwd.display(),
        task.trim()
    )
}

/// The turn that carries out an approved plan. Pure.
pub fn run_prompt(plan_path: &str) -> String {
    format!(
        "Implement the approved plan in `{plan_path}`, step by step, inside this folder only. \
         If a step turns out to be wrong, stop and say why instead of improvising. \
         Finish with a short summary of what changed."
    )
}

/// The newest plan written at or after `since` (unix seconds), if any.
pub fn latest_since(cwd: &Path, since: i64) -> Result<Option<Plan>> {
    let dir = plans_dir(cwd);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Ok(None);
    };
    let mut best: Option<(i64, PathBuf)> = None;
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        let m = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if m >= since && best.as_ref().is_none_or(|(b, _)| m > *b) {
            best = Some((m, p));
        }
    }
    match best {
        Some((modified, p)) => Ok(Some(Plan {
            body: read_checked(&p)?,
            path: p.display().to_string(),
            modified,
        })),
        None => Ok(None),
    }
}

/// Read a plan, refusing anything that is not a Markdown file inside a `.hermes/plans` folder.
pub fn read_checked(p: &Path) -> Result<String> {
    let norm = p.display().to_string().replace('\\', "/").to_lowercase();
    if !norm.contains("/.hermes/plans/") || !norm.ends_with(".md") || norm.contains("/../") {
        bail!("Not a Hermes plan file");
    }
    let meta = std::fs::metadata(p)?;
    if meta.len() > MAX_PLAN_BYTES {
        bail!("Plan file is larger than 256 KB");
    }
    Ok(std::fs::read_to_string(p)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_plan_written_by_this_turn_only() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(latest_since(d.path(), 0).unwrap(), None);
        let dir = plans_dir(d.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.md"), "# Plan A").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let p = latest_since(d.path(), 0).unwrap().unwrap();
        assert_eq!(p.body, "# Plan A");
        let future = chrono::Utc::now().timestamp() + 3600;
        assert_eq!(
            latest_since(d.path(), future).unwrap(),
            None,
            "older plans are not this turn's"
        );
    }

    #[test]
    fn only_plan_files_can_be_read() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("secret.md");
        std::fs::write(&outside, "x").unwrap();
        assert!(read_checked(&outside).is_err());
        assert!(read_checked(&d.path().join(".hermes/plans/../../secret.md")).is_err());
    }

    #[test]
    fn prompts_scope_the_planner_and_the_run() {
        let p = plan_prompt(" add a README ", Path::new("C:/repo"));
        assert!(p.starts_with("/plan Work only inside C:/repo"));
        assert!(p.ends_with("add a README"));
        assert!(run_prompt("C:/repo/.hermes/plans/x.md").contains("`C:/repo/.hermes/plans/x.md`"));
    }
}
