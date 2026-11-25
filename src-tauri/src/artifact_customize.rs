//! The artifact customizer.
//!
//! Studio shows the artifact preview in a frame and injects `CUSTOMIZER_JS`, which turns clicks
//! into a selection description. That description and the current files go to a headless agent,
//! which answers with a JSON replace plan. Studio validates every replacement against the files,
//! shows the diff, and writes only after the owner applies it. The agent never writes files.
//!
//! The rules in `artifact_check` run on every replacement, so a plan cannot introduce placeholder
//! text, banned dashes or machine-only URLs. Publish runs the full check again.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::artifacts::{self, Artifact};

pub const TURN_LIMIT: Duration = Duration::from_secs(180);
const CONTEXT_FILE: usize = 48_000;
const CONTEXT_TOTAL: usize = 180_000;
const MAX_EDITS: usize = 12;
const MAX_EDIT_CHARS: usize = 60_000;
const MAX_PLAN_CHARS: usize = 400_000;
const MAX_INSTRUCTION: usize = 2_000;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct AgentChoice {
    pub id: String,
    pub name: String,
    pub installed: bool,
}

/// The headless agents that can take a customization, in preference order.
pub fn agents() -> Vec<AgentChoice> {
    [
        ("claude-code", "Claude Code"),
        ("antigravity", "Antigravity"),
        ("hermes", "Hermes"),
    ]
    .into_iter()
    .map(|(id, name)| AgentChoice {
        id: id.to_string(),
        name: name.to_string(),
        installed: crate::bus::headless_available(id),
    })
    .collect()
}

/// The agent used when the caller names none: the best flat-rate headless agent installed here.
pub fn default_agent() -> Result<&'static str> {
    crate::bus::best_headless("", crate::bus::headless_available).ok_or_else(|| {
        anyhow!("Install Claude Code, Antigravity or Hermes to customize artifacts with AI")
    })
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Selection {
    #[serde(default)]
    pub selector: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub classes: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub html: String,
}

impl Selection {
    fn bounded(&self) -> Selection {
        let cut = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
        Selection {
            selector: cut(&self.selector, 600),
            tag: cut(&self.tag, 40),
            id: cut(&self.id, 120),
            classes: cut(&self.classes, 300),
            text: cut(&self.text, 300),
            html: cut(&self.html, 4_000),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Edit {
    pub path: String,
    pub old: String,
    pub new: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub line: u32,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Plan {
    pub summary: String,
    pub edits: Vec<Edit>,
    pub agent: String,
    pub duration_ms: i64,
    pub cost_usd: f64,
}

#[derive(Deserialize)]
struct RawPlan {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    files: Vec<RawEdit>,
}

#[derive(Deserialize)]
struct RawEdit {
    path: String,
    #[serde(default)]
    old: String,
    #[serde(default)]
    new: String,
    #[serde(default)]
    why: String,
}

/// The prompt a headless agent answers with a plan. Pure.
pub fn plan_prompt(
    name: &str,
    slug: &str,
    selection: &Selection,
    instruction: &str,
    files: &BTreeMap<String, String>,
) -> String {
    let mut p = format!(
        "You are editing one web artifact for AgentOS Studio. The artifact is \"{name}\" (slug {slug}), a static folder of HTML, CSS and JavaScript published to GitHub Pages under /a/{slug}/. The owner selected one element in the live preview and asked for a change.\n\n\
## Selected element\nselector: {selector}\ntag: {tag}\nid: {id}\nclasses: {classes}\ntext: {text}\n\nowner HTML (may show the DOM after scripts ran):\n{html}\n\n\
## Requested change\n{instruction}\n\n\
## Rules\n\
- Change only what the request needs. The page must keep working after the edit.\n\
- Each edit replaces one exact, unique piece of the current file text. `old` must be copied exactly from the file contents below, including whitespace. `new` is what replaces it.\n\
- If `old` is empty, the edit creates a new file and the path must not exist yet.\n\
- Paths are relative to the artifact folder, for example index.html or styles.css. Never edit agentos.js.\n\
- Do not add placeholder text, TODO or FIXME markers, lorem ipsum, or coming-soon copy.\n\
- Never use em dashes or en dashes. Use a hyphen, a colon or a period.\n\
- Do not address localhost, 127.0.0.1 or 0.0.0.0.\n\
- Do not add network dependencies that a static page cannot load. Prefer the existing styles and scripts.\n\
- Never use root-relative paths like /styles.css; the app lives under /a/{slug}/.\n\
- Keep the change small enough to review. At most {MAX_EDITS} edits.\n\n\
## Current files\n",
        selector = if selection.selector.is_empty() { "(unknown)" } else { &selection.selector },
        tag = selection.tag,
        id = selection.id,
        classes = selection.classes,
        text = selection.text,
        html = selection.html,
        instruction = instruction.trim(),
    );
    for (path, content) in files {
        p.push_str(&format!(
            "\n===== FILE {path} =====\n{content}\n===== END {path} =====\n"
        ));
    }
    p.push_str(
        "\n## Output\nReturn one JSON object and nothing else. No markdown fences, no commentary.\n\
{\"summary\":\"one line describing the change\",\"files\":[{\"path\":\"styles.css\",\"old\":\"exact current text\",\"new\":\"replacement text\",\"why\":\"short reason\"}]}\n",
    );
    p
}

/// The JSON object inside an agent reply, with fences and prose stripped. Pure.
pub fn extract_json(text: &str) -> Result<String> {
    let start = text
        .find('{')
        .ok_or_else(|| anyhow!("The agent did not return a JSON plan"))?;
    let end = text
        .rfind('}')
        .filter(|e| *e > start)
        .ok_or_else(|| anyhow!("The agent returned JSON that never closes"))?;
    Ok(text[start..=end].to_string())
}

fn normalize_path(path: &str) -> Result<String> {
    let raw = path.trim().replace('\\', "/");
    let raw = raw.strip_prefix("./").unwrap_or(&raw).to_string();
    if raw == "agentos.js" {
        bail!("agentos.js is generated by Studio and cannot be edited");
    }
    if !artifacts::valid_rel(&raw) {
        bail!("{path} is not an editable artifact path");
    }
    Ok(raw)
}

/// Validate a plan against the current files and compute line numbers. Pure.
pub fn validate_edits(
    edits: &[Edit],
    files: &BTreeMap<String, String>,
    existing: &BTreeSet<String>,
) -> Result<Vec<Edit>> {
    if edits.is_empty() {
        bail!("The plan contains no changes");
    }
    if edits.len() > MAX_EDITS {
        bail!("The plan has more than {MAX_EDITS} edits; ask for a smaller change");
    }
    let mut working = files.clone();
    let mut out = Vec::new();
    let mut total = 0usize;
    for (index, edit) in edits.iter().enumerate() {
        let step = index + 1;
        let path = normalize_path(&edit.path).map_err(|e| anyhow!("edit {step}: {e}"))?;
        if edit.old.chars().count() > MAX_EDIT_CHARS || edit.new.chars().count() > MAX_EDIT_CHARS {
            bail!("edit {step}: {path} changes more than {MAX_EDIT_CHARS} characters at once");
        }
        total += edit.old.len() + edit.new.len();
        let mut line = 1u32;
        if edit.old.is_empty() {
            if existing.contains(&path) || working.contains_key(&path) {
                bail!("edit {step}: {path} already exists; send its current text in `old` to change it");
            }
            if edit.new.trim().is_empty() {
                bail!("edit {step}: {path} would be created empty");
            }
            working.insert(path.clone(), edit.new.clone());
        } else {
            let Some(content) = working.get(&path).cloned() else {
                bail!("edit {step}: {path} is not part of this artifact");
            };
            let count = content.matches(edit.old.as_str()).count();
            if count == 0 {
                bail!(
                    "edit {step}: the text to replace was not found in {path}. The plan is stale or the text was copied wrong; generate it again"
                );
            }
            if count > 1 {
                bail!("edit {step}: the text to replace appears {count} times in {path}; include more surrounding text so it is unique");
            }
            let at = content.find(edit.old.as_str()).unwrap_or(0);
            line = content[..at].bytes().filter(|b| *b == b'\n').count() as u32 + 1;
            working.insert(
                path.clone(),
                content.replacen(edit.old.as_str(), &edit.new, 1),
            );
        }
        if let Some(first) = crate::artifact_check::text_issues(&path, &edit.new).first() {
            bail!("edit {step}: the replacement in {path} {}", first.message);
        }
        out.push(Edit {
            path,
            old: edit.old.clone(),
            new: edit.new.clone(),
            why: edit.why.trim().chars().take(300).collect(),
            line,
        });
    }
    if total > MAX_PLAN_CHARS {
        bail!("The plan changes more than {MAX_PLAN_CHARS} characters at once; ask for a smaller change");
    }
    Ok(out)
}

/// Parse and validate the agent's answer. Pure.
pub fn validate_plan(
    raw: &str,
    files: &BTreeMap<String, String>,
    existing: &BTreeSet<String>,
) -> Result<(String, Vec<Edit>)> {
    let json = extract_json(raw)?;
    let parsed: RawPlan = serde_json::from_str(&json)
        .map_err(|e| anyhow!("The agent returned JSON that does not match the plan shape: {e}"))?;
    let summary = parsed.summary.trim().chars().take(500).collect::<String>();
    let edits: Vec<Edit> = parsed
        .files
        .into_iter()
        .map(|r| Edit {
            path: r.path,
            old: r.old,
            new: r.new,
            why: r.why,
            line: 0,
        })
        .collect();
    let checked = validate_edits(&edits, files, existing)?;
    Ok((summary, checked))
}

fn priority(path: &str) -> u8 {
    match path {
        "index.html" => 0,
        "styles.css" => 1,
        "app.js" => 2,
        p if p.ends_with(".html") => 3,
        p if p.ends_with(".css") => 4,
        p if p.ends_with(".js") || p.ends_with(".mjs") => 5,
        _ => 6,
    }
}

fn context_files(c: &Connection, id: &str) -> Result<(BTreeMap<String, String>, BTreeSet<String>)> {
    let mut listed = artifacts::files_in(c, id)?;
    let existing: BTreeSet<String> = listed.iter().map(|f| f.path.clone()).collect();
    listed.sort_by_key(|f| (priority(&f.path), f.path.clone()));
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    for item in listed {
        if item.path == "agentos.js" {
            continue;
        }
        let Ok(text) = artifacts::read_file_in(c, id, &item.path) else {
            continue;
        };
        let room = CONTEXT_TOTAL.saturating_sub(total);
        if room == 0 {
            break;
        }
        let text: String = text.chars().take(CONTEXT_FILE.min(room)).collect();
        total += text.chars().count();
        files.insert(item.path, text);
    }
    Ok((files, existing))
}

/// The agent a customization may run on. Unlike `ask`, there is no caller to protect from asking
/// itself; the owner picks the agent in the Studio panel.
fn check_agent(agent: &str) -> Result<String> {
    let to = crate::vault::norm_agent(agent)?;
    if to == "human" {
        bail!("The customizer runs on a headless agent, not on the owner");
    }
    if !crate::bus::is_headless(&to) {
        bail!("{to} runs inside its IDE and cannot take a customization turn");
    }
    if !crate::bus::headless_available(&to) {
        bail!("{to} is not installed on this machine");
    }
    Ok(to)
}

/// Run one headless turn and return a validated plan. Writes nothing.
pub fn customize_in(
    c: &Connection,
    id: &str,
    selection: &Selection,
    instruction: &str,
    agent: Option<&str>,
    limit: Duration,
) -> Result<Plan> {
    let instruction = instruction.trim();
    if instruction.is_empty() {
        bail!("Describe the change you want first");
    }
    if instruction.chars().count() > MAX_INSTRUCTION {
        bail!("Keep the request under {MAX_INSTRUCTION} characters");
    }
    let artifact = artifacts::get_in(c, id)?;
    let fallback = default_agent()?;
    let requested = agent
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .unwrap_or(fallback);
    let agent = check_agent(requested)?;
    if crate::ask::answering(&agent) {
        bail!("{agent} is answering another question right now; try again in a minute");
    }
    let (files, existing) = context_files(c, id)?;
    if files.is_empty() {
        bail!("This artifact has no editable text files yet");
    }
    let selection = selection.bounded();
    let prompt = plan_prompt(
        &artifact.name,
        &artifact.slug,
        &selection,
        instruction,
        &files,
    );
    let dir = artifacts::artifact_dir(&artifact.slug);
    let started = Instant::now();
    let (answer, _session, cost) = crate::ask::run_turn(&agent, &prompt, &dir, limit)?;
    let (summary, edits) = validate_plan(&answer, &files, &existing)?;
    Ok(Plan {
        summary,
        edits,
        agent: agent.to_string(),
        duration_ms: started.elapsed().as_millis() as i64,
        cost_usd: cost,
    })
}

/// Re-validate a plan against the files as they are now, then write every touched file once.
pub fn apply_in(c: &Connection, id: &str, edits: &[Edit]) -> Result<Artifact> {
    let listed = artifacts::files_in(c, id)?;
    let existing: BTreeSet<String> = listed.iter().map(|f| f.path.clone()).collect();
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    for edit in edits {
        wanted.insert(edit.path.trim().replace('\\', "/"));
    }
    let mut files = BTreeMap::new();
    for path in &wanted {
        if existing.contains(path) {
            files.insert(path.clone(), artifacts::read_file_in(c, id, path)?);
        }
    }
    let checked = validate_edits(edits, &files, &existing)?;
    let mut final_map = files.clone();
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for edit in &checked {
        touched.insert(edit.path.clone());
        if edit.old.is_empty() {
            final_map.insert(edit.path.clone(), edit.new.clone());
            continue;
        }
        if let Some(current) = final_map.get(&edit.path).cloned() {
            final_map.insert(
                edit.path.clone(),
                current.replacen(edit.old.as_str(), &edit.new, 1),
            );
        }
    }
    for path in &touched {
        let Some(text) = final_map.get(path) else {
            continue;
        };
        if text.len() as u64 > 1024 * 1024 {
            bail!("{path} would grow past 1 MB");
        }
        if let Some(first) = crate::artifact_check::text_issues(path, text).first() {
            bail!("{path}:{} {}", first.line, first.message);
        }
    }
    for path in &touched {
        let text = final_map.get(path).cloned().unwrap_or_default();
        artifacts::write_file_in(c, id, path, &text)?;
    }
    artifacts::get_in(c, id)
}

/// Injected into every HTML page served by the preview server. It does nothing until the Studio
/// frame enables it, so preview windows and the published site behave normally.
pub const CUSTOMIZER_JS: &str = r##"(function () {
  if (window.__agentosCustomizer) return;
  window.__agentosCustomizer = true;
  var enabled = false;
  var box = null;
  var cursorTag = null;

  function parentWindow() {
    try {
      return window.parent && window.parent !== window ? window.parent : null;
    } catch (error) {
      return null;
    }
  }

  function post(message) {
    var target = parentWindow();
    if (!target) return;
    try {
      target.postMessage(message, "*");
    } catch (error) {}
  }

  function ensureBox() {
    if (box) return box;
    box = document.createElement("div");
    box.setAttribute("data-agentos-customizer", "box");
    box.style.cssText = "position:fixed;pointer-events:none;z-index:2147483646;border:2px solid #10b981;background:rgba(16,185,129,0.12);border-radius:6px;display:none;box-sizing:border-box;";
    (document.body || document.documentElement).appendChild(box);
    return box;
  }

  function place(el) {
    var target = ensureBox();
    var r = el.getBoundingClientRect();
    target.style.left = r.left + "px";
    target.style.top = r.top + "px";
    target.style.width = r.width + "px";
    target.style.height = r.height + "px";
    target.style.display = "block";
  }

  function hide() {
    if (box) box.style.display = "none";
  }

  function selectorFor(el) {
    var parts = [];
    var node = el;
    while (node && node.nodeType === 1 && node !== document.documentElement) {
      var part = node.tagName.toLowerCase();
      if (node.id) {
        parts.unshift(part + "#" + node.id);
        break;
      }
      var classes = (node.getAttribute("class") || "").split(/\s+/).filter(Boolean).slice(0, 3);
      if (classes.length) part += "." + classes.join(".");
      var parent = node.parentElement;
      if (parent) {
        var same = Array.prototype.filter.call(parent.children, function (child) {
          return child.tagName === node.tagName;
        });
        if (same.length > 1) part += ":nth-of-type(" + (same.indexOf(node) + 1) + ")";
      }
      parts.unshift(part);
      node = node.parentElement;
    }
    return parts.join(" > ");
  }

  function onMove(event) {
    if (!enabled) return;
    var el = event.target;
    if (!el || el.nodeType !== 1 || el === box) return;
    place(el);
  }

  function onClick(event) {
    if (!enabled) return;
    var el = event.target;
    if (!el || el.nodeType !== 1 || el === box) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.stopImmediatePropagation) event.stopImmediatePropagation();
    place(el);
    var text = (el.textContent || "").replace(/\s+/g, " ").trim().slice(0, 300);
    post({
      t: "agentos:pick",
      pick: {
        selector: selectorFor(el).slice(0, 600),
        tag: el.tagName.toLowerCase(),
        id: el.id || "",
        classes: (el.getAttribute("class") || "").trim().slice(0, 300),
        text: text,
        html: el.outerHTML.slice(0, 4000)
      }
    });
  }

  function onKey(event) {
    if (event.key === "Escape") {
      stop();
      post({ t: "agentos:off" });
    }
  }

  function start() {
    if (enabled) return;
    enabled = true;
    cursorTag = document.createElement("style");
    cursorTag.setAttribute("data-agentos-customizer", "cursor");
    cursorTag.textContent = "html, html * { cursor: crosshair !important; }";
    (document.head || document.documentElement).appendChild(cursorTag);
    document.addEventListener("click", onClick, true);
    document.addEventListener("mousemove", onMove, true);
    document.addEventListener("keydown", onKey, true);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("resize", hide, true);
    post({ t: "agentos:ready" });
  }

  function stop() {
    if (!enabled) return;
    enabled = false;
    if (cursorTag && cursorTag.parentNode) cursorTag.parentNode.removeChild(cursorTag);
    cursorTag = null;
    document.removeEventListener("click", onClick, true);
    document.removeEventListener("mousemove", onMove, true);
    document.removeEventListener("keydown", onKey, true);
    window.removeEventListener("scroll", hide, true);
    window.removeEventListener("resize", hide, true);
    hide();
  }

  window.addEventListener("message", function (event) {
    var source = parentWindow();
    if (!source || event.source !== source) return;
    var data = event.data || {};
    if (data.t === "agentos:enable") start();
    if (data.t === "agentos:off") stop();
  });
})();
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "index.html".to_string(),
                "<h1>Hello</h1>\n<button class=\"primary\">Go</button>\n".to_string(),
            ),
            (
                "styles.css".to_string(),
                ".primary { color: #fff; }\n".to_string(),
            ),
        ])
    }

    fn existing() -> BTreeSet<String> {
        files().keys().cloned().collect()
    }

    fn edit(path: &str, old: &str, new: &str) -> Edit {
        Edit {
            path: path.into(),
            old: old.into(),
            new: new.into(),
            why: String::new(),
            line: 0,
        }
    }

    #[test]
    fn a_plan_maps_edits_to_files_and_lines() {
        let raw = r##"Here is the plan:
```json
{"summary":"Make the button green","files":[
  {"path":"styles.css","old":".primary { color: #fff; }","new":".primary { color: #fff; background: #067a56; }","why":"stronger action"}
]}
```"##;
        let (summary, edits) = validate_plan(raw, &files(), &existing()).unwrap();
        assert_eq!(summary, "Make the button green");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].path, "styles.css");
        assert_eq!(edits[0].line, 1);
        assert_eq!(edits[0].why, "stronger action");
    }

    #[test]
    fn stale_or_ambiguous_text_is_refused() {
        let stale = validate_plan(
            r#"{"summary":"x","files":[{"path":"styles.css","old":".gone { }","new":".gone { color: red }"}]}"#,
            &files(),
            &existing(),
        )
        .unwrap_err()
        .to_string();
        assert!(stale.contains("not found"), "{stale}");

        let mut doubled = files();
        doubled.insert("app.js".into(), "x = 1;\nx = 1;\n".into());
        let ambiguous = validate_plan(
            r#"{"summary":"x","files":[{"path":"app.js","old":"x = 1;","new":"x = 2;"}]}"#,
            &doubled,
            &existing(),
        )
        .unwrap_err()
        .to_string();
        assert!(ambiguous.contains("2 times"), "{ambiguous}");
    }

    #[test]
    fn generated_files_and_escape_paths_are_refused() {
        let e = normalize_path("agentos.js").unwrap_err().to_string();
        assert!(e.contains("generated"), "{e}");
        assert!(normalize_path("../secret.txt").is_err());
        assert!(normalize_path("images\\x.png").is_err());
        assert!(normalize_path("./styles.css").is_ok());
    }

    #[test]
    fn rejects_placeholders_dashes_and_local_urls_in_the_replacement() {
        for (bad, needle) in [
            ("# TODO", "TODO marker"),
            ("one \u{2014} two", "em dash"),
            ("see http://localhost:3000", "localhost"),
        ] {
            let plan = format!(
                r#"{{"summary":"x","files":[{{"path":"styles.css","old":".primary {{ color: #fff; }}","new":".primary {{ color: red; }} {}"}}]}}"#,
                bad
            );
            let e = validate_plan(&plan, &files(), &existing())
                .unwrap_err()
                .to_string();
            assert!(e.contains(needle), "{bad}: {e}");
        }
    }

    #[test]
    fn a_plan_can_create_a_file_then_edit_it() {
        let edits = vec![
            edit("about.html", "", "<p>About</p>\n"),
            edit("about.html", "About", "About us"),
        ];
        let checked = validate_edits(&edits, &files(), &existing()).unwrap();
        assert_eq!(checked[0].line, 1);
        assert_eq!(checked[1].line, 1);
        assert!(validate_edits(&[edit("index.html", "", "x")], &files(), &existing()).is_err());
    }

    #[test]
    fn the_prompt_carries_the_selection_files_and_schema() {
        let sel = Selection {
            selector: "body > main > button.primary".into(),
            tag: "button".into(),
            id: String::new(),
            classes: "primary".into(),
            text: "Go".into(),
            html: "<button class=\"primary\">Go</button>".into(),
        };
        let p = plan_prompt("Probe", "probe", &sel, "Make it green", &files());
        assert!(p.contains("body > main > button.primary"));
        assert!(p.contains("Make it green"));
        assert!(p.contains("===== FILE index.html ====="));
        assert!(p.contains("\"summary\""));
        assert!(p.contains("Never edit agentos.js"));
        assert!(!p.contains('\u{2014}'));
        assert!(!p.contains('\u{2013}'));
    }

    #[test]
    fn the_selection_is_bounded() {
        let big = Selection {
            selector: "x".repeat(2_000),
            html: "y".repeat(20_000),
            ..Default::default()
        };
        let b = big.bounded();
        assert_eq!(b.selector.chars().count(), 600);
        assert_eq!(b.html.chars().count(), 4_000);
    }

    #[test]
    fn the_injected_script_is_self_contained_and_guarded() {
        assert!(CUSTOMIZER_JS.contains("agentos:pick"));
        assert!(CUSTOMIZER_JS.contains("event.source !== source"));
        assert!(CUSTOMIZER_JS.contains("stopPropagation"));
        assert!(!CUSTOMIZER_JS.contains('\u{2014}'));
        assert!(!CUSTOMIZER_JS.contains('\u{2013}'));
    }

    #[test]
    fn every_installed_agent_is_listed_with_a_usable_default() {
        let list = agents();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].id, "claude-code");
        if list.iter().any(|a| a.installed) {
            assert!(default_agent().is_ok());
        }
    }

    #[test]
    fn an_applied_plan_rechecks_the_disk_and_writes_once() {
        let dir = tempfile::tempdir().unwrap();
        artifacts::set_test_root(Some(dir.path().join("artifacts")));
        let c = crate::store::test_conn();
        let a = artifacts::create_in(&c, "Demo", "A page", "blank").unwrap();

        apply_in(
            &c,
            &a.id,
            &[edit(
                "styles.css",
                ".primary:hover { background: #056348; }",
                ".primary:hover { background: #0a7f5c; }",
            )],
        )
        .unwrap();
        let css = artifacts::read_file_in(&c, &a.id, "styles.css").unwrap();
        assert!(css.contains("#0a7f5c"), "{css}");

        let stale = apply_in(&c, &a.id, &[edit("styles.css", "#056348", "#111111")])
            .unwrap_err()
            .to_string();
        assert!(stale.contains("styles.css"), "{stale}");
        let css = artifacts::read_file_in(&c, &a.id, "styles.css").unwrap();
        assert!(!css.contains("#111111"), "a refused plan writes nothing");

        apply_in(&c, &a.id, &[edit("notes/plan.md", "", "# Plan\n")]).unwrap();
        let plan = artifacts::read_file_in(&c, &a.id, "notes/plan.md").unwrap();
        assert_eq!(plan, "# Plan\n");

        artifacts::set_test_root(None);
    }
}
