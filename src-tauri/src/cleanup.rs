use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::audit;
use crate::config;
use crate::missions::{self, MissionStatus};
use crate::procs;
use crate::store;

const MIN_SWEEP_INTERVAL_SECS: i64 = 10 * 60;
const LOCK_MAX_AGE_SECS: i64 = 30 * 60;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupItem {
    pub kind: String,
    pub target: String,
    pub reason: String,
    pub archived_diff: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    pub dry_run: bool,
    pub items: Vec<CleanupItem>,
    pub removed: usize,
    pub at: i64,
}

static LAST_SWEEP_AT: OnceLock<Mutex<Option<i64>>> = OnceLock::new();
static LAST_REPORT: OnceLock<Mutex<Option<CleanupReport>>> = OnceLock::new();

fn last_sweep() -> &'static Mutex<Option<i64>> {
    LAST_SWEEP_AT.get_or_init(|| Mutex::new(None))
}

fn last_report_lock() -> &'static Mutex<Option<CleanupReport>> {
    LAST_REPORT.get_or_init(|| Mutex::new(None))
}

pub fn status() -> Option<CleanupReport> {
    last_report_lock().lock().unwrap().clone()
}

pub fn run_startup() {
    let _ = run_now();
}

pub fn step() {
    if crate::pause::is_paused() {
        return;
    }
    let now = store::now_unix();
    {
        let guard = last_sweep().lock().unwrap();
        if let Some(last) = *guard {
            if now - last < MIN_SWEEP_INTERVAL_SECS {
                return;
            }
        }
    }
    let _ = run_now();
}

fn default_paths() -> (PathBuf, PathBuf, PathBuf) {
    let cfg = config::read_config();
    let vault = cfg
        .vault_path
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs_vault_fallback());
    let locks = vault.join("04_Agent_Engine").join("locks");
    let archive = vault.join("04_Agent_Engine").join("archive");
    let wt_root = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into()))
        .join("agentos-studio")
        .join("worktrees");
    (locks, wt_root, archive)
}

fn dirs_vault_fallback() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join("Obsidian")
        .join("Hermes Obsidian vault")
}

pub fn dry_run() -> CleanupReport {
    let (locks, wt_root, archive) = default_paths();
    let items = dry_run_in(&locks, &wt_root, store::now_unix());
    CleanupReport {
        dry_run: true,
        items,
        removed: 0,
        at: store::now_unix(),
    }
}

pub fn run_now() -> CleanupReport {
    let (locks, wt_root, archive) = default_paths();
    let report = run_sweep_in(&locks, &wt_root, &archive, store::now_unix());
    *last_sweep().lock().unwrap() = Some(report.at);
    *last_report_lock().lock().unwrap() = Some(report.clone());
    report
}

pub fn dry_run_in(locks_dir: &Path, worktrees_root: &Path, now: i64) -> Vec<CleanupItem> {
    let mut items = Vec::new();

    // 1. Expired vault locks
    if let Ok(entries) = fs::read_dir(locks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("lock") {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if let Ok(mod_t) = meta.modified() {
                    if let Ok(dur) = mod_t.duration_since(std::time::UNIX_EPOCH) {
                        let age = now - dur.as_secs() as i64;
                        if age >= LOCK_MAX_AGE_SECS {
                            items.push(CleanupItem {
                                kind: "lock".into(),
                                target: path.to_string_lossy().into_owned(),
                                reason: format!("expired lock ({}m old)", age / 60),
                                archived_diff: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // 2. Orphan worktrees
    let active_missions: HashSet<String> = missions::list_missions(None)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| matches!(m.status, MissionStatus::Running | MissionStatus::Queued))
        .map(|m| m.worktree)
        .collect();

    if let Ok(entries) = fs::read_dir(worktrees_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let p_str = path.to_string_lossy().into_owned();
            if !active_missions.contains(&p_str) {
                items.push(CleanupItem {
                    kind: "worktree".into(),
                    target: p_str,
                    reason: "orphan worktree with no active mission".into(),
                    archived_diff: None,
                });
            }
        }
    }

    // 3. Lingering headless worker processes without an active mission
    let running_ids: HashSet<String> = missions::list_missions(Some(MissionStatus::Running))
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.id)
        .collect();
    if let Ok(list) = procs::list_procs() {
        for p in list {
            let active = p
                .mission_id
                .as_deref()
                .map(|id| running_ids.contains(id))
                .unwrap_or(false);
            if !active {
                if let Some(pid) = p.pid {
                    items.push(CleanupItem {
                        kind: "process".into(),
                        target: format!("{}:{}", p.id, pid),
                        reason: "lingering worker process without active mission".into(),
                        archived_diff: None,
                    });
                }
            }
        }
    }

    items
}

pub fn run_sweep_in(
    locks_dir: &Path,
    worktrees_root: &Path,
    archive_dir: &Path,
    now: i64,
) -> CleanupReport {
    let mut items = dry_run_in(locks_dir, worktrees_root, now);
    let mut removed = 0usize;
    let _ = fs::create_dir_all(archive_dir.join("locks"));
    let _ = fs::create_dir_all(archive_dir.join("diffs"));

    for item in &mut items {
        match item.kind.as_str() {
            "lock" => {
                let src = Path::new(&item.target);
                let fname = src
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| format!("lock-{now}.lock"));
                let dst = archive_dir.join("locks").join(format!("{now}-{fname}"));
                if fs::rename(src, &dst).is_ok() || fs::remove_file(src).is_ok() {
                    removed += 1;
                    let _ = audit::record("cleanup", "remove_lock", &item.reason, Some(&item.target));
                }
            }
            "worktree" => {
                let wt = Path::new(&item.target);
                if let Some(diff_path) = archive_uncommitted_diff(wt, archive_dir, now) {
                    item.archived_diff = Some(diff_path);
                }
                if fs::remove_dir_all(wt).is_ok() {
                    removed += 1;
                    let detail = match &item.archived_diff {
                        Some(d) => format!("{} (diff archived to {})", item.reason, d),
                        None => item.reason.clone(),
                    };
                    let _ = audit::record("cleanup", "remove_worktree", &detail, Some(&item.target));
                }
            }
            "process" => {
                if let Some((proc_id, pid_str)) = item.target.split_once(':') {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        kill_pid(pid);
                    }
                    let _ = procs::remove_proc(proc_id);
                    removed += 1;
                    let _ = audit::record("cleanup", "kill_orphan_proc", &item.reason, Some(proc_id));
                }
            }
            _ => {}
        }
    }

    CleanupReport {
        dry_run: false,
        items,
        removed,
        at: now,
    }
}

fn archive_uncommitted_diff(wt: &Path, archive_dir: &Path, now: i64) -> Option<String> {
    let status = Command::new("git")
        .args(["-C", &wt.to_string_lossy(), "status", "--porcelain"])
        .output()
        .ok()?;
    if !status.status.success() || status.stdout.is_empty() {
        return None;
    }
    let diff = Command::new("git")
        .args(["-C", &wt.to_string_lossy(), "diff", "HEAD"])
        .output()
        .ok()?;
    let name = wt.file_name()?.to_string_lossy();
    let out_path = archive_dir.join("diffs").join(format!("{now}-{name}.patch"));
    let mut content = diff.stdout;
    if content.is_empty() {
        content = status.stdout;
    }
    fs::write(&out_path, content).ok()?;
    Some(out_path.to_string_lossy().into_owned())
}

fn kill_pid(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweeps_synthetic_orphan_worktree_and_stale_lock_and_logs_audit() {
        let ts = store::now_unix();
        let root = std::env::temp_dir().join(format!("agentos_cleanup_test_{ts}"));
        let locks = root.join("locks");
        let wts = root.join("worktrees");
        let archive = root.join("archive");
        fs::create_dir_all(&locks).unwrap();
        fs::create_dir_all(&wts).unwrap();
        store::set_test_db(Some(root.join("studio.db")));

        let lock_file = locks.join("stale.lock");
        fs::write(&lock_file, "old lock").unwrap();
        let orphan_wt = wts.join("orphan-wt");
        fs::create_dir_all(&orphan_wt).unwrap();
        fs::write(orphan_wt.join("notes.txt"), "dirty").unwrap();

        let future = ts + LOCK_MAX_AGE_SECS + 60;
        let dry = dry_run_in(&locks, &wts, future);
        assert_eq!(dry.len(), 2);

        let rep = run_sweep_in(&locks, &wts, &archive, future);
        assert_eq!(rep.removed, 2);
        assert!(!lock_file.exists());
        assert!(!orphan_wt.exists());

        let _ = fs::remove_dir_all(&root);
    }
}
