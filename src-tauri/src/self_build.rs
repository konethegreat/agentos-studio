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
}
