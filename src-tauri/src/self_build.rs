//! Ouroboros (Beyond B6): the fleet may improve AgentOS Studio itself, inside two fences.
//!
//! - **Protected paths.** A pull request on the Studio repository that touches what keeps the
//!   fleet honest (the gate, the merge, receipts, Shadow, budgets, release and updater signing,
//!   dependencies, the agent rule files) fails the constitution gate with a `protected` finding.
//!   Those files change only by the owner's hand.
//! - **One heavy build at a time.** A Studio mission builds the whole Rust app. The laptop runs
//!   one cargo job at a time (11 GB of memory, `jobs = 2`), so a Studio mission starts only when
//!   no other Studio mission is running and no cargo or rustc process is alive.

use crate::constitution_gate::{ChangedFile, Finding};
use crate::missions::Mission;
use crate::store;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

const KEY_LAST_RUN_AT: &str = "self_build.last_run_at";
const KEY_LAST_CANDIDATE: &str = "self_build.last_candidate";
const KEY_LAST_WEEKLY_SHIP_AT: &str = "self_build.last_weekly_ship_at";
const WEEKLY_COOLDOWN_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelfBuildStatus {
    pub last_run_at: Option<i64>,
    pub last_candidate: Option<String>,
    pub last_weekly_ship_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateReport {
    pub version: String,
    pub installer_path: String,
    pub health_ok: bool,
    pub pr_url: Option<String>,
}

pub fn status() -> Result<SelfBuildStatus> {
    let conn = store::open()?;
    let last_run_at = store::kv_get(&conn, KEY_LAST_RUN_AT)?.and_then(|s| s.parse::<i64>().ok());
    let last_candidate = store::kv_get(&conn, KEY_LAST_CANDIDATE)?;
    let last_weekly_ship_at =
        store::kv_get(&conn, KEY_LAST_WEEKLY_SHIP_AT)?.and_then(|s| s.parse::<i64>().ok());
    Ok(SelfBuildStatus {
        last_run_at,
        last_candidate,
        last_weekly_ship_at,
    })
}

/// B6.2: runs the candidate release flow with injectable build, verify, and PR steps.
pub fn run_candidate_release_with<B, V, P>(
    repo_dir: &Path,
    version: &str,
    now: i64,
    build_fn: B,
    verify_fn: V,
    pr_fn: P,
) -> Result<CandidateReport>
where
    B: FnOnce(&Path, &str) -> Result<String>,
    V: FnOnce(&Path) -> Result<bool>,
    P: FnOnce(&Path, &str) -> Result<Option<String>>,
{
    if !is_self(&repo_dir.to_string_lossy()) {
        bail!("candidate release is only for the agentos-studio repository");
    }
    let installer_path = build_fn(repo_dir, version)?;
    let health_ok = verify_fn(Path::new(&installer_path))?;
    if !health_ok {
        bail!("candidate installer failed live template or health verification");
    }
    let pr_url = pr_fn(repo_dir, version)?;
    let conn = store::open()?;
    store::kv_set(&conn, KEY_LAST_RUN_AT, &now.to_string())?;
    store::kv_set(&conn, KEY_LAST_CANDIDATE, version)?;
    Ok(CandidateReport {
        version: version.to_string(),
        installer_path,
        health_ok,
        pr_url,
    })
}

/// B6.2: default candidate release flow for `agentos-studio`.
pub fn run_candidate_release(repo_dir: &Path, version: &str) -> Result<CandidateReport> {
    if cargo_busy() {
        bail!("another cargo or rustc build is already running");
    }
    run_candidate_release_with(
        repo_dir,
        version,
        store::now(),
        |dir, ver| {
            let npm = crate::procs::find("npm").unwrap_or_else(|| std::path::PathBuf::from("npm"));
            let out = crate::procs::command(&npm)
                .current_dir(dir)
                .env("CARGO_TARGET_DIR", dir.join("src-tauri/target-0.9.1"))
                .args(["run", "tauri", "build"])
                .output()?;
            if !out.status.success() {
                bail!("tauri build failed");
            }
            Ok(dir
                .join(format!(
                    "src-tauri/target-0.9.1/release/bundle/nsis/AgentOS Studio_{ver}_x64-setup.exe"
                ))
                .to_string_lossy()
                .into_owned())
        },
        |installer| Ok(installer.exists()),
        |dir, ver| {
            let gh = crate::procs::find("gh").unwrap_or_else(|| std::path::PathBuf::from("gh"));
            let title = format!("chore(release): candidate v{ver}");
            let body = format!("Automated candidate release v{ver} verified by Ouroboros.");
            let out = crate::procs::command(&gh)
                .current_dir(dir)
                .args(["pr", "create", "--title", &title, "--body", &body])
                .output()?;
            if out.status.success() {
                Ok(Some(
                    String::from_utf8_lossy(&out.stdout).trim().to_string(),
                ))
            } else {
                Ok(None)
            }
        },
    )
}

/// B6.3: builds the weekly ship summary from receipts merged in the last 7 days.
pub fn format_weekly_ship(receipts: &[crate::receipts::Receipt], now: i64) -> Option<String> {
    let cutoff = now - WEEKLY_COOLDOWN_SECS;
    let recent: Vec<&crate::receipts::Receipt> =
        receipts.iter().filter(|r| r.at >= cutoff).collect();
    if recent.is_empty() {
        return None;
    }
    let mut repos: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for r in &recent {
        *repos.entry(r.repo.as_str()).or_default() += 1;
    }
    let breakdown = repos
        .into_iter()
        .map(|(repo, n)| format!("{repo}: {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "AgentOS Studio weekly ship: {} PR(s) merged ({breakdown}). Reply \"ship\" to tag.",
        recent.len()
    ))
}

/// B6.3: sends the weekly ship summary via `send_fn` if outside the 7-day cooldown.
pub fn maybe_send_weekly_ship_with<F>(now: i64, send_fn: F) -> Result<bool>
where
    F: FnOnce(&str) -> Result<()>,
{
    let conn = store::open()?;
    if let Some(last) =
        store::kv_get(&conn, KEY_LAST_WEEKLY_SHIP_AT)?.and_then(|s| s.parse::<i64>().ok())
    {
        if now - last < WEEKLY_COOLDOWN_SECS {
            return Ok(false);
        }
    }
    let receipts = crate::receipts::all()?;
    let Some(msg) = format_weekly_ship(&receipts, now) else {
        return Ok(false);
    };
    send_fn(&msg)?;
    store::kv_set(&conn, KEY_LAST_WEEKLY_SHIP_AT, &now.to_string())?;
    Ok(true)
}

/// B6.3: sends the weekly ship summary to the owner's phone via Hermes.
pub fn maybe_send_weekly_ship(now: i64) -> Result<bool> {
    maybe_send_weekly_ship_with(now, |msg| {
        let Some(target) = crate::hermes_surfaces::notify_target() else {
            return Err(anyhow!("no notify target configured"));
        };
        crate::hermes_surfaces::send(&target, msg).map(|_| ())
    })
}

/// The repository name of Studio itself.
pub const SELF_REPO: &str = "agentos-studio";

/// Paths no mission may change on the Studio repository: a prefix ends with `/`, anything else
/// is one exact file.
pub const PROTECTED: &[&str] = &[
    ".github/",
    ".agentos/",
    ".agents/",
    ".qoder/",
    "AGENTS.md",
    "CLAUDE.md",
    "GEMINI.md",
    "package.json",
    "package-lock.json",
    "src-tauri/Cargo.toml",
    "src-tauri/Cargo.lock",
    "src-tauri/tauri.conf.json",
    "src-tauri/capabilities/",
    "src-tauri/src/constitution_gate.rs",
    "src-tauri/src/review_agent.rs",
    "src-tauri/src/receipts.rs",
    "src-tauri/src/shadow.rs",
    "src-tauri/src/budget.rs",
    "src-tauri/src/self_build.rs",
];

/// What every Studio worker brief adds, so the first attempt stays inside the fence.
pub const WORKER_RULES: &str = "\n\nThis repository is AgentOS Studio itself. Do not change these paths; a pull request that does is sent back: .github/, .agentos/, .agents/, .qoder/, AGENTS.md, CLAUDE.md, GEMINI.md, package.json, package-lock.json, src-tauri/Cargo.toml, src-tauri/Cargo.lock, src-tauri/tauri.conf.json, src-tauri/capabilities/, and src-tauri/src/ constitution_gate.rs, review_agent.rs, receipts.rs, shadow.rs, budget.rs and self_build.rs. Build and test with at most two jobs (cargo build -j 2, cargo test -j 2).";

/// `konethegreat/agentos-studio`, `agentos-studio` or a path ending in it. Pure.
pub fn is_self(repo: &str) -> bool {
    let r = repo.trim().trim_end_matches(['/', '\\']);
    let name = r.rsplit(['/', '\\']).next().unwrap_or(r);
    name.eq_ignore_ascii_case(SELF_REPO)
}

/// Is this path one no mission may change? Pure.
pub fn is_protected(path: &str) -> bool {
    let p = path.trim().trim_start_matches("./").replace('\\', "/");
    PROTECTED.iter().any(|g| {
        if g.ends_with('/') {
            p.starts_with(g)
        } else {
            p.eq_ignore_ascii_case(g)
        }
    })
}

/// Findings for protected paths a Studio pull request touches (added, changed or deleted). Pure.
pub fn protected_findings(repo: &str, files: &[ChangedFile]) -> Vec<Finding> {
    if !is_self(repo) {
        return Vec::new();
    }
    files
        .iter()
        .filter(|f| is_protected(&f.path))
        .map(|f| Finding {
            rule: "protected".into(),
            path: f.path.clone(),
            line: None,
            detail: "this path keeps the fleet honest and changes only by the owner's hand; revert it in this pull request".into(),
        })
        .collect()
}

/// May this Studio mission start now? Pure: `cargo_busy` is read by the caller.
pub fn may_start(m: &Mission, running: &[&Mission], cargo_busy: bool) -> bool {
    !is_self(&m.repo) || (!cargo_busy && !running.iter().any(|r| is_self(&r.repo)))
}

/// Process names (lowercase, no extension) in a process listing. Pure.
pub fn heavy_build_in(listing: &str) -> bool {
    listing.lines().any(|l| {
        let first = l
            .trim()
            .trim_start_matches('"')
            .split(['"', ',', ' ', '\t'])
            .next()
            .unwrap_or("")
            .to_lowercase();
        let base = first.rsplit(['/', '\\']).next().unwrap_or("");
        let base = base.strip_suffix(".exe").unwrap_or(base);
        base == "cargo" || base == "rustc"
    })
}

/// Is any cargo or rustc process alive on this machine?
pub fn cargo_busy() -> bool {
    let out = if cfg!(windows) {
        crate::procs::command(std::path::Path::new("tasklist"))
            .args(["/FO", "CSV", "/NH"])
            .stdin(std::process::Stdio::null())
            .output()
    } else {
        crate::procs::command(std::path::Path::new("ps"))
            .args(["-A", "-o", "comm="])
            .stdin(std::process::Stdio::null())
            .output()
    };
    // Unknown is busy: a second heavy build is the failure this fence exists for.
    out.map(|o| heavy_build_in(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(p: &str) -> ChangedFile {
        ChangedFile {
            path: p.into(),
            ..Default::default()
        }
    }

    #[test]
    fn studio_is_recognised_by_any_name_form() {
        assert!(is_self("konethegreat/agentos-studio"));
        assert!(is_self("agentos-studio"));
        assert!(is_self("C:\\Users\\me\\agentos-studio\\"));
        assert!(!is_self("konethegreat/agentos-studio-website"));
        assert!(!is_self("agentos-sandbox-1"));
    }

    #[test]
    fn the_fence_holds_on_studio_only() {
        let files = vec![
            file(".github/workflows/release.yml"),
            file(".agentos/routing.json"),
            file("src-tauri/src/review_agent.rs"),
            file("src-tauri/src/goal_loop.rs"),
            file("src/views/Today.tsx"),
            file("docs/GOALS.md"),
            file("src-tauri/tauri.conf.json"),
        ];
        let f = protected_findings("konethegreat/agentos-studio", &files);
        let paths: Vec<&str> = f.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                ".github/workflows/release.yml",
                ".agentos/routing.json",
                "src-tauri/src/review_agent.rs",
                "src-tauri/tauri.conf.json"
            ]
        );
        assert!(f.iter().all(|x| x.rule == "protected"));
        assert!(protected_findings("konethegreat/agentos-sandbox-1", &files).is_empty());
        assert!(!is_protected("src-tauri/src/review_agent_helpers.rs"));
    }

    #[test]
    fn one_heavy_build_at_a_time() {
        let studio = Mission {
            id: "M-1".into(),
            repo: "agentos-studio".into(),
            ..Default::default()
        };
        let other = Mission {
            id: "M-2".into(),
            repo: "agentos-sandbox-1".into(),
            ..Default::default()
        };
        assert!(may_start(&studio, &[&other], false));
        assert!(!may_start(&studio, &[&other], true), "cargo is busy");
        assert!(
            !may_start(&studio, &[&studio], false),
            "a Studio mission already runs"
        );
        assert!(
            may_start(&other, &[&studio], true),
            "other repositories are not fenced"
        );
    }

    #[test]
    fn cargo_and_rustc_are_found_in_a_listing() {
        let win = "\"svchost.exe\",\"1\",\"Services\",\"0\",\"1 K\"\n\"rustc.exe\",\"22\",\"Console\",\"1\",\"900 K\"\n";
        assert!(heavy_build_in(win));
        assert!(heavy_build_in("/usr/bin/cargo\nbash\n"));
        assert!(!heavy_build_in("\"cargo-watch-helper.exe\",\"3\"\nnode\n"));
        assert!(!heavy_build_in(""));
    }

    #[test]
    fn candidate_release_records_version_and_requires_self_repo() {
        let d = tempfile::TempDir::new().unwrap();
        store::set_test_db(Some(d.path().join("s.db")));
        let studio = d.path().join("agentos-studio");
        let other = d.path().join("other-repo");
        assert!(run_candidate_release_with(
            &other,
            "1.7.0",
            1000,
            |_, _| Ok("setup.exe".into()),
            |_| Ok(true),
            |_, _| Ok(None),
        )
        .is_err());
        let rep = run_candidate_release_with(
            &studio,
            "1.7.0",
            1000,
            |_, _| Ok("setup.exe".into()),
            |_| Ok(true),
            |_, _| {
                Ok(Some(
                    "https://github.com/konethegreat/agentos-studio/pull/202".into(),
                ))
            },
        )
        .unwrap();
        assert_eq!(rep.version, "1.7.0");
        assert!(rep.health_ok);
        let st = status().unwrap();
        assert_eq!(st.last_run_at, Some(1000));
        assert_eq!(st.last_candidate.as_deref(), Some("1.7.0"));
        store::set_test_db(None);
    }

    #[test]
    fn weekly_ship_formats_summary() {
        let r = crate::receipts::Receipt {
            seq: 1,
            at: 1000,
            mission: "m1".into(),
            goal: None,
            repo: "konethegreat/agentos-studio".into(),
            pr: 201,
            pr_url: "https://github.com/konethegreat/agentos-studio/pull/201".into(),
            head_commit: "abc".into(),
            merge_commit: None,
            checks: vec![],
            implementer: "claude".into(),
            reviewer: "hermes".into(),
            verdict: "approve".into(),
            verify: "pass".into(),
            spent_usd: 0.0,
            prev_hash: String::new(),
            public_key: "pk".into(),
            hash: "h".into(),
            signature: "s".into(),
        };
        let msg = format_weekly_ship(&[r], 2000).unwrap();
        assert!(msg.contains("1 PR(s) merged"));
        assert!(msg.contains("konethegreat/agentos-studio: 1"));
    }
}
