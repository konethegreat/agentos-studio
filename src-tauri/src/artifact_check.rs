//! Deterministic checks for an artifact folder.
//!
//! Publish runs these first, so a broken page never reaches GitHub Pages. The checks are static
//! and cheap: every file Publish would copy, HTML tag balance, local asset references, JavaScript
//! syntax through `node --check` when Node is installed, placeholder markers, banned dashes, and
//! URLs that only work on this machine. Nothing here calls a model.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::procs;

const MAX_ISSUES: usize = 60;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const NODE_LIMIT: Duration = Duration::from_secs(20);

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Issue {
    pub file: String,
    pub line: u32,
    pub rule: String,
    pub message: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Report {
    pub ok: bool,
    pub issues: Vec<Issue>,
}

impl Report {
    fn finish(mut issues: Vec<Issue>) -> Report {
        let ok = issues.is_empty();
        issues.truncate(MAX_ISSUES);
        Report { ok, issues }
    }
}

fn issue(file: &str, line: u32, rule: &str, message: impl Into<String>) -> Issue {
    Issue {
        file: file.to_string(),
        line,
        rule: rule.to_string(),
        message: message.into(),
    }
}

fn is_text_ext(ext: &str) -> bool {
    matches!(
        ext,
        "html" | "css" | "js" | "mjs" | "json" | "webmanifest" | "svg" | "md" | "txt"
    )
}

/// The rules that need only the file text. Used by the customizer on a simulated result, so a plan
/// that introduces banned content is refused before anything is written.
pub fn text_issues(file: &str, text: &str) -> Vec<Issue> {
    let mut out = Vec::new();
    if let Some((idx, _)) = text
        .char_indices()
        .find(|(_, c)| *c == '\u{2014}' || *c == '\u{2013}')
    {
        out.push(issue(
            file,
            line_at(text, idx),
            "dash",
            "uses an em dash or en dash; use a hyphen, a colon or a period",
        ));
    }
    for (token, rule, message) in [
        ("TODO", "placeholder", "contains a TODO marker"),
        ("FIXME", "placeholder", "contains a FIXME marker"),
    ] {
        if let Some(idx) = find_word(text, token) {
            out.push(issue(file, line_at(text, idx), rule, message));
        }
    }
    let lower = text.to_ascii_lowercase();
    for (needle, message) in [
        ("implement later", "contains placeholder wording"),
        ("lorem ipsum", "contains lorem ipsum filler text"),
    ] {
        if let Some(idx) = lower.find(needle) {
            out.push(issue(file, line_at(text, idx), "placeholder", message));
        }
    }
    for needle in ["localhost", "127.0.0.1", "0.0.0.0"] {
        if let Some(idx) = lower.find(needle) {
            out.push(issue(
                file,
                line_at(text, idx),
                "local-url",
                format!("addresses {needle}, which does not exist for a public page"),
            ));
            break;
        }
    }
    out
}

/// Check a whole artifact folder. `node` turns the JavaScript syntax check on.
pub fn check_with(root: &Path, node: bool) -> Report {
    let mut issues = Vec::new();
    if !root.is_dir() {
        issues.push(issue(
            "index.html",
            1,
            "missing-index",
            "the artifact folder does not exist",
        ));
        return Report::finish(issues);
    }
    match fs::metadata(root.join("index.html")) {
        Ok(m) if m.len() > 0 => {}
        _ => issues.push(issue(
            "index.html",
            1,
            "missing-index",
            "index.html is missing or empty; the public page needs one",
        )),
    }

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for entry in walkdir::WalkDir::new(root).max_depth(3).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.split('/').any(|s| s.starts_with('.')) {
            continue;
        }
        files.push((rel, entry.path().to_path_buf()));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    for (rel, path) in files {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size > MAX_FILE_BYTES {
            issues.push(issue(
                &rel,
                0,
                "file-size",
                "is larger than 1 MB, which stops the publish",
            ));
        }
        if rel == "agentos.js" || !is_text_ext(&ext) {
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => {
                issues.push(issue(&rel, 0, "encoding", "is not UTF-8 text"));
                continue;
            }
        };
        issues.extend(text_issues(&rel, &text));
        if ext == "html" {
            issues.extend(html_issues(&rel, &text));
            issues.extend(reference_issues(&rel, &text, root, RefKind::Html));
        }
        if ext == "css" {
            issues.extend(reference_issues(&rel, &text, root, RefKind::Css));
        }
        if (ext == "js" || ext == "mjs") && node && !has_module_syntax(&text) {
            issues.extend(node_issues(&rel, &path));
        }
    }
    Report::finish(issues)
}

pub fn check(root: &Path) -> Report {
    check_with(root, procs::find("node").is_some())
}

fn line_at(text: &str, idx: usize) -> u32 {
    text[..idx].bytes().filter(|b| *b == b'\n').count() as u32 + 1
}

fn find_word(text: &str, word: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(p) = text[from..].find(word) {
        let start = from + p;
        let end = start + word.len();
        let before = if start == 0 { 0 } else { bytes[start - 1] };
        let after = bytes.get(end).copied().unwrap_or(0);
        let boundary = |b: u8| !(b as char).is_ascii_alphanumeric() && b != b'_';
        if boundary(before) && boundary(after) {
            return Some(start);
        }
        from = end;
    }
    None
}

fn has_module_syntax(text: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?m)^\s*(?:import|export)\b").unwrap());
    re.is_match(text)
}

fn node_issues(rel: &str, path: &Path) -> Vec<Issue> {
    let Ok((code, _out, err)) = procs::run_with_timeout(
        "node",
        &["--check", &path.display().to_string()],
        None,
        path.parent(),
        NODE_LIMIT,
    ) else {
        return Vec::new();
    };
    if code == Some(0) {
        return Vec::new();
    }
    let detail = err
        .lines()
        .find(|l| l.contains("Error") || l.contains("error"))
        .unwrap_or("")
        .trim()
        .chars()
        .take(240)
        .collect::<String>();
    let line = line_from_node(&err).unwrap_or(1);
    vec![issue(
        rel,
        line,
        "js-syntax",
        if detail.is_empty() {
            "does not parse as JavaScript".to_string()
        } else {
            format!("does not parse as JavaScript: {detail}")
        },
    )]
}

fn line_from_node(err: &str) -> Option<u32> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r":(\d+)\r?\n").unwrap());
    re.captures(err)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

fn is_void(name: &str) -> bool {
    VOID.contains(&name)
}

fn is_raw_text(name: &str) -> bool {
    matches!(name, "script" | "style" | "textarea" | "title")
}

fn html_issues(rel: &str, text: &str) -> Vec<Issue> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut stack: Vec<(String, u32)> = Vec::new();
    let mut i = 0usize;
    let mut line = 1u32;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if text[i..].starts_with("<!--") {
            let end = text[i + 4..]
                .find("-->")
                .map(|p| i + 4 + p + 3)
                .unwrap_or(bytes.len());
            line += text[i..end].bytes().filter(|b| *b == b'\n').count() as u32;
            i = end;
            continue;
        }
        if text[i..].starts_with("<!") || text[i..].starts_with("<?") {
            let end = text[i..].find('>').map(|p| i + p + 1).unwrap_or(bytes.len());
            line += text[i..end].bytes().filter(|b| *b == b'\n').count() as u32;
            i = end;
            continue;
        }
        let start_line = line;
        let mut j = i + 1;
        let closing = bytes.get(j).copied() == Some(b'/');
        if closing {
            j += 1;
        }
        let name_start = j;
        while j < bytes.len() && (bytes[j] as char).is_ascii_alphanumeric() {
            j += 1;
        }
        if j == name_start {
            i += 1;
            continue;
        }
        let name = text[name_start..j].to_ascii_lowercase();
        let mut k = j;
        let mut quote: Option<u8> = None;
        while k < bytes.len() {
            let c = bytes[k];
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None if c == b'"' || c == b'\'' => quote = Some(c),
                None if c == b'>' => break,
                None => {}
            }
            k += 1;
        }
        if k >= bytes.len() {
            out.push(issue(rel, start_line, "unclosed-tag", format!("<{name}> never closes with >")));
            break;
        }
        let self_closing = k > j && bytes[k - 1] == b'/';
        line += text[j..=k].bytes().filter(|b| *b == b'\n').count() as u32;
        i = k + 1;
        if closing {
            match stack.iter().rposition(|(n, _)| n == &name) {
                Some(pos) => {
                    let dropped: Vec<(String, u32)> = stack.split_off(pos + 1);
                    for (dropped_name, dropped_line) in dropped {
                        out.push(issue(
                            rel,
                            dropped_line,
                            "unclosed-tag",
                            format!("<{dropped_name}> is not closed before </{name}>"),
                        ));
                    }
                    stack.pop();
                }
                None => out.push(issue(
                    rel,
                    start_line,
                    "stray-close",
                    format!("</{name}> has no matching opening tag"),
                )),
            }
        } else if !is_void(&name) && !self_closing {
            stack.push((name.clone(), start_line));
            if is_raw_text(&name) {
                let needle = format!("</{name}");
                let lower = text[i..].to_ascii_lowercase();
                match lower.find(&needle) {
                    Some(p) => {
                        let close_start = i + p;
                        let end = text[close_start..]
                            .find('>')
                            .map(|q| close_start + q + 1)
                            .unwrap_or(bytes.len());
                        line += text[i..end].bytes().filter(|b| *b == b'\n').count() as u32;
                        i = end;
                        stack.pop();
                    }
                    None => {
                        out.push(issue(
                            rel,
                            start_line,
                            "unclosed-tag",
                            format!("<{name}> is never closed"),
                        ));
                        stack.pop();
                    }
                }
            }
        }
    }
    for (name, at) in stack {
        out.push(issue(rel, at, "unclosed-tag", format!("<{name}> is never closed")));
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum RefKind {
    Html,
    Css,
}

fn reference_issues(rel: &str, text: &str, root: &Path, kind: RefKind) -> Vec<Issue> {
    let mut out = Vec::new();
    let base = Path::new(rel).parent().unwrap_or(Path::new(""));
    for (reference, offset) in match kind {
        RefKind::Html => html_refs(text),
        RefKind::Css => css_refs(text),
    } {
        let reference = reference.trim();
        if reference.is_empty() {
            out.push(issue(rel, line_at(text, offset), "empty-link", "has an empty src or href"));
            continue;
        }
        let lower = reference.to_ascii_lowercase();
        if lower.starts_with('#')
            || lower.starts_with("data:")
            || lower.starts_with("mailto:")
            || lower.starts_with("tel:")
            || lower.starts_with("javascript:")
            || lower.starts_with("http://")
            || lower.starts_with("https://")
        {
            continue;
        }
        if lower.starts_with("//") {
            continue;
        }
        if lower.starts_with('/') {
            out.push(issue(
                rel,
                line_at(text, offset),
                "root-path",
                format!("{reference} starts at the site root, but this app lives under /a/<slug>/ on GitHub Pages"),
            ));
            continue;
        }
        if reference.contains('%') || reference.contains('\\') {
            continue;
        }
        let clean = reference.split(['?', '#']).next().unwrap_or(reference);
        if clean.is_empty() {
            continue;
        }
        let Some(target) = join_under(root, base, clean) else {
            out.push(issue(
                rel,
                line_at(text, offset),
                "missing-asset",
                format!("{reference} leaves the artifact folder"),
            ));
            continue;
        };
        let target = if clean.ends_with('/') { target.join("index.html") } else { target };
        if !target.is_file() {
            out.push(issue(
                rel,
                line_at(text, offset),
                "missing-asset",
                format!("{reference} does not exist in the artifact folder"),
            ));
        }
    }
    out
}

fn join_under(root: &Path, base: &Path, reference: &str) -> Option<PathBuf> {
    let mut path = root.join(base);
    for part in reference.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if !path.starts_with(root) || path == root {
                return None;
            }
            path.pop();
            continue;
        }
        if Path::new(part).components().any(|c| !matches!(c, Component::Normal(_))) {
            return None;
        }
        path.push(part);
    }
    path.starts_with(root).then_some(path)
}

fn html_refs(text: &str) -> Vec<(String, usize)> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r#"(?i)\b(?:src|href)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap()
    });
    re.captures_iter(text)
        .map(|c| {
            let value = c.get(1).or_else(|| c.get(2)).map(|m| m.as_str()).unwrap_or("");
            (value.to_string(), c.get(0).map(|m| m.start()).unwrap_or(0))
        })
        .collect()
}

fn css_refs(text: &str) -> Vec<(String, usize)> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r#"(?i)url\(\s*(?:"([^"]*)"|'([^']*)'|([^'")]+))\s*\)"#).unwrap()
    });
    re.captures_iter(text)
        .map(|c| {
            let value = c
                .get(1)
                .or_else(|| c.get(2))
                .or_else(|| c.get(3))
                .map(|m| m.as_str())
                .unwrap_or("");
            (value.trim().to_string(), c.get(0).map(|m| m.start()).unwrap_or(0))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let target = dir.path().join(path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(target, content).unwrap();
        }
        dir
    }

    fn page(body: &str) -> String {
        format!("<!doctype html>\n<html lang=\"en\"><head><title>x</title><link rel=\"stylesheet\" href=\"styles.css\"></head><body>{body}</body></html>\n")
    }

    const GOOD_CSS: &str = "body { margin: 0; color: #123; }\n";

    #[test]
    fn a_clean_page_passes_every_rule() {
        let dir = fixture(&[
            ("index.html", &page("<main><h1>Hi</h1><p>Ready.</p></main>")),
            ("styles.css", GOOD_CSS),
        ]);
        let report = check_with(dir.path(), false);
        assert!(report.ok, "{:?}", report.issues);
    }

    #[test]
    fn a_missing_index_or_empty_page_blocks_publish() {
        let dir = fixture(&[("styles.css", GOOD_CSS)]);
        let report = check_with(dir.path(), false);
        assert!(!report.ok);
        assert_eq!(report.issues[0].rule, "missing-index");

        let empty = fixture(&[("index.html", "")]);
        assert!(!check_with(empty.path(), false).ok);
    }

    #[test]
    fn unbalanced_containers_are_reported_with_their_line() {
        let html = "<!doctype html>\n<html>\n<body>\n  <div>\n    <section>\n      <p>x</p>\n    </div>\n  </body>\n</html>\n";
        let dir = fixture(&[("index.html", html)]);
        let report = check_with(dir.path(), false);
        let unclosed: Vec<&Issue> = report.issues.iter().filter(|i| i.rule == "unclosed-tag").collect();
        assert_eq!(unclosed.len(), 1, "{:?}", report.issues);
        assert_eq!(unclosed[0].file, "index.html");
        assert_eq!(unclosed[0].line, 5);

        let stray = fixture(&[("index.html", &page("<p>x</p></div>"))]);
        let report = check_with(stray.path(), false);
        assert!(report.issues.iter().any(|i| i.rule == "stray-close"), "{:?}", report.issues);
    }

    #[test]
    fn script_and_style_bodies_do_not_break_balance() {
        let html = "<!doctype html>\n<html><head><style>a > b { color: red }</style></head>\n<body><script>if (1 < 2) { document.write(\"<div>\"); }</script></body></html>\n";
        let dir = fixture(&[("index.html", html)]);
        let report = check_with(dir.path(), false);
        assert!(report.ok, "{:?}", report.issues);
    }

    #[test]
    fn missing_assets_and_root_paths_are_errors() {
        let html = page("<img src=\"logo.png\" alt=\"logo\"><a href=\"/about.html\">About</a>");
        let dir = fixture(&[("index.html", &html), ("styles.css", GOOD_CSS)]);
        let report = check_with(dir.path(), false);
        assert!(report.issues.iter().any(|i| i.rule == "missing-asset"), "{:?}", report.issues);
        assert!(report.issues.iter().any(|i| i.rule == "root-path"), "{:?}", report.issues);

        let fixed = fixture(&[
            ("index.html", &page("<a href=\"about.html\">About</a>")),
            ("about.html", &page("<p>About</p>")),
            ("styles.css", GOOD_CSS),
        ]);
        assert!(check_with(fixed.path(), false).ok);
    }

    #[test]
    fn css_urls_are_checked_relative_to_the_stylesheet() {
        let css = "body { background: url(\"art/bg.png\"); }\n";
        let dir = fixture(&[("index.html", &page("<p>x</p>")), ("styles.css", css)]);
        let report = check_with(dir.path(), false);
        assert!(report.issues.iter().any(|i| i.rule == "missing-asset"), "{:?}", report.issues);
        let good = fixture(&[
            ("index.html", &page("<p>x</p>")),
            ("styles.css", css),
            ("art/bg.png", "png"),
        ]);
        assert!(check_with(good.path(), false).ok);
    }

    #[test]
    fn placeholder_dashes_and_local_urls_are_refused() {
        let dir = fixture(&[(
            "index.html",
            &page("<p>Implement later</p><p>Owner: 127.0.0.1</p>"),
        )]);
        let report = check_with(dir.path(), false);
        assert!(report.issues.iter().any(|i| i.rule == "placeholder"), "{:?}", report.issues);
        assert!(report.issues.iter().any(|i| i.rule == "local-url"), "{:?}", report.issues);

        let dash = fixture(&[("index.html", &page("<p>one \u{2014} two</p>"))]);
        assert!(check_with(dash.path(), false).issues.iter().any(|i| i.rule == "dash"));

        let marker = fixture(&[("app.js", "// TODO wire this up\n")]);
        fs::write(marker.path().join("index.html"), page("<p>x</p>")).unwrap();
        assert!(check_with(marker.path(), false).issues.iter().any(|i| i.rule == "placeholder"));
    }

    #[test]
    fn todo_inside_a_word_is_not_a_marker() {
        assert_eq!(find_word("Todos are lowercase", "TODO"), None);
        assert_eq!(find_word("// TODO: fix", "TODO"), Some(3));
        assert_eq!(find_word("FIXME\n", "FIXME"), Some(0));
    }

    #[test]
    fn the_shipped_templates_pass() {
        let dir = fixture(&[("index.html", &page("<p>x</p>"))]);
        let parent = dir.path();
        crate::artifacts::set_test_root(Some(parent.join("artifacts")));
        let c = crate::store::test_conn();
        for template in ["blank", "todo", "notes"] {
            let artifact = crate::artifacts::create_in(&c, &format!("Probe {template}"), "", template).unwrap();
            let report = check_with(&crate::artifacts::artifact_dir(&artifact.slug), false);
            assert!(report.ok, "{template}: {:?}", report.issues);
        }
        crate::artifacts::set_test_root(None);
    }

    #[test]
    fn text_issues_are_shared_with_the_customizer() {
        assert!(text_issues("a.js", "// TODO: later").iter().any(|i| i.rule == "placeholder"));
        assert!(text_issues("a.css", "x { color: red }").is_empty());
        assert!(text_issues("a.html", "see http://localhost:3000").iter().any(|i| i.rule == "local-url"));
    }
}
