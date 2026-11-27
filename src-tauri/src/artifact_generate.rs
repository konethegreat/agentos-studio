//! Description to artifact: one headless turn researches the brief and returns a whole static app.
//!
//! The agent never writes files. It answers with `{"summary":"...","files":[{"path","content"}]}`.
//! Studio validates every path and every file with the same rules Publish runs, writes the set to
//! a staging folder, runs the publish checks, and only then creates the artifact. A refused answer
//! leaves nothing behind. The `template` column stores the kind, `generated`, so the gallery and
//! manifest can tell generated apps from edited ones without a schema change.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::artifacts::Artifact;

/// A whole app takes longer than a customization. The limit is enforced by `procs`.
pub const GENERATE_LIMIT: Duration = Duration::from_secs(360);
const MAX_BRIEF: usize = 4_000;
const MIN_BRIEF: usize = 10;
const MAX_NAME: usize = 80;
const MAX_FILES: usize = 24;
const MAX_FILE_CHARS: usize = 400_000;
const MAX_TOTAL_CHARS: usize = 900_000;

/// Research tools for a Claude Code generation turn. Antigravity gets its usual read-only rules.
const CLAUDE_TOOLS: [&str; 8] = [
    "mcp__agentos-hub__recall",
    "mcp__agentos-hub__project_brief",
    "mcp__agentos-hub__search_shared_memory",
    "WebSearch",
    "WebFetch",
    "Read",
    "Grep",
    "Glob",
];

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Generated {
    pub artifact: Artifact,
    pub summary: String,
    pub agent: String,
    pub duration_ms: i64,
    pub cost_usd: f64,
}

#[derive(Deserialize)]
struct RawApp {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    files: Vec<RawFile>,
}

#[derive(Deserialize)]
struct RawFile {
    path: String,
    #[serde(default)]
    content: String,
}

/// The prompt a headless agent answers with a whole app. Pure.
pub fn generation_prompt(name: &str, brief: &str) -> String {
    format!(
        "You are building a new web artifact for AgentOS Studio. It is a complete static web app \
that will be previewed locally and published to GitHub Pages under /a/<slug>/. Everything in it \
must work in any modern browser with no server.\n\n\
## The brief\n{name}: {brief}\n\n\
## Research first\n\
Think like a product engineer before writing code. Work out who uses this, the exact job it does, \
the data it keeps, the defaults, the empty states and the edge cases. Use the research tools you \
have (web search and fetch when available, the agentos-hub memory tools) and your own knowledge to \
check facts and current practice. Build the app this brief asks for, not a generic template with \
the brief painted on. If the brief names a domain, use its real vocabulary and workflow.\n\n\
## What to build\n\
- index.html is required. Link styles.css and app.js with relative paths (href=\"styles.css\", \
src=\"app.js\"). Never use root-relative URLs like /styles.css: the app lives under /a/<slug>/.\n\
- Make it genuinely functional: real state in localStorage, forms that validate, filters and \
sorting that work, keyboard support, clear empty states, and a layout that works from 360 px wide \
up.\n\
- Use plain HTML, CSS and JavaScript. No frameworks, no build step.\n\
- Self-contained: no CDN links, external fonts, remote APIs, analytics or images that need the \
network. The page must work offline.\n\
- Accessible: html lang, a descriptive title, labels on inputs, aria attributes only where they add \
meaning, visible focus, readable contrast.\n\
- Visual quality: a deliberate, calm design with a clear hierarchy. No filler text and no dead \
buttons.\n\
- Never use em dashes or en dashes. Use a hyphen, a colon or a period. This is enforced.\n\
- Do not write TODO, FIXME, \"implement later\", lorem ipsum, or coming-soon copy.\n\
- Do not address localhost, 127.0.0.1 or 0.0.0.0.\n\
- Do not create agentos.js; Studio generates it on publish. Keep files at most {MAX_FILES} and the \
whole app under about 120 KB.\n\
- Allowed file types: html, css, js, mjs, json, svg, md, txt, webmanifest.\n\n\
## Output\n\
Return one JSON object and nothing else, with no markdown fences. Keep the JSON compact (no pretty \
printing) so the whole app fits in one reply.\n\
{{\"summary\":\"one line describing the app\",\"files\":[{{\"path\":\"index.html\",\"content\":\"complete \
file text\"}},{{\"path\":\"styles.css\",\"content\":\"complete file text\"}}]}}\n\
Every content value must be the complete final file, escaped as valid JSON.\n"
    )
}

fn clean_dashes(text: &str) -> String {
    text.replace(['\u{2014}', '\u{2013}'], "-")
}

/// Validate and normalise the agent's answer into `(summary, files)`. Pure.
pub fn validate_app(raw: &str) -> Result<(String, Vec<(String, String)>)> {
    let json = crate::artifact_customize::extract_json(raw)?;
    let parsed: RawApp = serde_json::from_str(&json)
        .map_err(|e| anyhow!("The agent returned JSON that does not match the app shape: {e}"))?;
    if parsed.files.is_empty() {
        bail!("The agent returned no files");
    }
    if parsed.files.len() > MAX_FILES {
        bail!("The app has more than {MAX_FILES} files; ask for a smaller app");
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut files: Vec<(String, String)> = Vec::new();
    let mut total = 0usize;
    for (index, file) in parsed.files.iter().enumerate() {
        let step = index + 1;
        let raw_path = file.path.trim().replace('\\', "/");
        let path = raw_path.strip_prefix("./").unwrap_or(&raw_path).to_string();
        if path == "agentos.js" {
            bail!("file {step}: agentos.js is generated by Studio and cannot be part of an app");
        }
        if !crate::artifacts::valid_rel(&path) {
            bail!("file {step}: {raw_path} is not a publishable text file");
        }
        if !seen.insert(path.clone()) {
            bail!("file {step}: {raw_path} appears twice");
        }
        let content = clean_dashes(&file.content);
        if content.trim().is_empty() {
            bail!("file {step}: {path} is empty");
        }
        if content.chars().count() > MAX_FILE_CHARS {
            bail!("file {step}: {path} is larger than {MAX_FILE_CHARS} characters");
        }
        if let Some(first) = crate::artifact_check::text_issues(&path, &content).first() {
            bail!("{path}:{} {}", first.line, first.message);
        }
        total += content.len();
        files.push((path, content));
    }
    if !seen.contains("index.html") {
        bail!("The app has no index.html");
    }
    if total > MAX_TOTAL_CHARS {
        bail!("The app is larger than {MAX_TOTAL_CHARS} bytes; ask for a smaller app");
    }
    let summary: String = clean_dashes(parsed.summary.trim())
        .chars()
        .take(500)
        .collect();
    Ok((summary, files))
}

fn write_files(dir: &Path, files: &[(String, String)]) -> Result<()> {
    for (path, content) in files {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, content).with_context(|| format!("writing {}", target.display()))?;
    }
    Ok(())
}

/// Remove anything the agent wrote outside the answer, so a stray file cannot fail the publish
/// check or reach the artifact folder.
fn prune(dir: &Path, keep: &[(String, String)]) {
    let keep: HashSet<&str> = keep.iter().map(|(path, _)| path.as_str()).collect();
    for entry in walkdir::WalkDir::new(dir)
        .min_depth(1)
        .into_iter()
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(dir) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !keep.contains(rel.as_str()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Remove staging folders left by a generation that was killed before it could clean up. A live
/// turn owns its folder for at most the generation limit, so a day of age means nobody owns it.
fn prune_stale_staging() {
    let dir = crate::artifacts::root().join(".staging");
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        let stale = now
            .duration_since(modified)
            .map(|age| age > Duration::from_secs(86_400))
            .unwrap_or(false);
        if stale {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Run one research-and-build turn, check the result, and create the artifact. Writes nothing to
/// the artifact list unless every file passes the publish checks.
pub fn generate_in(
    c: &Connection,
    name: &str,
    brief: &str,
    agent: Option<&str>,
    limit: Duration,
) -> Result<Generated> {
    let name = name.trim();
    if name.is_empty() {
        bail!("Give the artifact a name");
    }
    if name.chars().count() > MAX_NAME {
        bail!("Artifact names are {MAX_NAME} characters or fewer");
    }
    let brief = brief.trim();
    if brief.chars().count() < MIN_BRIEF {
        bail!("Describe what the app should do in at least {MIN_BRIEF} characters");
    }
    if brief.chars().count() > MAX_BRIEF {
        bail!("Keep the description under {MAX_BRIEF} characters");
    }
    let fallback = crate::artifact_customize::default_agent()?;
    let requested = agent
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .unwrap_or(fallback);
    let agent = crate::artifact_customize::check_agent(requested)?;
    if crate::ask::answering(&agent) {
        bail!("{agent} is working on another artifact right now; try again in a minute");
    }

    let staging = crate::artifacts::root()
        .join(".staging")
        .join(uuid::Uuid::new_v4().simple().to_string());
    prune_stale_staging();
    fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;
    let prompt = generation_prompt(name, brief);
    let started = Instant::now();
    let model = (agent == "antigravity")
        .then(crate::artifact_customize::model_for_turn)
        .flatten();
    crate::ask::mark_answering(&agent, limit.as_secs() + 30);
    let turn = crate::ask::run_turn_with(
        &agent,
        &prompt,
        &staging,
        limit,
        &CLAUDE_TOOLS,
        model.as_deref(),
    );
    crate::ask::clear_answering(&agent);

    let result = (|| -> Result<Generated> {
        let (answer, _session, cost) = turn?;
        let (summary, files) = validate_app(&answer)?;
        write_files(&staging, &files)?;
        prune(&staging, &files);
        let report = crate::artifact_check::check(&staging);
        if !report.ok {
            let mut message = String::from("The app failed Studio's publish checks:");
            for issue in report.issues.iter().take(8) {
                message.push_str(&format!(
                    "\n{}:{} {}",
                    issue.file, issue.line, issue.message
                ));
            }
            bail!("{message}");
        }
        let description = if summary.is_empty() {
            brief.chars().take(200).collect::<String>()
        } else {
            summary.clone()
        };
        let artifact = crate::artifacts::create_with_files(c, name, &description, &files)?;
        Ok(Generated {
            artifact,
            summary,
            agent: agent.clone(),
            duration_ms: started.elapsed().as_millis() as i64,
            cost_usd: cost,
        })
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_json(files: &[(&str, &str)]) -> String {
        let items: Vec<String> = files
            .iter()
            .map(|(path, content)| {
                format!(
                    "{{\"path\":{},\"content\":{}}}",
                    serde_json::to_string(path).unwrap(),
                    serde_json::to_string(content).unwrap()
                )
            })
            .collect();
        format!(
            "{{\"summary\":\"A timer\",\"files\":[{}]}}",
            items.join(",")
        )
    }

    #[test]
    fn the_prompt_asks_for_research_and_a_complete_app() {
        let p = generation_prompt("Focus Timer", "A pomodoro timer with saved sessions");
        assert!(p.contains("Focus Timer"));
        assert!(p.contains("A pomodoro timer with saved sessions"));
        assert!(p.contains("Research first"));
        assert!(p.contains("index.html is required"));
        assert!(p.contains("relative paths"));
        assert!(p.contains("\"summary\""));
        assert!(!p.contains('\u{2014}'));
        assert!(!p.contains('\u{2013}'));
    }

    #[test]
    fn a_complete_app_is_accepted_and_dashes_are_normalised() {
        let raw = app_json(&[
            (
                "index.html",
                "<title>Timer</title><h1>Focus - timer</h1>\u{2014}",
            ),
            ("styles.css", "body { font-family: system-ui; }"),
            ("./app.js", "let seconds = 0;"),
        ]);
        let (summary, files) = validate_app(&raw).unwrap();
        assert_eq!(summary, "A timer");
        assert_eq!(files.len(), 3);
        assert_eq!(files[2].0, "app.js");
        assert!(!files[0].1.contains('\u{2014}'));
        assert!(files[0].1.contains('-'));
    }

    #[test]
    fn a_summary_with_a_banned_dash_is_normalised() {
        let raw = format!(
            "{{\"summary\":\"Focus \u{2014} timer\",\"files\":[{}]}}",
            "{\"path\":\"index.html\",\"content\":\"<p>x</p>\"}"
        );
        let (summary, _) = validate_app(&raw).unwrap();
        assert_eq!(summary, "Focus - timer");
    }

    #[test]
    fn incomplete_or_unsafe_apps_are_refused() {
        let cases = [
            (app_json(&[("app.js", "let a = 1;")]), "no index.html"),
            (
                app_json(&[("index.html", "<p>x</p>"), ("agentos.js", "bad")]),
                "generated by Studio",
            ),
            (
                app_json(&[("index.html", "<p>x</p>"), ("../x.html", "<p>y</p>")]),
                "not a publishable",
            ),
            (
                app_json(&[("index.html", "<p>x</p>"), ("index.html", "<p>y</p>")]),
                "appears twice",
            ),
            (
                app_json(&[("index.html", "<p>x</p>"), ("app.js", "// TODO wire this")]),
                "TODO",
            ),
            (app_json(&[("index.html", "  ")]), "empty"),
        ];
        for (raw, needle) in cases {
            let e = validate_app(&raw).unwrap_err().to_string();
            assert!(e.contains(needle), "{needle}: {e}");
        }
    }

    #[test]
    fn too_many_files_are_refused() {
        let items: Vec<String> = (0..=MAX_FILES)
            .map(|i| format!("{{\"path\":\"p{i}.html\",\"content\":\"<p>x</p>\"}}"))
            .collect();
        let raw = format!("{{\"files\":[{}]}}", items.join(","));
        let e = validate_app(&raw).unwrap_err().to_string();
        assert!(e.contains(&MAX_FILES.to_string()), "{e}");
    }
}
