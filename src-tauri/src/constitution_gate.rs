//! The constitution gate: the AgentOS laws as checks on a pull request's diff, run by Studio
//! before the reviewer and again by the gatekeeper before any merge.
//!
//! Only lines the pull request adds are judged, so a repository's history never blocks new work.
//! Five rules:
//!
//! - **dash:** no em dash or en dash (Law 2).
//! - **placeholder:** no placeholder tags, stubbed macros or filler text (Law 2).
//! - **hidden_unicode:** no invisible characters that change how text reads: Unicode tag
//!   characters, zero-width characters and bidirectional controls.
//! - **secret:** nothing that looks like a credential (cloud keys, tokens, private keys, a
//!   quoted value assigned to a key, token, secret or password name). The finding never repeats
//!   the value.
//! - **docs:** code under `src/` or `src-tauri/src/` changed with no Markdown file in the same
//!   pull request (Law 5).
//!
//! A diff too large to read in full is a finding too: an unread line is not a passed line.
//! `scan_lines` is shared with the capability quarantine, which judges whole files.

use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;

/// The name a gate rejection is recorded under, as the reviewer of that round.
pub const REVIEWER: &str = "constitution-gate";
/// The marker `github::clip` appends when it cut a long output.
const TRIMMED: &str = "(output trimmed)";
const MAX_FINDINGS_IN_NOTE: usize = 40;
/// What every worker brief says about the gate, so the first attempt can pass it.
pub const WORKER_RULES: &str = "\n\nBefore review, AgentOS Studio's constitution gate checks the lines your pull request adds: no em or en dashes, no placeholder tags or stubbed code, no hidden Unicode, no credentials, and when code under src/ or src-tauri/src/ changes, a Markdown file (README or docs) changes in the same pull request. A finding sends the work back to you.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub rule: String,
    pub path: String,
    /// 1-based line in the new file, when the finding is about one line.
    pub line: Option<u32>,
    pub detail: String,
}

/// One file of a unified diff, reduced to what the gate reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    /// Added lines with their line number in the new file.
    pub added: Vec<(u32, String)>,
    pub deleted: bool,
    pub binary: bool,
}

/// Parse `git diff` / `gh pr diff` output. Pure.
pub fn parse_diff(diff: &str) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut line_no: u32 = 0;
    for raw in diff.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            // `a/<path> b/<path>`; the b side is refined by the +++ line when present.
            let path = rest
                .rsplit_once(" b/")
                .map(|(_, b)| b.to_string())
                .unwrap_or_default();
            files.push(ChangedFile {
                path,
                ..Default::default()
            });
            continue;
        }
        let Some(f) = files.last_mut() else {
            continue;
        };
        if raw.starts_with("+++ ") {
            match raw.trim_start_matches("+++ ").trim() {
                "/dev/null" => f.deleted = true,
                p => f.path = p.strip_prefix("b/").unwrap_or(p).to_string(),
            }
            continue;
        }
        if raw.starts_with("--- ") {
            continue;
        }
        if raw.starts_with("Binary files ") || raw.starts_with("GIT binary patch") {
            f.binary = true;
            continue;
        }
        if let Some(h) = raw.strip_prefix("@@ ") {
            // @@ -a,b +c,d @@
            line_no = h
                .split_whitespace()
                .find_map(|t| t.strip_prefix('+'))
                .and_then(|t| t.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(1);
            continue;
        }
        if let Some(added) = raw.strip_prefix('+') {
            f.added.push((line_no, added.to_string()));
            line_no += 1;
        } else if raw.starts_with(' ') {
            line_no += 1;
        }
    }
    files
}

fn re(
    cell: &'static OnceLock<Vec<(&'static str, Regex)>>,
    src: &[(&'static str, &str)],
) -> &'static [(&'static str, Regex)] {
    cell.get_or_init(|| {
        src.iter()
            .map(|(name, p)| (*name, Regex::new(p).expect("gate pattern compiles")))
            .collect()
    })
}

/// Placeholder tags, written so this file does not contain the words it looks for.
fn placeholders() -> &'static [(&'static str, Regex)] {
    static C: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    re(
        &C,
        &[
            ("a to-do tag", r"\bT[O]DO\b"),
            ("a fix-me tag", r"\bFIXM[E]\b"),
            ("an XXX tag", r"\bX[X]X\b"),
            ("a TBD tag", r"\bTB[D]\b"),
            ("a stubbed todo macro", r"\btod[o]!\s*\("),
            ("a stubbed unimplemented macro", r"\bunimplemente[d]!\s*\("),
            ("filler text", r"(?i)\blore[m] ipsum\b"),
        ],
    )
}

fn secrets() -> &'static [(&'static str, Regex)] {
    static C: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    re(
        &C,
        &[
            ("an AWS access key", r"\bAKIA[0-9A-Z]{16}\b"),
            ("a GitHub token", r"\bgh[pousr]_[A-Za-z0-9]{36,}"),
            ("a GitHub token", r"\bgithub_pat_[A-Za-z0-9_]{40,}"),
            ("an Anthropic key", r"\bsk-ant-[A-Za-z0-9_\-]{20,}"),
            ("an OpenAI style key", r"\bsk-(?:proj-)?[A-Za-z0-9_\-]{32,}"),
            ("a Slack token", r"\bxox[baprs]-[A-Za-z0-9\-]{10,}"),
            ("a Google API key", r"\bAIza[0-9A-Za-z_\-]{35}"),
            ("a Notion token", r"\b(?:secret|ntn)_[A-Za-z0-9]{40,}"),
            ("a private key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
            (
                "a quoted credential",
                r#"(?i)\b(?:api[_\-]?key|secret|token|password|passwd)\b["']?\s*[:=]\s*["'][A-Za-z0-9_\-+/=.]{16,}["']"#,
            ),
        ],
    )
}

/// Characters that hide text or reorder it on screen. Pure.
pub fn hidden_char(c: char) -> Option<&'static str> {
    match c as u32 {
        0xE0000..=0xE007F => Some("a Unicode tag character"),
        0x200B..=0x200D | 0x2060 | 0xFEFF => Some("a zero-width character"),
        0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069 => Some("a bidirectional control"),
        _ => None,
    }
}

fn is_lockfile(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "package-lock.json"
            | "Cargo.lock"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "poetry.lock"
            | "uv.lock"
    )
}

/// Judge some lines of one file against the dash, placeholder, hidden-character and secret
/// rules. Pure; the capability quarantine calls it with every line of a file.
pub fn scan_lines<'a>(path: &str, lines: impl IntoIterator<Item = (u32, &'a str)>) -> Vec<Finding> {
    let mut out = Vec::new();
    let lock = is_lockfile(path);
    let mut push = |rule: &str, line: u32, detail: String| {
        out.push(Finding {
            rule: rule.into(),
            path: path.to_string(),
            line: Some(line),
            detail,
        })
    };
    for (n, text) in lines {
        if !lock {
            if text.contains('\u{2014}') || text.contains('\u{2013}') {
                push("dash", n, "an em dash or en dash".into());
            }
            for (what, r) in placeholders() {
                if r.is_match(text) {
                    push("placeholder", n, (*what).into());
                }
            }
            let mut seen: Vec<&str> = Vec::new();
            for (i, c) in text.chars().enumerate() {
                // A byte order mark that opens a file is an encoding marker, not hidden text.
                if i == 0 && n == 1 && c == '\u{FEFF}' {
                    continue;
                }
                if let Some(what) = hidden_char(c) {
                    if !seen.contains(&what) {
                        seen.push(what);
                        push("hidden_unicode", n, format!("{what} (U+{:04X})", c as u32));
                    }
                }
            }
        }
        for (what, r) in secrets() {
            if let Some(m) = r.find(text) {
                let start: String = m.as_str().chars().take(4).collect();
                push("secret", n, format!("{what} ({start}...)"));
                break;
            }
        }
    }
    out
}

fn is_code(path: &str) -> bool {
    path.starts_with("src/") || path.starts_with("src-tauri/src/")
}

/// Every rule over a parsed diff. Pure.
pub fn scan(files: &[ChangedFile]) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in files.iter().filter(|f| !f.binary && !f.deleted) {
        out.extend(scan_lines(
            &f.path,
            f.added.iter().map(|(n, t)| (*n, t.as_str())),
        ));
    }
    let code: Vec<&str> = files
        .iter()
        .filter(|f| is_code(&f.path))
        .map(|f| f.path.as_str())
        .collect();
    let docs = files.iter().any(|f| f.path.to_lowercase().ends_with(".md"));
    if !code.is_empty() && !docs {
        out.push(Finding {
            rule: "docs".into(),
            path: code[0].to_string(),
            line: None,
            detail: format!(
                "{} code file(s) under src changed and no Markdown file did; update the README or docs in the same pull request",
                code.len()
            ),
        });
    }
    out
}

/// The gate over raw diff text, including the rule that an unread diff does not pass. Pure.
pub fn check_diff(diff: &str) -> Vec<Finding> {
    let mut out = scan(&parse_diff(diff));
    if diff.trim_end().ends_with(TRIMMED) {
        out.push(Finding {
            rule: "too_large".into(),
            path: String::new(),
            line: None,
            detail: "the diff is too large to read in full, so the unread part cannot pass; split the pull request".into(),
        });
    }
    out
}

/// The note a gate rejection carries back to the implementer. Pure.
pub fn note(findings: &[Finding]) -> String {
    let mut s = format!(
        "Constitution gate: {} finding(s). Fix them on the same branch; the reviewer runs once the gate passes.\n",
        findings.len()
    );
    for f in findings.iter().take(MAX_FINDINGS_IN_NOTE) {
        let at = match (f.path.is_empty(), f.line) {
            (true, _) => String::new(),
            (false, Some(n)) => format!("{}:{n} ", f.path),
            (false, None) => format!("{} ", f.path),
        };
        s.push_str(&format!("- {at}{}: {}\n", f.rule, f.detail));
    }
    if findings.len() > MAX_FINDINGS_IN_NOTE {
        s.push_str(&format!(
            "- and {} more\n",
            findings.len() - MAX_FINDINGS_IN_NOTE
        ));
    }
    s
}

/// Read a pull request's diff through `gh` and judge it.
pub fn check_pr(repo_with_owner: &str, number: u64) -> Result<Vec<Finding>> {
    let diff = crate::github::pr_diff(repo_with_owner, number)?;
    Ok(check_diff(&diff))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff_of(path: &str, added: &[&str]) -> String {
        let mut d = format!(
            "diff --git a/{path} b/{path}\nindex 1..2 100644\n--- a/{path}\n+++ b/{path}\n@@ -1,1 +1,{} @@\n context\n",
            added.len() + 1
        );
        for a in added {
            d.push('+');
            d.push_str(a);
            d.push('\n');
        }
        d
    }

    #[test]
    fn only_added_lines_are_judged_with_their_line_numbers() {
        let d = "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -10,3 +10,4 @@\n keep\n-old \u{2014} line\n+new line\n keep\n+second \u{2014} added\n";
        let files = parse_diff(d);
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].added,
            vec![
                (11, "new line".to_string()),
                (13, "second \u{2014} added".to_string())
            ]
        );
        let f = check_diff(d);
        assert_eq!(f.len(), 1, "the removed dash is not judged: {f:?}");
        assert_eq!((f[0].rule.as_str(), f[0].line), ("dash", Some(13)));
    }

    #[test]
    fn dashes_placeholders_and_hidden_characters_are_found() {
        let tag = format!("// {}{}: wire this", "TO", "DO");
        let stub = format!("fn f() {{ {}!() }}", "todo");
        let hidden = format!("let a = 1;{}", '\u{E0041}');
        let bidi = format!("x{}y", '\u{202E}');
        let d = diff_of(
            "docs/X.md",
            &[
                "fine \u{2013} not",
                &tag,
                &stub,
                &hidden,
                &bidi,
                "placeholder=\"Filter notes\"",
            ],
        );
        let f = check_diff(&d);
        let rules: Vec<&str> = f.iter().map(|x| x.rule.as_str()).collect();
        assert_eq!(rules.iter().filter(|r| **r == "dash").count(), 1);
        assert_eq!(rules.iter().filter(|r| **r == "placeholder").count(), 2);
        assert_eq!(rules.iter().filter(|r| **r == "hidden_unicode").count(), 2);
        assert!(f.iter().any(|x| x.detail.contains("U+E0041")));
        // An HTML placeholder attribute is ordinary UI text, not a placeholder marker.
        assert!(!f.iter().any(|x| x.line == Some(7)));
    }

    #[test]
    fn secrets_are_found_and_never_repeated() {
        let aws = format!("key = {}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        let gh = format!("{}{}", "ghp_", "a".repeat(36));
        let quoted = format!("\"api_key\": \"{}\"", "Z9".repeat(10));
        let pem = format!("-----BEGIN RSA {} KEY-----", "PRIVATE");
        let d = diff_of(
            "config/app.json",
            &[&aws, &gh, &quoted, &pem, "token = os.environ[\"TOKEN\"]"],
        );
        let f = check_diff(&d);
        let secrets: Vec<&Finding> = f.iter().filter(|x| x.rule == "secret").collect();
        assert_eq!(secrets.len(), 4, "{f:?}");
        for s in &secrets {
            assert!(!s.detail.contains("ABCDEFGHIJKLMNOP") && !s.detail.contains(&"a".repeat(36)));
        }
        // Lock files are full of hashes; only the secret rule reads them.
        let lock = diff_of(
            "package-lock.json",
            &["\"integrity\": \"sha512-abc\u{2014}\""],
        );
        assert!(check_diff(&lock).is_empty());
    }

    #[test]
    fn code_under_src_needs_a_docs_change_in_the_same_pull_request() {
        let code = diff_of("src-tauri/src/x.rs", &["pub fn x() {}"]);
        let f = check_diff(&code);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "docs");
        let with_docs = format!("{code}{}", diff_of("docs/X.md", &["Explains x."]));
        assert!(check_diff(&with_docs).is_empty());
        // Code outside src (a sandbox lib.js, a test) does not need docs.
        assert!(check_diff(&diff_of("lib.js", &["module.exports = {}"])).is_empty());
        assert!(check_diff(&diff_of("src-tauri/tests/t.rs", &["#[test] fn t() {}"])).is_empty());
    }

    #[test]
    fn a_trimmed_diff_does_not_pass_and_the_note_lists_findings() {
        let d = format!("{}\n(output trimmed)", diff_of("docs/X.md", &["ok"]));
        let f = check_diff(&d);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "too_large");
        let n = note(&[Finding {
            rule: "dash".into(),
            path: "a.md".into(),
            line: Some(3),
            detail: "an em dash or en dash".into(),
        }]);
        assert!(n.starts_with("Constitution gate: 1 finding(s)."));
        assert!(n.contains("- a.md:3 dash: an em dash or en dash"));
        let mut many = Vec::new();
        for i in 0..45 {
            many.push(Finding {
                rule: "dash".into(),
                path: "a.md".into(),
                line: Some(i),
                detail: "x".into(),
            });
        }
        assert!(note(&many).contains("- and 5 more"));
    }

    /// Real pull requests through `gh`: prints every finding so false positives show up.
    /// Run: cargo test --lib constitution_gate::tests::real_pull_requests -- --ignored --nocapture
    #[test]
    #[ignore = "reads real pull requests through gh"]
    fn real_pull_requests() {
        let prs = std::env::var("GATE_PRS").unwrap_or_else(|_| {
            "konethegreat/agentos-studio#125,konethegreat/agentos-studio#126,konethegreat/agentos-studio#127,konethegreat/agentos-studio#129,konethegreat/agentos-sandbox-1#16".into()
        });
        for pr in prs.split(',') {
            let (repo, n) = pr.split_once('#').unwrap();
            let f = check_pr(repo, n.parse().unwrap()).unwrap();
            println!("{pr}: {} finding(s)", f.len());
            for x in &f {
                println!("  {}:{:?} {} {}", x.path, x.line, x.rule, x.detail);
            }
        }
    }

    #[test]
    fn binary_and_deleted_files_are_skipped_and_a_bom_is_allowed() {
        let d = "diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\ndiff --git a/old.md b/old.md\n--- a/old.md\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
        let files = parse_diff(d);
        assert!(files[0].binary && files[1].deleted);
        assert!(check_diff(d).is_empty());
        let bom = diff_of("docs/new.md", &[]).replace(
            "@@ -1,1 +1,1 @@\n context\n",
            "@@ -0,0 +1,1 @@\n+\u{FEFF}# Title\n",
        );
        assert!(check_diff(&bom).is_empty(), "{:?}", check_diff(&bom));
    }
}
