//! Staged startup with safe self-repair.
//!
//! Every launch runs a fixed sequence of checks, each with a time budget, and reports what it
//! found. Only repairs that are idempotent and never destructive run automatically: creating
//! missing directories, migrating `studio.db`, adding missing vault folders, and repairing this
//! process's PATH. Anything that writes another tool's configuration, installs software or
//! authenticates is reported with a pointer to the screen that can do it, never done silently.
//!
//! The report is streamed to the UI as it runs, cached to disk so the next launch can paint
//! immediately, and appended to `logs/boot.log`.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::procs;

pub const EV_BOOT: &str = "agentos://boot";
const SNAPSHOT: &str = "boot-snapshot.json";

/// Binary checks: the tools the app drives, and the flag that reports a version.
const TOOLS: &[(&str, &[&str])] = &[
    ("git", &["--version"]),
    ("node", &["--version"]),
    ("gh", &["--version"]),
    ("hermes", &["--version"]),
    ("claude", &["--version"]),
];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Repaired,
    Warn,
    Failed,
    Skipped,
}

impl Status {
    pub fn is_problem(&self) -> bool {
        matches!(self, Status::Warn | Status::Failed)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BootStep {
    pub id: String,
    pub label: String,
    pub status: Status,
    pub detail: String,
    pub ms: u128,
    /// Where to fix it when the status is a warning: a Studio view id.
    pub fix_view: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Summary {
    pub ok: usize,
    pub repaired: usize,
    pub warn: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BootReport {
    pub app_version: String,
    pub started_at: String,
    pub elapsed_ms: u128,
    pub ready: bool,
    pub steps: Vec<BootStep>,
    pub summary: Summary,
    pub logs_dir: String,
    pub vault: String,
    /// Set when a previous run crashed, so the UI can say so instead of pretending all is well.
    pub previous_crash: Option<String>,
}

impl BootReport {
    fn tally(&mut self) {
        let mut s = Summary::default();
        for step in &self.steps {
            match step.status {
                Status::Ok => s.ok += 1,
                Status::Repaired => s.repaired += 1,
                Status::Warn => s.warn += 1,
                Status::Failed => s.failed += 1,
                Status::Skipped => s.skipped += 1,
            }
        }
        self.summary = s;
    }

    pub fn problems(&self) -> Vec<&BootStep> {
        self.steps
            .iter()
            .filter(|s| s.status.is_problem())
            .collect()
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(crate::settings::home)
        .join("agentos-studio")
}

pub fn logs_dir() -> PathBuf {
    config_dir().join("logs")
}

pub fn crash_log() -> PathBuf {
    logs_dir().join("crash.log")
}

fn snapshot_path() -> PathBuf {
    config_dir().join(SNAPSHOT)
}

/// Last run's report, for an instant first paint.
pub fn load_snapshot() -> Option<BootReport> {
    let text = fs::read_to_string(snapshot_path()).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_snapshot(r: &BootReport) {
    if let Ok(text) = serde_json::to_string_pretty(r) {
        let _ = fs::create_dir_all(config_dir());
        let _ = fs::write(snapshot_path(), text);
    }
}

/// Append one line per run so a support question can be answered from a file.
fn append_log(r: &BootReport) {
    let dir = logs_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let line = serde_json::json!({
        "at": r.started_at,
        "version": r.app_version,
        "ready": r.ready,
        "elapsed_ms": r.elapsed_ms,
        "summary": r.summary,
        "steps": r.steps.iter().map(|s| serde_json::json!({
            "id": s.id, "status": s.status, "ms": s.ms, "detail": s.detail
        })).collect::<Vec<_>>(),
    });
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("boot.log"))
    {
        let _ = writeln!(f, "{line}");
        // Keep the log bounded: 400 runs is far more than needed for support.
        if let Ok(text) = fs::read_to_string(dir.join("boot.log")) {
            let lines: Vec<&str> = text.lines().collect();
            if lines.len() > 400 {
                let keep = lines[lines.len() - 400..].join("\n");
                let _ = fs::write(dir.join("boot.log"), format!("{keep}\n"));
            }
        }
    }
}

/// Record panics to a file. A GUI app with no console otherwise dies with no evidence at all.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = logs_dir();
        let _ = fs::create_dir_all(&dir);
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(crash_log())
        {
            let at = crate::vault::now();
            let payload = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            let loc = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "unknown location".into());
            let backtrace = std::backtrace::Backtrace::force_capture();
            let _ = writeln!(f, "=== {at} ===\n{payload}\nat {loc}\n{backtrace}\n");
        }
        default(info);
    }));
}

fn step(id: &str, label: &str) -> BootStep {
    BootStep {
        id: id.into(),
        label: label.into(),
        status: Status::Ok,
        detail: String::new(),
        ms: 0,
        fix_view: None,
    }
}

struct Runner {
    app: Option<AppHandle>,
    steps: Vec<BootStep>,
}

impl Runner {
    fn push(&mut self, s: BootStep) {
        self.steps.push(s);
        if let Some(app) = &self.app {
            let _ = app.emit(
                EV_BOOT,
                serde_json::json!({
                    "steps": self.steps,
                    "done": false,
                }),
            );
        }
    }

    /// Run one check, timing it and capturing a failure as a step instead of aborting boot.
    fn run<F>(&mut self, id: &str, label: &str, fix: Option<&str>, f: F)
    where
        F: FnOnce() -> (Status, String),
    {
        let started = Instant::now();
        let (status, detail) = f();
        let mut s = step(id, label);
        s.status = status;
        s.detail = detail;
        s.ms = started.elapsed().as_millis();
        s.fix_view = fix.map(String::from);
        self.push(s);
    }
}

fn check_tool(name: &str, args: &[&str]) -> (Status, String) {
    let Some(exe) = procs::find(name) else {
        return (Status::Warn, format!("{name} not on PATH"));
    };
    match procs::run_with_timeout(name, args, None, None, Duration::from_secs(4)) {
        Ok((Some(0), out, _)) => {
            let v = out.lines().next().unwrap_or("").trim().to_string();
            (Status::Ok, format!("{} ({})", v, exe.display()))
        }
        Ok(_) => (
            Status::Warn,
            format!("{} found but did not answer --version", exe.display()),
        ),
        Err(e) => (Status::Warn, format!("{name}: {e}")),
    }
}

const TOOL_CACHE: &str = "tool-versions.json";
const TOOL_TTL_SECS: u64 = 3600;

#[derive(Serialize, Deserialize, Clone)]
struct ToolCached {
    detail: String,
    at: u64,
}

fn tool_cache_path() -> PathBuf {
    config_dir().join(TOOL_CACHE)
}

fn load_tool_cache() -> std::collections::HashMap<String, ToolCached> {
    fs::read_to_string(tool_cache_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_tool_cache(map: &std::collections::HashMap<String, ToolCached>) {
    if let Ok(text) = serde_json::to_string_pretty(map) {
        let _ = fs::create_dir_all(config_dir());
        let _ = fs::write(tool_cache_path(), text);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Probe a tool, or reuse a fresh cache entry. Only successful probes are cached: `claude
/// --version` costs about three seconds, and re-proving it on every launch is what made startup
/// feel slow.
fn check_tool_cached(
    name: &'static str,
    args: &'static [&'static str],
    cache: &std::collections::HashMap<String, ToolCached>,
) -> (Status, String, Option<String>) {
    if let Some(entry) = cache.get(name) {
        let fresh = now_secs().saturating_sub(entry.at) < TOOL_TTL_SECS;
        // The binary must still resolve; that costs microseconds, unlike a version probe.
        if fresh && procs::find(name).is_some() {
            return (Status::Ok, format!("{} (cached)", entry.detail), None);
        }
    }
    let (status, detail) = check_tool(name, args);
    let fresh = (status == Status::Ok).then(|| detail.clone());
    (status, detail, fresh)
}

/// Run every check and return the report. Blocking; call it off the UI thread.
pub fn run(app: Option<AppHandle>) -> BootReport {
    let started = Instant::now();
    let mut r = Runner {
        app,
        steps: Vec::new(),
    };
    let settings = crate::settings::Settings::load();
    let vault = settings.vault_path();

    // 1. Directories this app owns. Creating them is safe and idempotent.
    r.run("dirs", "Application folders", None, || {
        let mut created = Vec::new();
        for d in [config_dir(), logs_dir(), config_dir().join("backups")] {
            if !d.exists() && fs::create_dir_all(&d).is_ok() {
                created.push(d.display().to_string());
            }
        }
        if created.is_empty() {
            (Status::Ok, "All present".into())
        } else {
            (Status::Repaired, format!("Created {}", created.join(", ")))
        }
    });

    // 2. PATH. GUI processes inherit a stale environment, which is the most common reason a
    //    tool "is installed" but cannot be found.
    r.run("path", "Tool search path", None, || {
        let added = procs::heal_process_path();
        if added == 0 {
            (Status::Ok, "PATH already complete".into())
        } else {
            (
                Status::Repaired,
                format!("Added {added} entries to this process PATH"),
            )
        }
    });

    // 3. Settings.
    r.run("settings", "Settings", Some("setup"), || {
        if settings
            .vault
            .as_deref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
        {
            (Status::Ok, "Saved vault path".to_string())
        } else if vault.exists() {
            (
                Status::Ok,
                format!(
                    "Using {} (not saved yet; pick it in Setup to pin it)",
                    vault.display()
                ),
            )
        } else {
            (
                Status::Warn,
                format!(
                    "Vault not found at {}. Choose a folder in Setup.",
                    vault.display()
                ),
            )
        }
    });

    // 4. Studio database.
    r.run(
        "store",
        "Studio database",
        None,
        || match crate::store::open() {
            Ok(c) => {
                let ws: i64 = c
                    .query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))
                    .unwrap_or(0);
                (Status::Ok, format!("studio.db ready, {ws} workspace(s)"))
            }
            Err(e) => (Status::Failed, format!("studio.db: {e:#}")),
        },
    );

    // 5. Vault. Missing folders are created; existing files are never touched.
    r.run("vault", "Obsidian vault", Some("setup"), || {
        let v = crate::vault::Vault::new(&vault);
        if !vault.exists() {
            return (Status::Warn, format!("{} does not exist", vault.display()));
        }
        let (ok, missing) = v.structure_ok();
        if ok {
            return (Status::Ok, vault.display().to_string());
        }
        match v.scaffold() {
            Ok(made) if !made.is_empty() => {
                (Status::Repaired, format!("Created {}", made.join(", ")))
            }
            Ok(_) => (
                Status::Warn,
                format!("Still missing: {}", missing.join(", ")),
            ),
            Err(e) => (Status::Failed, format!("could not scaffold: {e:#}")),
        }
    });

    // 6. Tools. Parallel so the total wait is one timeout rather than five, and cached for an
    //    hour so a warm launch does not re-probe what was just confirmed.
    let tool_cache = load_tool_cache();
    r.run("tools", "Connected tools", Some("setup"), || {
        let (tx, rx) = mpsc::channel::<(&'static str, Status, String, Option<String>)>();
        for (name, args) in TOOLS {
            let tx = tx.clone();
            let name = *name;
            let cached = tool_cache.clone();
            std::thread::spawn(move || {
                let (status, detail, fresh) = check_tool_cached(name, args, &cached);
                let _ = tx.send((name, status, detail, fresh));
            });
        }
        drop(tx);
        let mut found = Vec::new();
        let mut updates: std::collections::HashMap<String, ToolCached> = tool_cache.clone();
        for _ in 0..TOOLS.len() {
            if let Ok((name, status, detail, fresh)) = rx.recv_timeout(Duration::from_secs(8)) {
                if let Some(d) = fresh {
                    updates.insert(
                        name.to_string(),
                        ToolCached {
                            detail: d,
                            at: now_secs(),
                        },
                    );
                }
                found.push((name.to_string(), status, detail));
            }
        }
        save_tool_cache(&updates);
        found.sort_by(|a, b| a.0.cmp(&b.0));
        let missing: Vec<&str> = found
            .iter()
            .filter(|(_, s, _)| s.is_problem())
            .map(|(n, _, _)| n.as_str())
            .collect();
        let detail = found
            .iter()
            .map(|(n, s, d)| {
                if s.is_problem() {
                    format!("{n}: {d}")
                } else {
                    format!("{n} ok")
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        if missing.is_empty() {
            (Status::Ok, detail)
        } else if found
            .iter()
            .any(|(n, s, _)| n == "hermes" && !s.is_problem())
        {
            (
                Status::Warn,
                format!("Hermes is ready; missing {}", missing.join(", ")),
            )
        } else {
            (Status::Warn, detail)
        }
    });

    // 7. Hermes configuration, read only. A parse failure is reported, never rewritten here.
    r.run("hermes", "Hermes configuration", Some("control"), || {
        match crate::hermes_engine::read_config() {
            Ok(cfg) => {
                let m = crate::hermes_engine::model_config(&cfg);
                let (total, enabled) = crate::hermes_engine::mcp_counts(&cfg);
                (
                    Status::Ok,
                    format!(
                        "{} on {m}, {enabled}/{total} MCP servers",
                        crate::hermes_engine::provider_label(&m),
                        m = m.model
                    ),
                )
            }
            Err(e) => (
                Status::Failed,
                format!("config.yaml unreadable: {e:#}. Open MCP and Providers to repair."),
            ),
        }
    });

    // 8. Agent registrations. Reported, not written: registering touches other tools' configs.
    r.run("mcp", "agentos-hub registration", Some("mcp"), || {
        let has = |p: PathBuf| {
            fs::read_to_string(p)
                .map(|t| t.contains("agentos-hub"))
                .unwrap_or(false)
        };
        let hermes_ok = has(procs::hermes_config());
        let claude_ok = has(procs::claude_json());
        let mut registered = Vec::new();
        if hermes_ok {
            registered.push("Hermes");
        }
        if claude_ok {
            registered.push("Claude Code");
        }
        match registered.len() {
            2 => (
                Status::Ok,
                "Registered in Hermes and Claude Code".to_string(),
            ),
            0 => (
                Status::Warn,
                "Not registered in any agent. Use Sync or Cut Over in MCP and Providers"
                    .to_string(),
            ),
            _ => (
                Status::Warn,
                format!(
                    "Registered in {}. Use Sync in MCP and Providers for the rest",
                    registered.join(" and ")
                ),
            ),
        }
    });

    let mut report = BootReport {
        app_version: env!("CARGO_PKG_VERSION").into(),
        started_at: crate::vault::now(),
        elapsed_ms: started.elapsed().as_millis(),
        ready: true,
        steps: r.steps,
        summary: Summary::default(),
        logs_dir: logs_dir().display().to_string(),
        vault: vault.display().to_string(),
        previous_crash: previous_crash(),
    };
    report.tally();
    report.ready = report.summary.failed == 0;
    save_snapshot(&report);
    append_log(&report);
    if let Some(app) = &r.app {
        let _ = app.emit(
            EV_BOOT,
            serde_json::json!({ "steps": report.steps, "done": true, "report": report }),
        );
    }
    report
}

/// Most recent crash entry, trimmed, so the UI can surface it on the next launch.
fn previous_crash() -> Option<String> {
    let text = fs::read_to_string(crash_log()).ok()?;
    let last = text
        .rsplit("=== ")
        .next()?
        .lines()
        .take(4)
        .collect::<Vec<_>>()
        .join(" ");
    Some(last.chars().take(300).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_tally_and_problems() {
        let mut r = BootReport {
            app_version: "x".into(),
            started_at: "t".into(),
            elapsed_ms: 1,
            ready: true,
            steps: vec![
                BootStep {
                    id: "a".into(),
                    label: "A".into(),
                    status: Status::Ok,
                    detail: String::new(),
                    ms: 1,
                    fix_view: None,
                },
                BootStep {
                    id: "b".into(),
                    label: "B".into(),
                    status: Status::Repaired,
                    detail: String::new(),
                    ms: 2,
                    fix_view: None,
                },
                BootStep {
                    id: "c".into(),
                    label: "C".into(),
                    status: Status::Warn,
                    detail: String::new(),
                    ms: 3,
                    fix_view: Some("setup".into()),
                },
            ],
            summary: Summary::default(),
            logs_dir: String::new(),
            vault: String::new(),
            previous_crash: None,
        };
        r.tally();
        assert_eq!(
            (r.summary.ok, r.summary.repaired, r.summary.warn),
            (1, 1, 1)
        );
        assert_eq!(r.problems().len(), 1);
        assert!(r.problems()[0].status.is_problem());
    }

    #[test]
    fn report_round_trips_through_json() {
        let r = BootReport {
            app_version: "0.4.0".into(),
            started_at: "now".into(),
            elapsed_ms: 42,
            ready: false,
            steps: vec![BootStep {
                id: "tools".into(),
                label: "Connected tools".into(),
                status: Status::Warn,
                detail: "gh not on PATH".into(),
                ms: 12,
                fix_view: Some("setup".into()),
            }],
            summary: Summary {
                warn: 1,
                ..Default::default()
            },
            logs_dir: "logs".into(),
            vault: "v".into(),
            previous_crash: Some("boom".into()),
        };
        let text = serde_json::to_string(&r).unwrap();
        let back: BootReport = serde_json::from_str(&text).unwrap();
        assert_eq!(back.steps[0].status, Status::Warn);
        assert!(back.steps[0].fix_view.is_some());
        assert_eq!(back.summary.warn, 1);
    }

    #[test]
    fn paths_live_under_the_config_dir() {
        assert!(logs_dir().ends_with("logs"));
        assert!(crash_log().ends_with("crash.log"));
        assert_eq!(crash_log().parent().unwrap(), logs_dir());
    }
}
