//! Constitution rendering and managed-block injection.
//!
//! The output is byte-compatible with the AgentOS Python tooling (`fleet_deploy.py`):
//! the same markers, headers and rule-file frontmatter, so either tool sees the other's
//! files as already synced.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;
use serde::Serialize;

pub const BEGIN: &str =
    "<!-- AGENTOS:BEGIN (managed by AgentOS fleet_deploy.py - edit templates/CONSTITUTION.md instead) -->";
pub const END: &str = "<!-- AGENTOS:END -->";
pub const GI_BEGIN: &str = "# >>> AgentOS local agent caches (rule files stay tracked) >>>";
pub const GI_END: &str = "# <<< AgentOS <<<";
pub const GI_LINES: [&str; 5] = [
    ".claude/settings.local.json",
    ".qoder/settings.local.json",
    "CLAUDE.local.md",
    "AGENTS.local.md",
    "GEMINI.local.md",
];
const DISABLED_PREFIX: &str = "<!-- agentos:disabled-laws";

/// Bundled default used when the vault has no CONSTITUTION.md yet.
pub const DEFAULT_CONSTITUTION: &str = include_str!("../../templates/CONSTITUTION.md");

/// The three agent context files every repository receives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Claude,
    Agents,
    Gemini,
    System,
}

impl Kind {
    pub fn file_name(self) -> &'static str {
        match self {
            Kind::Claude => "CLAUDE.md",
            Kind::Agents => "AGENTS.md",
            Kind::Gemini => "GEMINI.md",
            Kind::System => "SYSTEM_CONTEXT.md",
        }
    }
    fn tool(self) -> &'static str {
        match self {
            Kind::Claude => "Claude Code",
            Kind::Agents => "Qoder and Antigravity",
            Kind::Gemini => "Antigravity / Gemini",
            Kind::System => "all agents",
        }
    }
    fn who(self) -> &'static str {
        match self {
            Kind::Claude => "You are **claude-code** in the AgentOS fleet. AgentOS-Hub tools appear as `mcp__agentos-hub__*`.",
            Kind::Agents => "Your agent name is **qoder** when running in Qoder and **antigravity** when running in Antigravity.",
            Kind::Gemini => "You are **antigravity** in the AgentOS fleet (Gemini-based IDE agent).",
            Kind::System => "Applies to hermes, claude-code, qoder, and antigravity alike.",
        }
    }
    pub const REPO: [Kind; 3] = [Kind::Claude, Kind::Agents, Kind::Gemini];
}

pub fn block_re() -> Regex {
    Regex::new(r"(?s)<!-- AGENTOS:BEGIN[^\n]*-->.*?<!-- AGENTOS:END -->").expect("static regex")
}

pub fn dash_re() -> Regex {
    Regex::new("[\u{2013}\u{2014}]").expect("static regex")
}

/// Path of CONSTITUTION.md inside a vault.
pub fn constitution_path(vault: &Path) -> PathBuf {
    vault
        .join("04_Agent_Engine")
        .join("templates")
        .join("CONSTITUTION.md")
}

/// Raw constitution text (including any disabled-laws marker).
pub fn load_raw(vault: &Path) -> String {
    fs::read_to_string(constitution_path(vault))
        .map(|s| s.replace("\r\n", "\n"))
        .unwrap_or_else(|_| DEFAULT_CONSTITUTION.replace("\r\n", "\n"))
}

#[derive(Debug, Clone, Serialize)]
pub struct Law {
    pub number: u32,
    pub title: String,
    pub enabled: bool,
}

fn law_heading_re() -> Regex {
    Regex::new(r"(?m)^### Law (\d+) - (.+)$").expect("static regex")
}

fn disabled_set(raw: &str) -> Vec<u32> {
    raw.lines()
        .find(|l| l.trim_start().starts_with(DISABLED_PREFIX))
        .map(|l| {
            l.trim_start()
                .trim_start_matches(DISABLED_PREFIX)
                .trim_end_matches("-->")
                .split(',')
                .filter_map(|n| n.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Laws found in the constitution with their enabled state.
pub fn laws(raw: &str) -> Vec<Law> {
    let disabled = disabled_set(raw);
    law_heading_re()
        .captures_iter(raw)
        .filter_map(|c| {
            let number: u32 = c[1].parse().ok()?;
            Some(Law {
                number,
                title: c[2].trim().to_string(),
                enabled: !disabled.contains(&number),
            })
        })
        .collect()
}

/// Return `raw` with law `number` enabled or disabled (recorded in a marker comment).
pub fn set_law_enabled(raw: &str, number: u32, enabled: bool) -> String {
    let mut disabled = disabled_set(raw);
    disabled.retain(|n| *n != number);
    if !enabled {
        disabled.push(number);
    }
    disabled.sort_unstable();
    let body: Vec<&str> = raw
        .lines()
        .filter(|l| !l.trim_start().starts_with(DISABLED_PREFIX))
        .collect();
    let mut out = String::new();
    if !disabled.is_empty() {
        let list: Vec<String> = disabled.iter().map(u32::to_string).collect();
        out.push_str(&format!("{DISABLED_PREFIX} {} -->\n", list.join(",")));
    }
    out.push_str(&body.join("\n"));
    out.push('\n');
    out
}

/// Constitution text as agents see it: marker removed, disabled law sections dropped.
pub fn effective(raw: &str) -> String {
    let disabled = disabled_set(raw);
    let heading = law_heading_re();
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in raw.lines() {
        if line.trim_start().starts_with(DISABLED_PREFIX) {
            continue;
        }
        if line.starts_with("### ") {
            skipping = heading
                .captures(line)
                .and_then(|c| c[1].parse::<u32>().ok())
                .map(|n| disabled.contains(&n))
                .unwrap_or(false);
        } else if line.starts_with("## ") {
            skipping = false;
        }
        if !skipping {
            out.push(line);
        }
    }
    out.join("\n").trim().to_string()
}

pub fn managed_block(raw: &str, kind: Kind) -> String {
    format!(
        "{BEGIN}\n# AgentOS rules ({})\n\n{}\n\n{}\n{END}",
        kind.tool(),
        kind.who(),
        effective(raw)
    )
}

pub fn rule_file(raw: &str) -> String {
    format!(
        "---\ntrigger: always_on\ndescription: AgentOS constitution - autonomy, anti-slop, PR loop, language choice, living docs, agentos-hub bus\n---\n\n{BEGIN}\n# AgentOS rules\n\nUse your own agent name (qoder, antigravity, claude-code, hermes) in AgentOS-Hub calls.\n\n{}\n{END}\n",
        effective(raw)
    )
}

pub fn cursorrules(raw: &str) -> String {
    format!(
        "{BEGIN}\n# AgentOS rules (Cursor-compatible IDE agents: Qoder, Antigravity)\n# Agent name: qoder in Qoder, antigravity in Antigravity.\n\n{}\n{END}\n",
        effective(raw)
    )
}

/// Line ending already used by an existing file, so rewrites never churn CRLF files.
fn newline_of(path: &Path) -> &'static str {
    match fs::read(path) {
        Ok(bytes) if bytes.windows(2).any(|w| w == b"\r\n") => "\r\n",
        _ => "\n",
    }
}

fn write_with_newline(path: &Path, text_lf: &str, nl: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let data = if nl == "\n" {
        text_lf.to_string()
    } else {
        text_lf.replace('\n', nl)
    };
    fs::write(path, data).with_context(|| format!("writing {}", path.display()))
}

/// Pure transform: insert or replace the managed block in `existing`.
pub fn apply_block(existing: Option<&str>, block: &str) -> String {
    match existing {
        None => format!("{block}\n"),
        Some(old) => {
            let old = old.replace("\r\n", "\n");
            let re = block_re();
            if re.is_match(&old) {
                re.replace(&old, regex::NoExpand(block)).into_owned()
            } else {
                format!("{}\n\n{block}\n", old.trim_end())
            }
        }
    }
}

/// Insert or replace the managed block in a file. Returns created/updated/appended/unchanged.
pub fn upsert_block(path: &Path, block: &str) -> Result<&'static str> {
    let existing = fs::read_to_string(path).ok();
    let had_block = existing
        .as_deref()
        .map(|t| block_re().is_match(t))
        .unwrap_or(false);
    let new = apply_block(existing.as_deref(), block);
    if let Some(old) = &existing {
        if old.replace("\r\n", "\n") == new {
            return Ok("unchanged");
        }
    }
    let nl = newline_of(path);
    write_with_newline(path, &new, nl)?;
    Ok(match (&existing, had_block) {
        (None, _) => "created",
        (Some(_), true) => "updated",
        (Some(_), false) => "appended",
    })
}

/// Write a file AgentOS owns entirely.
pub fn write_owned(path: &Path, content: &str) -> Result<&'static str> {
    let existing = fs::read_to_string(path).ok();
    if existing.as_deref().map(|e| e.replace("\r\n", "\n")) == Some(content.to_string()) {
        return Ok("unchanged");
    }
    write_with_newline(path, content, newline_of(path))?;
    Ok(if existing.is_some() {
        "updated"
    } else {
        "created"
    })
}

/// Pure transform for the .gitignore cache block.
pub fn apply_gitignore(existing: Option<&str>) -> String {
    let text = existing.unwrap_or("").replace("\r\n", "\n");
    if let (Some(s), Some(e)) = (text.find(GI_BEGIN), text.find(GI_END)) {
        if e > s {
            // Rebuild the block with only the lines the user does not already have outside it.
            let (before, after) = (&text[..s], &text[e + GI_END.len()..]);
            let outside: Vec<&str> = before.lines().chain(after.lines()).map(str::trim).collect();
            let needed: Vec<&str> = GI_LINES
                .iter()
                .copied()
                .filter(|l| !outside.contains(l))
                .collect();
            let block = [GI_BEGIN]
                .into_iter()
                .chain(needed)
                .chain([GI_END])
                .collect::<Vec<_>>()
                .join("\n");
            return format!("{before}{block}{after}");
        }
    }
    let present: Vec<&str> = text.lines().map(str::trim).collect();
    let missing: Vec<&str> = GI_LINES
        .iter()
        .copied()
        .filter(|l| !present.contains(l))
        .collect();
    if missing.is_empty() {
        return text;
    }
    let block = [GI_BEGIN]
        .into_iter()
        .chain(missing)
        .chain([GI_END])
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        format!("{block}\n")
    } else {
        format!("{}\n\n{block}\n", text.trim_end())
    }
}

pub fn ensure_gitignore(repo: &Path) -> Result<&'static str> {
    let path = repo.join(".gitignore");
    let existing = fs::read_to_string(&path).ok();
    let new = apply_gitignore(existing.as_deref());
    if existing.as_deref().map(|e| e.replace("\r\n", "\n")) == Some(new.clone()) {
        return Ok("unchanged");
    }
    write_with_newline(&path, &new, newline_of(&path))?;
    Ok(if existing.is_some() {
        "appended"
    } else {
        "created"
    })
}

/// Paths (relative to a repo root) that AgentOS manages.
pub const REPO_RULE_PATHS: [&str; 5] = [
    "CLAUDE.md",
    "AGENTS.md",
    "GEMINI.md",
    ".qoder/rules/agentos.md",
    ".agents/rules/agentos.md",
];

/// Desired content of one managed repo path given its committed or current base text.
pub fn desired_repo_file(raw: &str, rel: &str, base: Option<&str>) -> String {
    match rel {
        "CLAUDE.md" => apply_block(base, &managed_block(raw, Kind::Claude)),
        "AGENTS.md" => apply_block(base, &managed_block(raw, Kind::Agents)),
        "GEMINI.md" => apply_block(base, &managed_block(raw, Kind::Gemini)),
        ".gitignore" => apply_gitignore(base),
        _ => rule_file(raw),
    }
}

/// Replace em-dashes and en-dashes with ASCII equivalents (Law 2).
pub fn undash(text: &str) -> String {
    let spaced_em = Regex::new("[ \t]*\u{2014}[ \t]*").expect("static regex");
    let range = Regex::new("(\\d)\u{2013}(\\d)").expect("static regex");
    let spaced_en = Regex::new("[ \t]*\u{2013}[ \t]*").expect("static regex");
    let t = spaced_em.replace_all(text, " - ");
    let t = range.replace_all(&t, "$1-$2");
    spaced_en.replace_all(&t, " - ").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "## AgentOS Constitution v2\n\nIntro.\n\n### Law 1 - Full autonomy\n- one\n\n### Law 2 - Anti-slop\n- two\n\n### Security floor (never overridden by Law 1)\n- floor\n";

    #[test]
    fn laws_parse_and_toggle() {
        let l = laws(RAW);
        assert_eq!(l.len(), 2);
        assert!(l.iter().all(|x| x.enabled));
        let off = set_law_enabled(RAW, 2, false);
        assert!(!laws(&off)[1].enabled);
        let eff = effective(&off);
        assert!(!eff.contains("Anti-slop"));
        assert!(eff.contains("Security floor"));
        assert!(!eff.contains("disabled-laws"));
        let on = set_law_enabled(&off, 2, true);
        assert_eq!(effective(&on), effective(RAW));
    }

    #[test]
    fn block_is_idempotent_and_preserves_user_text() {
        let block = managed_block(RAW, Kind::Claude);
        let first = apply_block(Some("# My repo\n\nUser notes.\n"), &block);
        assert!(first.starts_with("# My repo"));
        let second = apply_block(Some(&first), &block);
        assert_eq!(first, second);
    }

    #[test]
    fn gitignore_block_only_adds_missing_lines() {
        let out = apply_gitignore(Some("node_modules/\nCLAUDE.local.md\n"));
        assert!(out.contains(GI_BEGIN));
        assert_eq!(out.matches("CLAUDE.local.md").count(), 1);
        assert_eq!(apply_gitignore(Some(&out)), out);
    }

    #[test]
    fn undash_handles_spaced_and_ranges() {
        let s = format!("a {} b, 1{}3, x{}y", '\u{2014}', '\u{2013}', '\u{2013}');
        assert_eq!(undash(&s), "a - b, 1-3, x - y");
        assert!(!dash_re().is_match(&undash(&s)));
    }

    #[test]
    fn crlf_files_keep_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("CLAUDE.md");
        fs::write(&p, "# Title\r\nline\r\n").unwrap();
        upsert_block(&p, &managed_block(RAW, Kind::Claude)).unwrap();
        let bytes = fs::read(&p).unwrap();
        let lf_only = bytes
            .windows(2)
            .filter(|w| w[1] == b'\n' && w[0] != b'\r')
            .count();
        assert_eq!(lf_only, 0);
    }
}
