//! Hermes skills, surfaced in the app.
//!
//! A skill is a `SKILL.md` file with YAML frontmatter and optional reference files, living under
//! Hermes's skills directory (`<hermes home>/skills/<category>/<name>/SKILL.md`). Studio reads
//! that tree directly for the list and the body, and uses `hermes skills ...` for anything that
//! writes (install, update, uninstall), so the terminal and the app stay in agreement.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use serde_yaml::Value as Yaml;

use crate::procs;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub category: String,
    pub version: Option<String>,
    pub tags: Vec<String>,
    pub path: String,
    pub bytes: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct HubSkill {
    pub name: String,
    pub identifier: String,
    pub source: String,
    pub trust_level: String,
    pub description: String,
}

pub fn skills_root() -> PathBuf {
    procs::hermes_home().join("skills")
}

/// The YAML block at the top of a Markdown file, if it has one.
pub fn frontmatter(text: &str) -> Option<Yaml> {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = t.strip_prefix("---")?;
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    let end = rest.find("\n---")?;
    serde_yaml::from_str(&rest[..end]).ok()
}

fn ystr(v: Option<&Yaml>) -> Option<String> {
    match v? {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Number(n) => Some(n.to_string()),
        Yaml::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn ylist(v: Option<&Yaml>) -> Vec<String> {
    v.and_then(Yaml::as_sequence)
        .map(|s| s.iter().filter_map(|x| ystr(Some(x))).collect())
        .unwrap_or_default()
}

/// Parse one `SKILL.md` into a `Skill`. `category` comes from the folder layout.
pub fn parse_skill(path: &Path, category: &str) -> Option<Skill> {
    let text = fs::read_to_string(path).ok()?;
    let fm = frontmatter(&text)?;
    let name = ystr(fm.get("name")).or_else(|| {
        path.parent()?
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
    })?;
    let mut tags = ylist(fm.get("tags"));
    if let Some(meta) = fm.get("metadata").and_then(|m| m.get("hermes")) {
        let extra = ylist(meta.get("tags"));
        for t in extra {
            if !tags.contains(&t) {
                tags.push(t);
            }
        }
    }
    Some(Skill {
        name,
        description: ystr(fm.get("description")).unwrap_or_default(),
        category: category.to_string(),
        version: ystr(fm.get("version")),
        tags,
        path: path.display().to_string(),
        bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    })
}

/// Every skill under `root`, one level of category folders deep (and one more for safety).
pub fn scan(root: &Path) -> Vec<Skill> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    for entry in walkdir::WalkDir::new(root)
        .min_depth(1)
        .max_depth(3)
        .into_iter()
        .flatten()
    {
        if !entry.file_type().is_file() || entry.file_name() != "SKILL.md" {
            continue;
        }
        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
        let category = rel
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_else(|| "uncategorised".into());
        let category = if category == "SKILL.md" {
            "uncategorised".to_string()
        } else {
            category
        };
        if let Some(s) = parse_skill(entry.path(), &category) {
            out.push(s);
        }
    }
    out.sort_by(|a, b| a.category.cmp(&b.category).then(a.name.cmp(&b.name)));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

pub fn list() -> Vec<Skill> {
    scan(&skills_root())
}

/// The full `SKILL.md` for one installed skill, capped so a huge file cannot freeze the UI.
pub fn read(name: &str) -> Result<String> {
    let all = list();
    let skill = all
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow::anyhow!("No skill named '{name}'"))?;
    let text = fs::read_to_string(&skill.path).with_context(|| skill.path.clone())?;
    if text.len() > 400_000 {
        let cut = text.len() - 400_000;
        return Ok(format!(
            "{}\n\n... truncated {cut} characters",
            &text[..400_000]
        ));
    }
    Ok(text)
}

fn cli(args: &[&str], secs: u64) -> Result<String> {
    let (code, out, err) = procs::run_with_timeout(
        "hermes",
        args,
        Some("y\ny\n"),
        None,
        std::time::Duration::from_secs(secs),
    )?;
    let text = format!("{out}\n{err}")
        .lines()
        .map(procs::clean_line)
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if code != Some(0) {
        bail!(
            "hermes {} failed: {text}",
            args.first().copied().unwrap_or("")
        );
    }
    Ok(text)
}

/// Search the skill registries. Hermes returns a JSON array for `--json`.
pub fn search_hub(query: &str, limit: usize) -> Result<Vec<HubSkill>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.clamp(1, 50).to_string();
    let text = cli(
        &[
            "skills",
            "search",
            query.trim(),
            "--json",
            "--limit",
            &limit,
        ],
        90,
    )?;
    let start = text
        .find('[')
        .ok_or_else(|| anyhow::anyhow!("No results for '{query}'"))?;
    let end = text
        .rfind(']')
        .ok_or_else(|| anyhow::anyhow!("Malformed search output"))?;
    let v: Value = serde_json::from_str(&text[start..=end])
        .with_context(|| "parsing hermes skills search output")?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .map(|s| HubSkill {
                    name: s
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("skill")
                        .into(),
                    identifier: s
                        .get("identifier")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    source: s
                        .get("source")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    trust_level: s
                        .get("trust_level")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    description: s
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn install(identifier: &str) -> Result<String> {
    if identifier.trim().is_empty() {
        bail!("Give a skill identifier");
    }
    let before = list().len();
    let out = cli(&["skills", "install", identifier.trim(), "--yes"], 300)?;
    let after = list().len();
    Ok(format!(
        "{} (installed skills: {before} to {after})",
        out.lines().last().unwrap_or("installed").trim()
    ))
}

pub fn remove(name: &str) -> Result<String> {
    let out = cli(&["skills", "uninstall", name, "--yes"], 120)?;
    Ok(out.lines().last().unwrap_or("removed").trim().to_string())
}

pub fn update_all() -> Result<String> {
    let out = cli(&["skills", "update"], 600)?;
    Ok(out.lines().last().unwrap_or("updated").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn reads_frontmatter_and_ignores_body_separators() {
        let text = "---\nname: demo\ndescription: Does a thing\nversion: 1.2.0\ntags: [a, b]\nmetadata:\n  hermes:\n    tags: [c]\n---\n\n# Demo\n\n---\nnot frontmatter\n";
        let fm = frontmatter(text).unwrap();
        assert_eq!(ystr(fm.get("name")).unwrap(), "demo");
        assert_eq!(ystr(fm.get("version")).unwrap(), "1.2.0");
        let mut tags = ylist(fm.get("tags"));
        tags.extend(ylist(
            fm.get("metadata")
                .and_then(|m| m.get("hermes"))
                .and_then(|h| h.get("tags")),
        ));
        assert_eq!(tags, vec!["a", "b", "c"]);
        assert!(frontmatter("no frontmatter here").is_none());
        assert!(frontmatter("---\nnot: closed").is_none());
    }

    #[test]
    fn scans_categories_and_skips_reference_files() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "dev/alpha/SKILL.md",
            "---\nname: alpha\ndescription: first\n---\nbody",
        );
        write(d.path(), "dev/alpha/references/api.md", "# not a skill");
        write(
            d.path(),
            "creative/beta/SKILL.md",
            "---\nname: beta\n---\nbody",
        );
        write(d.path(), "loose/SKILL.md", "---\nname: loose\n---\nbody");
        let skills = scan(d.path());
        let mut names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta", "loose"], "sorted by category then name");
        let alpha = skills.iter().find(|s| s.name == "alpha").unwrap();
        assert_eq!(alpha.category, "dev");
        assert_eq!(alpha.description, "first");
        let loose = skills.iter().find(|s| s.name == "loose").unwrap();
        assert_eq!(loose.category, "loose");
    }

    #[test]
    fn missing_root_scans_empty() {
        assert!(scan(Path::new("C:/definitely/not/here")).is_empty());
    }

    #[test]
    fn parses_hub_search_payload() {
        let payload = r#"[
          {"name":"excel-author","identifier":"official/finance/excel-author","source":"official","trust_level":"builtin","description":"Build workbooks."}
        ]"#;
        let start = payload.find('[').unwrap();
        let v: Value = serde_json::from_str(&payload[start..]).unwrap();
        let first = &v.as_array().unwrap()[0];
        assert_eq!(first["identifier"], "official/finance/excel-author");
    }
}
