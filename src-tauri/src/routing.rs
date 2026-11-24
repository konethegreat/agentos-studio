//! Team routing policy: a file in the repository that says which model a role should use.
//!
//! A policy is committed next to the code, so a team agrees on model choices in review rather than
//! in someone's local settings. Studio reads it when a mission is created; an explicit provider or
//! model on the mission still wins, because a policy is a default and not a cage.
//!
//! File shape, `.agentos/routing.json` in the repository root:
//!
//! ```json
//! {
//!   "roles": {
//!     "implementer": { "provider": "claude-subscription", "model": "claude-opus-5-5" },
//!     "reviewer":    { "provider": "antigravity-oauth",   "model": "gemini-4-argon-medium" }
//!   },
//!   "allow_merge": false
//! }
//! ```

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RoleChoice {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Policy {
    #[serde(default)]
    pub roles: std::collections::BTreeMap<String, RoleChoice>,
    /// Whether this repository may merge a mission's pull request at all.
    #[serde(default)]
    pub allow_merge: bool,
    /// Extra Claude Code tool rules for headless mission workers in this repository, for example
    /// `Bash(npm test*)` or `Bash(cargo *)`. Passed as `--allowedTools`; a bare shell rule with no
    /// pattern is refused at parse time, because it would allow any command.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// What every headless Claude Code mission worker may run: git, pull requests and the hub.
///
/// The read-only `gh` queries are here because work that must state facts (a portfolio, a
/// changelog, a plan from open issues) needs them, and a headless worker that is denied them
/// gives up without a pull request.
pub const BASE_TOOLS: [&str; 14] = [
    "mcp__agentos-hub",
    "Bash(git *)",
    "Bash(rg *)",
    "Bash(gh pr *)",
    "Bash(gh repo view*)",
    "Bash(gh repo list*)",
    "Bash(gh issue list*)",
    "Bash(gh issue view*)",
    "Bash(gh release list*)",
    "Bash(gh release view*)",
    "Bash(gh run list*)",
    "Bash(gh run view*)",
    "Bash(gh search *)",
    "Bash(gh api repos/*/readme*)",
];

/// The `--allowedTools` list for a Claude Code worker: the base rules plus the repository's own,
/// each `Bash(...)` rule mirrored as `PowerShell(...)` because Claude Code on Windows runs shell
/// commands through either tool. Deduplicated, order kept. Pure.
pub fn claude_allowed_tools(extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    for rule in BASE_TOOLS
        .iter()
        .map(|s| s.to_string())
        .chain(extra.iter().cloned())
    {
        let rule = rule.trim().to_string();
        if rule.is_empty() {
            continue;
        }
        if let Some(inner) = rule.strip_prefix("Bash(") {
            push(rule.clone());
            push(format!("PowerShell({inner}"));
        } else {
            push(rule);
        }
    }
    out
}

/// Read-only Windows commands a headless Antigravity worker reaches for when it inspects a
/// repository. `agy` runs `run_command` through Windows PowerShell 5.1, and headless mode cannot
/// ask for permission: one auto-denied command ends the whole run, so a cmdlet with no rule costs
/// the entire attempt. Writing cmdlets stay out on purpose.
pub const AGY_WINDOWS_RULES: [&str; 6] = [
    "command(regex:Get-ChildItem.*)",
    "command(regex:Get-Content .*)",
    "command(regex:Select-String .*)",
    "command(regex:Test-Path .*)",
    "command(regex:Get-Location)",
    "command(regex:Resolve-Path .*)",
];

/// The same allowlist in Antigravity's rule syntax, for `agy --settings`. `mcp__<server>` becomes
/// `mcp(<server>/*)` and `Bash(<pattern>)` becomes `command(regex:<pattern>)` with `*` as `.*`.
/// A plain `command(git)` prefix rule does not match on Windows (verified with agy 1.5.3), the
/// regex form does. `PowerShell(...)` mirrors and other Claude tool names are dropped, and the
/// read-only Windows rules in [`AGY_WINDOWS_RULES`] are appended. Pure.
pub fn agy_allow_rules(claude_rules: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in claude_rules {
        let r = r.trim();
        let rule = if let Some(server) = r.strip_prefix("mcp__") {
            format!("mcp({server}/*)")
        } else if let Some(inner) = r.strip_prefix("Bash(").and_then(|x| x.strip_suffix(')')) {
            let pattern: String = inner
                .split('*')
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(".*");
            format!("command(regex:{pattern})")
        } else {
            continue;
        };
        if !out.contains(&rule) {
            out.push(rule);
        }
    }
    for rule in AGY_WINDOWS_RULES {
        let rule = rule.to_string();
        if !out.contains(&rule) {
            out.push(rule);
        }
    }
    out
}

/// A rule that would hand a headless worker an unrestricted shell or skip permissions. Pure.
fn unsafe_rule(rule: &str) -> bool {
    let r = rule.trim();
    let bare = matches!(r, "Bash" | "PowerShell" | "Bash(*)" | "PowerShell(*)" | "*");
    bare || r.to_lowercase().contains("dangerously")
}

pub fn path_in(repo_dir: &Path) -> std::path::PathBuf {
    repo_dir.join(".agentos").join("routing.json")
}

pub fn parse(text: &str) -> Result<Policy> {
    let p: Policy = serde_json::from_str(text)?;
    for (role, c) in &p.roles {
        if !crate::missions::ROLES.contains(&role.as_str()) {
            anyhow::bail!("unknown role '{role}' in routing policy");
        }
        if c.provider.trim().is_empty() {
            anyhow::bail!("role '{role}' has no provider");
        }
    }
    if let Some(bad) = p.allowed_tools.iter().find(|r| unsafe_rule(r)) {
        anyhow::bail!("allowed_tools rule '{bad}' would allow any command; name a pattern");
    }
    Ok(p)
}

/// The repository's extra tool rules for headless workers (empty without a policy).
pub fn allowed_tools(repo_dir: &Path) -> Vec<String> {
    load(repo_dir).map(|p| p.allowed_tools).unwrap_or_default()
}

/// Build and test ecosystems a repository uses, from the build files at its root or one level
/// down. A headless worker needs the matching runners to verify its own work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildFiles {
    pub node: bool,
    pub pnpm: bool,
    pub yarn: bool,
    pub bun: bool,
    pub cargo: bool,
    pub go: bool,
    pub python: bool,
    pub dotnet: bool,
}

/// Build and test runners a worker may use to verify work. Anything else is refused, so this is
/// never a general shell.
const RUNNER_TOOLS: [&str; 30] = [
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "bun",
    "node",
    "deno",
    "python",
    "python3",
    "py",
    "uv",
    "pytest",
    "ruff",
    "cargo",
    "rustc",
    "go",
    "dotnet",
    "mvn",
    "gradle",
    "make",
    "cmake",
    "ninja",
    "tsc",
    "eslint",
    "prettier",
    "vitest",
    "jest",
    "playwright",
    "flutter",
    "dart",
];

fn is_build_skip(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".worktrees" | "dist" | "build" | "vendor"
    )
}

fn note_build_file(b: &mut BuildFiles, name: &str) {
    let n = name.to_lowercase();
    match n.as_str() {
        "package.json" => b.node = true,
        "pnpm-lock.yaml" => b.pnpm = true,
        "yarn.lock" => b.yarn = true,
        "bun.lockb" | "bun.lock" => b.bun = true,
        "cargo.toml" => b.cargo = true,
        "go.mod" => b.go = true,
        "pyproject.toml" | "setup.py" | "requirements.txt" | "uv.lock" | "pytest.ini"
        | "tox.ini" => b.python = true,
        _ if n.ends_with(".csproj") || n.ends_with(".sln") => b.dotnet = true,
        _ => {}
    }
}

fn note_dir(b: &mut BuildFiles, dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        note_build_file(b, &e.file_name().to_string_lossy());
    }
}

/// Build files at the repository root and in its immediate subfolders (a workspace often keeps
/// its app one level down). Skips `.git`, build output and dependency folders.
pub fn detect_build_files(dir: &Path) -> BuildFiles {
    let mut b = BuildFiles::default();
    note_dir(&mut b, dir);
    let Ok(rd) = std::fs::read_dir(dir) else {
        return b;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if is_build_skip(&name) {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            note_dir(&mut b, &p);
        }
    }
    b
}

fn runner_of(cmd: &str) -> Option<&'static str> {
    let c = cmd.rsplit("&& ").next().unwrap_or(cmd).trim();
    let first = c.split_whitespace().next()?.to_lowercase();
    RUNNER_TOOLS.iter().find(|t| **t == first).copied()
}

/// The rules a headless worker needs to run a goal's own verification commands and the build
/// families its repository uses, in the same `Bash(...)` syntax as the repository policy. A
/// worker told to keep checks green must be able to run them: one denied command ends an
/// Antigravity run before it can commit or open the pull request.
pub fn verification_tools(goal_commands: &[String], files: BuildFiles) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |tool: &str| {
        let rule = format!("Bash({tool} *)");
        if !out.contains(&rule) {
            out.push(rule);
        }
    };
    for c in goal_commands {
        if let Some(t) = runner_of(c) {
            push(t);
        }
    }
    if files.node {
        push("npm");
        push("npx");
        push("node");
    }
    if files.pnpm {
        push("pnpm");
    }
    if files.yarn {
        push("yarn");
    }
    if files.bun {
        push("bun");
    }
    if files.cargo {
        push("cargo");
        push("rustc");
    }
    if files.go {
        push("go");
    }
    if files.python {
        push("python");
        push("python3");
        push("uv");
        push("pytest");
    }
    if files.dotnet {
        push("dotnet");
    }
    out
}

///
/// The policy is what is merged on origin's default branch (an opt-in is a merged pull request),
/// so a stale or diverged owner checkout neither hides it nor grants tools that were never
/// merged. A folder without an origin copy (a new repository, a test) falls back to the file.
pub fn load(repo_dir: &Path) -> Option<Policy> {
    let text = crate::autonomy::on_origin(repo_dir, ".agentos/routing.json")
        .or_else(|| std::fs::read_to_string(path_in(repo_dir)).ok())?;
    parse(&text).ok()
}

/// Provider and model a mission should inherit for this role.
pub fn defaults_for(repo_dir: &Path, role: &str) -> Option<RoleChoice> {
    load(repo_dir)?.roles.get(role).cloned()
}

/// Whether the policy allows merging in this repository.
pub fn merge_allowed(repo_dir: &Path) -> bool {
    load(repo_dir).map(|p| p.allow_merge).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_parses_and_rejects_nonsense() {
        let p = parse(
            r#"{"roles":{"implementer":{"provider":"claude-subscription","model":"m"},"reviewer":{"provider":"antigravity-oauth"}},"allow_merge":true}"#,
        )
        .unwrap();
        assert_eq!(p.roles["implementer"].model, "m");
        assert!(p.allow_merge);

        assert!(
            parse(r#"{"roles":{"wizard":{"provider":"x"}}}"#).is_err(),
            "unknown role"
        );
        assert!(
            parse(r#"{"roles":{"reviewer":{"provider":"  "}}}"#).is_err(),
            "empty provider"
        );
        assert!(parse("not json").is_err());
    }

    #[test]
    fn worker_tools_are_the_base_plus_the_repo_mirrored_for_powershell() {
        let t = claude_allowed_tools(&["Bash(npm test*)".into(), "Bash(git *)".into()]);
        assert_eq!(t[0], "mcp__agentos-hub");
        assert!(t.contains(&"Bash(git *)".to_string()));
        assert!(t.contains(&"PowerShell(git *)".to_string()));
        assert!(t.contains(&"PowerShell(gh pr *)".to_string()));
        assert!(t.contains(&"PowerShell(npm test*)".to_string()));
        assert_eq!(
            t.iter().filter(|r| *r == "Bash(git *)").count(),
            1,
            "deduplicated"
        );

        let p = parse(r#"{"allowed_tools":["Bash(cargo *)"]}"#).unwrap();
        assert_eq!(p.allowed_tools, vec!["Bash(cargo *)"]);
        for bad in [
            "Bash",
            "PowerShell(*)",
            "*",
            "--dangerously-skip-permissions",
        ] {
            let json = format!(r#"{{"allowed_tools":["{bad}"]}}"#);
            assert!(parse(&json).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn antigravity_gets_the_same_allowlist_in_its_own_syntax() {
        let claude = claude_allowed_tools(&["Bash(npm test*)".into(), "Bash(node *.js)".into()]);
        let agy = agy_allow_rules(&claude);
        assert_eq!(
            agy,
            vec![
                "mcp(agentos-hub/*)",
                "command(regex:git .*)",
                "command(regex:rg .*)",
                "command(regex:gh pr .*)",
                "command(regex:gh repo view.*)",
                "command(regex:gh repo list.*)",
                "command(regex:gh issue list.*)",
                "command(regex:gh issue view.*)",
                "command(regex:gh release list.*)",
                "command(regex:gh release view.*)",
                "command(regex:gh run list.*)",
                "command(regex:gh run view.*)",
                "command(regex:gh search .*)",
                "command(regex:gh api repos/.*/readme.*)",
                "command(regex:npm test.*)",
                "command(regex:node .*\\.js)",
                "command(regex:Get-ChildItem.*)",
                "command(regex:Get-Content .*)",
                "command(regex:Select-String .*)",
                "command(regex:Test-Path .*)",
                "command(regex:Get-Location)",
                "command(regex:Resolve-Path .*)",
            ]
        );
        assert!(!agy
            .iter()
            .any(|r| r.contains("PowerShell") || r == "command(*)"));
        assert!(
            !agy.iter().any(|r| r.contains("Remove-Item")
                || r.contains("Set-Content")
                || r.contains("Out-File")),
            "the Windows rules stay read-only"
        );
        // The empty allowlist still carries the read-only Windows rules.
        let windows: Vec<String> = AGY_WINDOWS_RULES.iter().map(|r| r.to_string()).collect();
        assert_eq!(agy_allow_rules(&[]), windows);
    }

    #[test]
    fn missing_file_means_no_policy_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(load(d.path()).is_none());
        assert!(defaults_for(d.path(), "implementer").is_none());
        assert!(!merge_allowed(d.path()));
        // Origin wins over the checkout: a stale local file does not hide a merged opt-in.
        let repo = tempfile::tempdir().unwrap();
        let g = |args: &[&str]| crate::fleet::git(repo.path(), args);
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@example.com"]);
        g(&["config", "user.name", "t"]);
        std::fs::create_dir_all(repo.path().join(".agentos")).unwrap();
        std::fs::write(path_in(repo.path()), r#"{"allow_merge":true}"#).unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "opt in"]);
        g(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(path_in(repo.path()), r#"{"allow_merge":false}"#).unwrap();
        assert!(
            merge_allowed(repo.path()),
            "the merged policy on origin decides"
        );
    }

    #[test]
    fn defaults_come_from_the_repo_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".agentos")).unwrap();
        std::fs::write(
            path_in(d.path()),
            r#"{"roles":{"implementer":{"provider":"claude-subscription","model":"opus"}}}"#,
        )
        .unwrap();
        let c = defaults_for(d.path(), "implementer").unwrap();
        assert_eq!(c.provider, "claude-subscription");
        assert_eq!(c.model, "opus");
        assert!(defaults_for(d.path(), "reviewer").is_none());
    }

    #[test]
    fn verification_rules_name_the_goal_runners_and_the_build_families() {
        let files = BuildFiles {
            node: true,
            python: true,
            ..BuildFiles::default()
        };
        let rules = verification_tools(
            &[
                "npm test".into(),
                "cd src-tauri && cargo test -j 2".into(),
                "python -m pytest".into(),
            ],
            files,
        );
        for want in [
            "Bash(npm *)",
            "Bash(cargo *)",
            "Bash(python *)",
            "Bash(npx *)",
            "Bash(pytest *)",
        ] {
            assert!(
                rules.contains(&want.to_string()),
                "missing {want}: {rules:?}"
            );
        }
        assert!(rules.contains(&"Bash(npm *)".to_string()));
        assert_eq!(
            rules.iter().filter(|r| *r == "Bash(npm *)").count(),
            1,
            "deduplicated"
        );
        assert!(!rules.iter().any(|r| r == "Bash(*)"));
        // Anything that is not a known runner is never granted a rule.
        let none = verification_tools(
            &[
                "rm -rf /".into(),
                "curl http://x".into(),
                "pwsh -c 1".into(),
            ],
            BuildFiles::default(),
        );
        assert!(none.is_empty(), "{none:?}");
    }

    #[test]
    fn build_files_are_found_at_the_root_and_one_level_down() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Cargo.toml"), "").unwrap();
        std::fs::create_dir_all(d.path().join("app")).unwrap();
        std::fs::write(d.path().join("app/package.json"), "{}").unwrap();
        let b = detect_build_files(d.path());
        assert!(b.cargo && b.node && !b.python && !b.go);
        std::fs::create_dir_all(d.path().join("node_modules/x")).unwrap();
        std::fs::write(d.path().join("node_modules/x/pyproject.toml"), "").unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        std::fs::write(d.path().join(".git/go.mod"), "").unwrap();
        let again = detect_build_files(d.path());
        assert!(!again.python && !again.go, "skips builds folders and .git");
        assert_eq!(
            detect_build_files(&d.path().join("missing")),
            BuildFiles::default()
        );
    }
}
