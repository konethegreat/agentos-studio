//! Update detection and installation for Hermes, Claude Code and AgentOS Studio.
//!
//! Detection: `hermes update --check` plus the Hermes GitHub releases for notes, the npm
//! registry plus `CHANGELOG.md` for Claude Code, and the Studio GitHub releases. Results
//! carry a readable changelog summary. Skip and snooze choices live in `studio.db`.
//!
//! Installation always backs up first and records a manifest, so a bad update can be
//! rolled back from the Update Center:
//! - Hermes: a Hermes quick state snapshot (`hermes backup --quick`) plus the exact code
//!   commit; then `hermes update --yes`. Rollback pins the code back to that commit.
//! - Claude Code: the previous version is recorded; rollback runs `claude install <ver>`.
//! - Studio: the release installer is downloaded, its SHA-256 checked against the GitHub
//!   digest, then run. Rollback installs the previous release the same way.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};

use crate::{permissions, procs, store};

pub const EV_UPDATES: &str = "agentos://updates";
pub const EV_UPDATE_PROGRESS: &str = "agentos://update-progress";

pub const STUDIO_REPO: &str = "konethegreat/agentos-studio";
pub const HERMES_REPO: &str = "NousResearch/hermes-agent";
const CLAUDE_NPM: &str = "https://registry.npmjs.org/@anthropic-ai/claude-code/latest";
const CLAUDE_CHANGELOG: &str =
    "https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md";

pub const PRODUCTS: [&str; 3] = ["hermes", "claude-code", "studio"];

pub fn label(product: &str) -> &'static str {
    match product {
        "hermes" => "Hermes Agent",
        "claude-code" => "Claude Code",
        _ => "AgentOS Studio",
    }
}

// ---------------------------------------------------------------- versions

/// Numeric components of a version token such as `v0.23.0`, `2.1.318` or `0.3.0-rc.1`.
pub fn parse_version(s: &str) -> Option<Vec<u64>> {
    let t = s.trim().trim_start_matches(['v', 'V']);
    let core = t.split(['+', '-', ' ']).next()?;
    let parts: Option<Vec<u64>> = core.split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|p| !p.is_empty())
}

pub fn newer(a: &str, b: &str) -> bool {
    match (parse_version(a), parse_version(b)) {
        (Some(mut x), Some(mut y)) => {
            let n = x.len().max(y.len());
            x.resize(n, 0);
            y.resize(n, 0);
            x > y
        }
        _ => false,
    }
}

/// First `x.y.z` token in a `--version` line.
pub fn version_token(line: &str) -> Option<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"v?(\d+\.\d+\.\d+)").expect("static regex"));
    re.captures(line).map(|c| c[1].to_string())
}

// ---------------------------------------------------------------- changelog

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ChangeGroup {
    pub title: String,
    pub items: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Changelog {
    pub versions: Vec<String>,
    pub highlights: Vec<String>,
    pub groups: Vec<ChangeGroup>,
    pub url: String,
}

/// Plain text from one Markdown line: no emphasis, code ticks, links, HTML comments or
/// typographic dashes, capped for display.
pub fn clean_md(s: &str) -> String {
    static LINK: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static TAGS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let link =
        LINK.get_or_init(|| regex::Regex::new(r"\[([^\]]*)\]\([^)]*\)").expect("static regex"));
    let tags = TAGS.get_or_init(|| regex::Regex::new(r"<[^>]+>").expect("static regex"));
    let t = link.replace_all(s, "$1");
    let t = tags.replace_all(&t, "");
    let t = t
        .replace("**", "")
        .replace("__", "")
        .replace('`', "")
        .replace(" \u{2014} ", " - ")
        .replace(['\u{2014}', '\u{2013}'], "-");
    let t = t.trim().trim_start_matches(['-', '*', ' ']).trim();
    if t.chars().count() > 260 {
        format!("{}...", t.chars().take(257).collect::<String>())
    } else {
        t.to_string()
    }
}

fn heading_title(h: &str) -> String {
    clean_md(h.trim_start_matches('#'))
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string()
}

/// Summarise release-note Markdown: a "Highlights" section if there is one, otherwise the
/// first bullets; plus bullets grouped under their headings.
pub fn summarize_markdown(
    body: &str,
    max_groups: usize,
    max_items: usize,
) -> (Vec<String>, Vec<ChangeGroup>) {
    let mut groups: Vec<ChangeGroup> = Vec::new();
    let mut highlights = Vec::new();
    let mut heading = String::from("Changes");
    let mut in_comment = false;
    for raw in body.lines() {
        let line = raw.trim();
        if line.starts_with("<!--") {
            in_comment = !line.contains("-->");
            continue;
        }
        if in_comment {
            in_comment = !line.contains("-->");
            continue;
        }
        if line.starts_with("## ") || line.starts_with("### ") {
            heading = heading_title(line);
            continue;
        }
        let is_bullet = line.starts_with("- ") || line.starts_with("* ");
        if !is_bullet {
            continue;
        }
        let item = clean_md(line);
        if item.is_empty() {
            continue;
        }
        if heading.to_lowercase().contains("highlight") {
            if highlights.len() < max_items {
                highlights.push(item);
            }
            continue;
        }
        let count = groups.len();
        match groups.iter_mut().find(|g| g.title == heading) {
            Some(g) => {
                if g.items.len() < max_items {
                    g.items.push(item)
                }
            }
            None if count < max_groups => groups.push(ChangeGroup {
                title: heading.clone(),
                items: vec![item],
            }),
            None => {}
        }
    }
    if highlights.is_empty() {
        highlights = groups
            .iter()
            .flat_map(|g| g.items.iter().cloned())
            .take(4)
            .collect();
    }
    (highlights, groups)
}

/// Claude Code's CHANGELOG: `## <version>` sections of plain bullets. Collect every
/// version newer than `current` up to `latest`, grouped by the bullet's leading verb.
pub fn claude_changelog(md: &str, current: &str, latest: &str) -> Changelog {
    let mut versions = Vec::new();
    let mut added = Vec::new();
    let mut fixed = Vec::new();
    let mut changed = Vec::new();
    let mut take = false;
    for line in md.lines() {
        if let Some(v) = line.strip_prefix("## ") {
            let v = v.trim();
            take = newer(v, current) && !newer(v, latest);
            if take {
                versions.push(v.to_string());
            }
            continue;
        }
        if !take || !line.starts_with("- ") {
            continue;
        }
        let item = clean_md(line);
        let lower = item.to_lowercase();
        let bucket = if lower.starts_with("added") || lower.starts_with("new") {
            &mut added
        } else if lower.starts_with("fixed") || lower.starts_with("fix") {
            &mut fixed
        } else {
            &mut changed
        };
        bucket.push(item);
    }
    let mut highlights: Vec<String> = Vec::new();
    if !versions.is_empty() {
        highlights.push(format!(
            "{} new, {} fixed, {} changed across {} release{}",
            added.len(),
            fixed.len(),
            changed.len(),
            versions.len(),
            if versions.len() == 1 { "" } else { "s" }
        ));
    }
    highlights.extend(added.iter().take(3).cloned());
    let cap = |v: Vec<String>| v.into_iter().take(12).collect::<Vec<_>>();
    let groups = [("New", added), ("Fixed", fixed), ("Changed", changed)]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(t, v)| ChangeGroup {
            title: t.into(),
            items: cap(v),
        })
        .collect();
    Changelog {
        versions,
        highlights,
        groups,
        url: "https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md".into(),
    }
}

// ---------------------------------------------------------------- HTTP

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into()
}

fn user_agent() -> String {
    format!("agentos-studio/{}", env!("CARGO_PKG_VERSION"))
}

pub fn http_text(url: &str) -> Result<String> {
    let mut res = agent()
        .get(url)
        .header("User-Agent", &user_agent())
        .header(
            "Accept",
            "application/vnd.github+json, application/json, text/plain",
        )
        .call()
        .with_context(|| format!("GET {url}"))?;
    res.body_mut()
        .with_config()
        .limit(20 * 1024 * 1024)
        .read_to_string()
        .with_context(|| format!("reading {url}"))
}

fn http_bytes(url: &str) -> Result<Vec<u8>> {
    let mut res = agent()
        .get(url)
        .header("User-Agent", &user_agent())
        .header("Accept", "application/octet-stream")
        .call()
        .with_context(|| format!("GET {url}"))?;
    res.body_mut()
        .with_config()
        .limit(512 * 1024 * 1024)
        .read_to_vec()
        .with_context(|| format!("downloading {url}"))
}

#[derive(Deserialize, Clone, Debug)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub size: u64,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

pub fn releases(repo: &str) -> Result<Vec<Release>> {
    let text = http_text(&format!(
        "https://api.github.com/repos/{repo}/releases?per_page=20"
    ))?;
    let mut list: Vec<Release> = serde_json::from_str(&text).context("parsing releases")?;
    list.retain(|r| !r.draft && !r.prerelease && parse_version(&r.tag_name).is_some());
    list.sort_by(|a, b| {
        if newer(&a.tag_name, &b.tag_name) {
            std::cmp::Ordering::Less
        } else if newer(&b.tag_name, &a.tag_name) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    Ok(list)
}

/// Notes for every release newer than `current` (newest first, at most three bodies).
pub fn releases_changelog(list: &[Release], current: &str) -> Changelog {
    let newer_ones: Vec<&Release> = list
        .iter()
        .filter(|r| newer(&r.tag_name, current))
        .collect();
    let mut highlights = Vec::new();
    let mut groups: Vec<ChangeGroup> = Vec::new();
    for r in newer_ones.iter().take(3) {
        let (h, g) = summarize_markdown(r.body.as_deref().unwrap_or(""), 6, 6);
        if highlights.len() < 6 {
            highlights.extend(h.into_iter().take(6 - highlights.len()));
        }
        for grp in g {
            if groups.len() < 8 {
                groups.push(ChangeGroup {
                    title: format!("{} - {}", r.tag_name, grp.title),
                    items: grp.items,
                });
            }
        }
    }
    Changelog {
        versions: newer_ones.iter().map(|r| r.tag_name.clone()).collect(),
        highlights,
        groups,
        url: newer_ones
            .first()
            .map(|r| r.html_url.clone())
            .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------- status

#[derive(Serialize, Clone, Debug, Default)]
pub struct ProductUpdate {
    pub product: String,
    pub label: String,
    pub installed: bool,
    pub current: Option<String>,
    pub latest: Option<String>,
    /// `up_to_date`, `available`, `unknown` or `not_installed`.
    pub state: String,
    pub detail: String,
    pub changelog: Option<Changelog>,
    pub skipped: bool,
    pub snoozed_until: Option<i64>,
    pub alert: bool,
    pub can_update: bool,
    pub update_note: String,
    pub checked_at: i64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct UpdateReport {
    pub products: Vec<ProductUpdate>,
    pub checked_at: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HermesVerdict {
    UpToDate,
    Available(String),
    Failed(String),
    Unknown,
}

/// Parse `hermes update --check`. A failed channel lookup still exits 0, so the text decides.
pub fn parse_hermes_check(text: &str) -> HermesVerdict {
    let mut failed = None;
    for line in text
        .lines()
        .map(|l| procs::clean_line(l).trim().to_string())
    {
        let lower = line.to_lowercase();
        if lower.contains("update available") {
            return HermesVerdict::Available(
                line.trim_start_matches(['\u{2192}', '\u{2713}', ' '])
                    .to_string(),
            );
        }
        if lower.contains("up to date") {
            return HermesVerdict::UpToDate;
        }
        if line.starts_with('\u{2717}') && failed.is_none() {
            failed = Some(line.trim_start_matches(['\u{2717}', ' ']).to_string());
        }
    }
    failed
        .map(HermesVerdict::Failed)
        .unwrap_or(HermesVerdict::Unknown)
}

fn base(product: &str) -> ProductUpdate {
    ProductUpdate {
        product: product.into(),
        label: label(product).into(),
        state: "unknown".into(),
        checked_at: store::now(),
        ..Default::default()
    }
}

/// `(install directory, version line)` from `hermes --version`.
pub fn hermes_install() -> Option<(Option<PathBuf>, String)> {
    let out = procs::run("hermes", &["--version"])?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())?
        .trim()
        .to_string();
    let dir = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("Install directory:"))
        .map(|d| PathBuf::from(d.trim()))
        .filter(|p| p.is_dir());
    Some((dir, first))
}

pub fn check_hermes() -> ProductUpdate {
    let mut p = base("hermes");
    let Some((_, line)) = hermes_install() else {
        p.state = "not_installed".into();
        p.detail = "hermes was not found on PATH".into();
        return p;
    };
    p.installed = true;
    let current = version_token(&line);
    p.current = current.clone();
    p.can_update = true;
    p.update_note = "Runs hermes update --yes. The gateway restarts, so running agents stop. A Hermes state snapshot and the current code commit are saved first.".into();
    let verdict = match procs::run_with_timeout(
        "hermes",
        &["update", "--check"],
        None,
        None,
        Duration::from_secs(90),
    ) {
        Ok((_, out, err)) => parse_hermes_check(&format!("{out}\n{err}")),
        Err(e) => HermesVerdict::Failed(format!("{e:#}")),
    };
    let rel = releases(HERMES_REPO);
    let cur = current.clone().unwrap_or_default();
    let newest = rel.as_ref().ok().and_then(|l| {
        l.first()
            .map(|r| r.tag_name.trim_start_matches('v').to_string())
    });
    match verdict {
        HermesVerdict::UpToDate => {
            p.state = "up_to_date".into();
            p.latest = current.clone();
            p.detail = "Up to date with your update channel".into();
        }
        HermesVerdict::Available(detail) => {
            p.state = "available".into();
            p.detail = detail;
            p.latest = newest
                .clone()
                .filter(|n| newer(n, &cur))
                .or_else(|| Some(format!("{cur}+")));
        }
        HermesVerdict::Failed(why) => {
            if let Some(n) = newest.clone().filter(|n| newer(n, &cur)) {
                p.state = "available".into();
                p.latest = Some(n);
            }
            p.detail = format!("Could not check with hermes update --check: {why}");
        }
        HermesVerdict::Unknown => {
            p.detail = "hermes update --check gave no verdict".into();
        }
    }
    if p.state == "available" {
        if let Ok(list) = &rel {
            let cl = releases_changelog(list, &cur);
            if !cl.versions.is_empty() {
                p.changelog = Some(cl);
            }
        }
    }
    if let Err(e) = rel {
        if p.state != "up_to_date" {
            p.detail = format!("{} (release notes unavailable: {e:#})", p.detail);
        }
    }
    p
}

pub fn check_claude() -> ProductUpdate {
    let mut p = base("claude-code");
    let Some(line) = procs::version("claude") else {
        p.state = "not_installed".into();
        p.detail = "claude was not found on PATH".into();
        return p;
    };
    p.installed = true;
    p.current = version_token(&line);
    p.can_update = true;
    p.update_note = "Runs claude update. Package-manager installs update through their package manager. The current version is recorded so you can roll back with claude install <version>.".into();
    let cur = p.current.clone().unwrap_or_default();
    let latest = http_text(CLAUDE_NPM).and_then(|t| {
        let v: Value = serde_json::from_str(&t)?;
        v.get("version")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("no version in the npm registry reply"))
    });
    match latest {
        Ok(l) => {
            p.latest = Some(l.clone());
            if newer(&l, &cur) {
                p.state = "available".into();
                p.detail = format!("{cur} -> {l}");
                if let Ok(md) = http_text(CLAUDE_CHANGELOG) {
                    p.changelog = Some(claude_changelog(&md, &cur, &l));
                }
            } else {
                p.state = "up_to_date".into();
                p.detail = "Up to date".into();
            }
        }
        Err(e) => p.detail = format!("Could not reach the npm registry: {e:#}"),
    }
    p
}

/// True when this executable runs out of a Cargo `target` directory (a source build).
pub fn is_source_build(exe: &Path) -> bool {
    let s = exe.display().to_string().replace('\\', "/").to_lowercase();
    s.contains("/target/release/") || s.contains("/target/debug/")
}

pub fn check_studio() -> ProductUpdate {
    let mut p = base("studio");
    p.installed = true;
    let cur = env!("CARGO_PKG_VERSION").to_string();
    p.current = Some(cur.clone());
    let exe = std::env::current_exe().unwrap_or_default();
    if is_source_build(&exe) {
        p.update_note = "This copy runs from a source build. Update with git pull and npm run tauri build, so the agentos-hub path registered in your agents stays the same.".into();
    } else {
        p.can_update = true;
        p.update_note = "Downloads the installer from GitHub, checks its SHA-256 digest, then runs it. Studio closes while it installs.".into();
    }
    match releases(STUDIO_REPO) {
        Ok(list) => {
            let latest = list
                .first()
                .map(|r| r.tag_name.trim_start_matches('v').to_string());
            p.latest = latest.clone();
            if latest.as_deref().is_some_and(|l| newer(l, &cur)) {
                p.state = "available".into();
                p.detail = format!("{cur} -> {}", latest.unwrap_or_default());
                p.changelog = Some(releases_changelog(&list, &cur));
            } else {
                p.state = "up_to_date".into();
                p.detail = "Up to date".into();
            }
        }
        Err(e) => p.detail = format!("Could not reach GitHub: {e:#}"),
    }
    p
}

fn decorate(c: &rusqlite::Connection, p: &mut ProductUpdate) {
    let skip = store::kv_get(c, &format!("update.skip.{}", p.product))
        .ok()
        .flatten();
    let snooze = store::kv_get(c, &format!("update.snooze.{}", p.product))
        .ok()
        .flatten()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|t| *t > store::now());
    p.skipped = skip.is_some() && skip == p.latest;
    p.snoozed_until = snooze;
    p.alert = p.state == "available" && !p.skipped && snooze.is_none();
}

pub fn check_all() -> UpdateReport {
    let handles = [
        std::thread::spawn(check_hermes),
        std::thread::spawn(check_claude),
        std::thread::spawn(check_studio),
    ];
    let mut products: Vec<ProductUpdate> = handles
        .into_iter()
        .zip(PRODUCTS)
        .map(|(h, name)| {
            h.join().unwrap_or_else(|_| {
                let mut p = base(name);
                p.detail = "The check crashed".into();
                p
            })
        })
        .collect();
    if let Ok(c) = store::open() {
        for p in products.iter_mut() {
            decorate(&c, p);
        }
        if let Ok(json) = serde_json::to_string(&products) {
            let _ = store::kv_set(&c, "update.last_report", &json);
        }
    }
    UpdateReport {
        products,
        checked_at: store::now(),
    }
}

/// Products that should raise an OS notification now (alerting and not yet notified for
/// this version). Marks them as notified.
pub fn take_notifications(report: &UpdateReport) -> Vec<ProductUpdate> {
    let Ok(c) = store::open() else { return vec![] };
    let mut out = Vec::new();
    for p in report.products.iter().filter(|p| p.alert) {
        let key = format!("update.notified.{}", p.product);
        let latest = p.latest.clone().unwrap_or_default();
        if store::kv_get(&c, &key).ok().flatten().as_deref() != Some(latest.as_str()) {
            let _ = store::kv_set(&c, &key, &latest);
            out.push(p.clone());
        }
    }
    out
}

pub fn notification_body(p: &ProductUpdate) -> String {
    let head = format!(
        "{} {} is available (you have {}).",
        p.label,
        p.latest.clone().unwrap_or_default(),
        p.current.clone().unwrap_or_default()
    );
    match p.changelog.as_ref().and_then(|c| c.highlights.first()) {
        Some(h) => format!("{head} {h}"),
        None => head,
    }
}

pub fn skip_version(product: &str, version: &str) -> Result<()> {
    check_product(product)?;
    let c = store::open()?;
    store::kv_set(&c, &format!("update.skip.{product}"), version)?;
    permissions::audit_in(&c, "update.skip", product, version, "ok")
}

pub fn snooze(product: &str, hours: i64) -> Result<i64> {
    check_product(product)?;
    let until = store::now() + hours.clamp(1, 24 * 30) * 3600;
    let c = store::open()?;
    store::kv_set(&c, &format!("update.snooze.{product}"), &until.to_string())?;
    Ok(until)
}

pub fn clear_skip(product: &str) -> Result<()> {
    check_product(product)?;
    let c = store::open()?;
    store::kv_delete(&c, &format!("update.skip.{product}"))?;
    store::kv_delete(&c, &format!("update.snooze.{product}"))
}

fn check_product(product: &str) -> Result<()> {
    if PRODUCTS.contains(&product) {
        Ok(())
    } else {
        bail!("Unknown product {product}")
    }
}

// ---------------------------------------------------------------- backups

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct BackupManifest {
    pub id: String,
    pub product: String,
    pub created_at: i64,
    pub from_version: String,
    pub to_version: String,
    #[serde(default)]
    pub hermes_commit: Option<String>,
    #[serde(default)]
    pub hermes_install_dir: Option<String>,
    #[serde(default)]
    pub hermes_snapshot: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub dir: String,
}

pub fn backups_root() -> PathBuf {
    store::studio_dir().join("backups")
}

fn write_manifest(m: &mut BackupManifest) -> Result<PathBuf> {
    let dir = backups_root().join(&m.product).join(&m.id);
    std::fs::create_dir_all(&dir)?;
    m.dir = dir.display().to_string();
    std::fs::write(dir.join("manifest.json"), serde_json::to_string_pretty(m)?)?;
    Ok(dir)
}

pub fn list_backups() -> Vec<BackupManifest> {
    let mut out = Vec::new();
    for product in PRODUCTS {
        let Ok(entries) = std::fs::read_dir(backups_root().join(product)) else {
            continue;
        };
        for e in entries.flatten() {
            let f = e.path().join("manifest.json");
            if let Some(m) = std::fs::read_to_string(&f)
                .ok()
                .and_then(|t| serde_json::from_str::<BackupManifest>(&t).ok())
            {
                out.push(m);
            }
        }
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    out
}

fn find_backup(id: &str) -> Result<BackupManifest> {
    list_backups()
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| anyhow!("Backup {id} was not found"))
}

/// Parse the snapshot id from `hermes backup --quick` ("State snapshot created: <id>").
pub fn parse_snapshot_id(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("State snapshot created:"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------- runners

#[derive(Serialize, Clone, Debug)]
pub struct ProgressEvent {
    pub job: String,
    pub product: String,
    pub level: String,
    pub line: String,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

struct Progress {
    app: AppHandle,
    job: String,
    product: String,
}

impl Progress {
    fn emit(&self, level: &str, line: impl Into<String>) {
        let _ = self.app.emit(
            EV_UPDATE_PROGRESS,
            ProgressEvent {
                job: self.job.clone(),
                product: self.product.clone(),
                level: level.into(),
                line: line.into(),
                done: false,
                ok: None,
            },
        );
    }
    fn finish(&self, ok: bool, line: impl Into<String>) {
        let _ = self.app.emit(
            EV_UPDATE_PROGRESS,
            ProgressEvent {
                job: self.job.clone(),
                product: self.product.clone(),
                level: if ok { "ok" } else { "error" }.into(),
                line: line.into(),
                done: true,
                ok: Some(ok),
            },
        );
    }
}

/// Run a tool, streaming each output line; returns the exit code and the full output.
fn stream(pg: &Progress, tool: &str, args: &[&str], cwd: Option<&Path>) -> Result<(i32, String)> {
    let exe = procs::find(tool).ok_or_else(|| anyhow!("{tool} is not installed"))?;
    pg.emit("cmd", format!("$ {tool} {}", args.join(" ")));
    let mut cmd = procs::command(&exe);
    cmd.args(args)
        .env("PYTHONIOENCODING", "utf-8")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn().with_context(|| format!("starting {tool}"))?;
    let (tx, rx) = std::sync::mpsc::channel::<(bool, String)>();
    let mut threads = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        threads.push(std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send((false, l));
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        threads.push(std::thread::spawn(move || {
            for l in BufReader::new(err).lines().map_while(Result::ok) {
                let _ = tx.send((true, l));
            }
        }));
    }
    drop(tx);
    let mut all = String::new();
    for (is_err, l) in rx {
        let l = procs::clean_line(&l);
        all.push_str(&l);
        all.push('\n');
        pg.emit(if is_err { "err" } else { "out" }, l);
    }
    for t in threads {
        let _ = t.join();
    }
    let code = child.wait()?.code().unwrap_or(-1);
    Ok((code, all))
}

fn require_ok(pg: &Progress, tool: &str, args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let (code, out) = stream(pg, tool, args, cwd)?;
    if code != 0 {
        bail!("{tool} {} exited with code {code}", args.join(" "));
    }
    Ok(out)
}

fn spawn_job(
    app: AppHandle,
    product: &str,
    action: &'static str,
    target: String,
    f: impl FnOnce(&Progress) -> Result<String> + Send + 'static,
) -> String {
    let job = store::new_id();
    let pg = Progress {
        app,
        job: job.clone(),
        product: product.to_string(),
    };
    std::thread::spawn(move || match f(&pg) {
        Ok(msg) => {
            permissions::audit(action, &target, &msg, "ok");
            pg.finish(true, msg);
        }
        Err(e) => {
            permissions::audit(action, &target, &format!("{e:#}"), "error");
            pg.finish(false, format!("{e:#}"));
        }
    });
    job
}

fn studio_asset(rel: &Release) -> Option<&ReleaseAsset> {
    let want: fn(&str) -> bool = if cfg!(windows) {
        |n| n.ends_with("-setup.exe")
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            |n| n.ends_with("aarch64.dmg")
        } else {
            |n| n.ends_with("x64.dmg")
        }
    } else {
        |n| n.ends_with(".AppImage")
    };
    rel.assets.iter().find(|a| want(&a.name))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Download the Studio installer for `version`, verify it, store it in `dir` and open it.
fn install_studio_release(pg: &Progress, version: &str, dir: &Path) -> Result<PathBuf> {
    let tag = format!("v{}", version.trim_start_matches('v'));
    let text = http_text(&format!(
        "https://api.github.com/repos/{STUDIO_REPO}/releases/tags/{tag}"
    ))?;
    let rel: Release = serde_json::from_str(&text).context("parsing the release")?;
    let asset = studio_asset(&rel)
        .ok_or_else(|| anyhow!("Release {tag} has no installer for this platform"))?;
    pg.emit(
        "step",
        format!(
            "Downloading {} ({:.1} MB)",
            asset.name,
            asset.size as f64 / 1_048_576.0
        ),
    );
    let bytes = http_bytes(&asset.browser_download_url)?;
    let got = sha256_hex(&bytes);
    match asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
    {
        Some(want) if want.eq_ignore_ascii_case(&got) => {
            pg.emit("ok", format!("SHA-256 verified: {got}"))
        }
        Some(want) => bail!(
            "Checksum mismatch for {}: expected {want}, got {got}. Nothing was installed.",
            asset.name
        ),
        None => bail!(
            "GitHub did not publish a digest for {}. Refusing to run an unverified installer.",
            asset.name
        ),
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join(&asset.name);
    std::fs::write(&path, &bytes)?;
    pg.emit("step", format!("Saved {}", path.display()));
    open_installer(&path)?;
    Ok(path)
}

fn open_installer(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new(path)
            .spawn()
            .with_context(|| format!("starting {}", path.display()))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        std::process::Command::new(path).spawn()?;
    }
    Ok(())
}

fn backup_id(version: &str) -> String {
    format!(
        "{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        version.trim_start_matches('v')
    )
}

/// Back up, then install the latest version. Returns a job id; progress streams on
/// `agentos://update-progress`.
pub fn apply_update(app: AppHandle, product: &str, approved: bool) -> Result<String> {
    check_product(product)?;
    let status = match product {
        "hermes" => check_hermes(),
        "claude-code" => check_claude(),
        _ => check_studio(),
    };
    if status.state != "available" {
        bail!(
            "{} is already up to date ({})",
            status.label,
            status.current.unwrap_or_default()
        );
    }
    if !status.can_update {
        bail!("{}", status.update_note);
    }
    let from = status.current.clone().unwrap_or_default();
    let to = status.latest.clone().unwrap_or_default();
    let summary = format!(
        "Update {} from {from} to {to}. {}",
        status.label, status.update_note
    );
    permissions::guard("update.apply", product, &summary, approved)?;
    let product_s = product.to_string();
    let app2 = app.clone();
    Ok(spawn_job(
        app,
        product,
        "update.apply",
        format!("{product} {from} -> {to}"),
        move |pg| {
            let mut m = BackupManifest {
                id: backup_id(&from),
                product: product_s.clone(),
                created_at: store::now(),
                from_version: from.clone(),
                to_version: to.clone(),
                ..Default::default()
            };
            match product_s.as_str() {
                "hermes" => {
                    pg.emit("step", "Reading the update plan");
                    let _ = stream(pg, "hermes", &["update", "--plan"], None);
                    pg.emit(
                        "step",
                        "Backing up Hermes state and recording the code commit",
                    );
                    let (dir, _) =
                        hermes_install().ok_or_else(|| anyhow!("hermes is not installed"))?;
                    if let Some(d) = &dir {
                        let (_, out) = stream(pg, "git", &["rev-parse", "HEAD"], Some(d))?;
                        m.hermes_commit = out
                            .lines()
                            .next()
                            .map(|s| s.trim().to_string())
                            .filter(|s| s.len() >= 7);
                        m.hermes_install_dir = Some(d.display().to_string());
                    }
                    let label = format!("studio-pre-update-{from}");
                    let out = require_ok(
                        pg,
                        "hermes",
                        &["backup", "--quick", "--label", &label],
                        None,
                    )?;
                    m.hermes_snapshot = parse_snapshot_id(&out);
                    m.note = "Rollback pins the Hermes code to the recorded commit. Restore state with /snapshot restore <id> in a Hermes terminal.".into();
                    write_manifest(&mut m)?;
                    pg.emit("ok", format!("Backup {} saved", m.id));
                    pg.emit("step", "Installing the update (the gateway will restart)");
                    require_ok(pg, "hermes", &["update", "--yes"], None)?;
                    let now = hermes_install().map(|(_, l)| l).unwrap_or_default();
                    Ok(format!("Hermes updated: {now}"))
                }
                "claude-code" => {
                    m.note = format!("Rollback runs claude install {from} --force.");
                    write_manifest(&mut m)?;
                    pg.emit(
                        "ok",
                        format!("Backup {} saved (version {from} recorded)", m.id),
                    );
                    require_ok(pg, "claude", &["update"], None)?;
                    let now = procs::version("claude").unwrap_or_default();
                    Ok(format!("Claude Code updated: {now}"))
                }
                _ => {
                    m.note = format!("Rollback downloads and runs the v{from} installer.");
                    if let Ok(exe) = std::env::current_exe() {
                        m.files.push(exe.display().to_string());
                    }
                    let dir = write_manifest(&mut m)?;
                    pg.emit("ok", format!("Backup {} saved", m.id));
                    let path = install_studio_release(pg, &to, &dir.join("downloads"))?;
                    let app = app2.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(1500));
                        app.exit(0);
                    });
                    Ok(format!(
                        "Installer started: {}. Studio is closing so it can be replaced.",
                        path.display()
                    ))
                }
            }
        },
    ))
}

pub fn rollback(app: AppHandle, backup_id: &str, approved: bool) -> Result<String> {
    let m = find_backup(backup_id)?;
    let summary = format!(
        "Roll {} back to {} (backup {}). {}",
        label(&m.product),
        m.from_version,
        m.id,
        m.note
    );
    permissions::guard("update.rollback", &m.product, &summary, approved)?;
    let product = m.product.clone();
    let app2 = app.clone();
    Ok(spawn_job(
        app,
        &product,
        "update.rollback",
        format!("{} -> {}", m.product, m.from_version),
        move |pg| match m.product.as_str() {
            "hermes" => {
                let dir = m
                    .hermes_install_dir
                    .clone()
                    .map(PathBuf::from)
                    .ok_or_else(|| anyhow!("This backup has no install directory"))?;
                let commit = m
                    .hermes_commit
                    .clone()
                    .ok_or_else(|| anyhow!("This backup has no recorded commit"))?;
                if !commit.chars().all(|c| c.is_ascii_hexdigit()) {
                    bail!("Recorded commit is not a git hash");
                }
                let (_, status) = stream(
                    pg,
                    "git",
                    &["status", "--porcelain", "--untracked-files=no"],
                    Some(&dir),
                )?;
                if !status.trim().is_empty() {
                    bail!("The Hermes checkout has local changes. Commit or stash them first.");
                }
                require_ok(pg, "git", &["checkout", "--detach", &commit], Some(&dir))?;
                let now = hermes_install().map(|(_, l)| l).unwrap_or_default();
                let restore = m
                    .hermes_snapshot
                    .clone()
                    .map(|s| format!(" To restore Hermes state too, run /snapshot restore {s} in a Hermes terminal."))
                    .unwrap_or_default();
                Ok(format!(
                    "Hermes code pinned to {}: {now}.{restore}",
                    &commit[..commit.len().min(12)]
                ))
            }
            "claude-code" => {
                if parse_version(&m.from_version).is_none() {
                    bail!("Recorded version {} is not valid", m.from_version);
                }
                require_ok(pg, "claude", &["install", &m.from_version, "--force"], None)?;
                Ok(format!("Claude Code reinstalled at {}", m.from_version))
            }
            _ => {
                let path = install_studio_release(
                    pg,
                    &m.from_version,
                    &PathBuf::from(&m.dir).join("downloads"),
                )?;
                let app = app2.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500));
                    app.exit(0);
                });
                Ok(format!(
                    "Installer for v{} started: {}",
                    m.from_version,
                    path.display()
                ))
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("2.1.319", "2.1.318"));
        assert!(newer("v0.23.1", "0.23.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(!newer("0.23.0", "v0.23.0"));
        assert!(!newer("0.23.0+canary.1", "0.23.0"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("garbage", "0.1.0"));
        assert_eq!(
            version_token("Hermes Agent v0.23.0 (2026.9.24) - stable"),
            Some("0.23.0".into())
        );
        assert_eq!(
            version_token("2.1.318 (Claude Code)"),
            Some("2.1.318".into())
        );
    }

    #[test]
    fn parses_hermes_check_output() {
        assert_eq!(
            parse_hermes_check("\u{2192} Update channel: stable\n\u{2713} Up to date with the latest release (stable v0.23.0 (2bd378ed1b8d))."),
            HermesVerdict::UpToDate
        );
        assert!(matches!(
            parse_hermes_check("Update available: 3 commits behind origin/main."),
            HermesVerdict::Available(d) if d.contains("3 commits")
        ));
        assert!(matches!(
            parse_hermes_check("\u{2717} Could not resolve channel stable: offline"),
            HermesVerdict::Failed(d) if d.starts_with("Could not resolve")
        ));
        assert_eq!(parse_hermes_check(""), HermesVerdict::Unknown);
    }

    #[test]
    fn summarizes_release_notes() {
        let body = "# Hermes v1\n<!-- TABLE -->\n## \u{2728} Highlights\n- **Webapp** \u{2014} your [Desktop](https://x) in a tab\n- Faster `gateway`\n## Fixes\n- Fixed a crash\n- Fixed another\n";
        let (h, g) = summarize_markdown(body, 5, 5);
        assert_eq!(h, vec!["Webapp - your Desktop in a tab", "Faster gateway"]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].title, "Fixes");
        assert_eq!(g[0].items.len(), 2);
        for s in h.iter().chain(g[0].items.iter()) {
            assert!(!s.contains('\u{2014}') && !s.contains('\u{2013}'));
        }
    }

    #[test]
    fn extracts_claude_changelog_window() {
        let md = "# Changelog\n\n## 2.1.320\n- Added X\n## 2.1.319\n- Fixed Y\n- Changed Z\n- Added W\n## 2.1.318\n- Added old\n";
        let c = claude_changelog(md, "2.1.318", "2.1.319");
        assert_eq!(c.versions, vec!["2.1.319"]);
        assert_eq!(
            c.groups
                .iter()
                .map(|g| g.title.as_str())
                .collect::<Vec<_>>(),
            vec!["New", "Fixed", "Changed"]
        );
        assert!(c.highlights[0].starts_with("1 new, 1 fixed, 1 changed across 1 release"));
        assert!(!c
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .any(|i| i.contains("old") || i.contains("X")));
    }

    #[test]
    fn detects_source_builds_and_snapshot_ids() {
        assert!(is_source_build(Path::new(
            r"C:\Users\me\agentos-studio\src-tauri\target\release\agentos-studio.exe"
        )));
        assert!(!is_source_build(Path::new(
            r"C:\Program Files\AgentOS Studio\agentos-studio.exe"
        )));
        assert_eq!(
            parse_snapshot_id(
                "State snapshot created: 20261106-194314-studio\n  1 snapshot(s) stored"
            ),
            Some("20261106-194314-studio".into())
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
