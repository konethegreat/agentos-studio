//! Artifacts: small web apps the owner builds inside Studio.
//!
//! An artifact is a folder of plain HTML, CSS and JavaScript under the Studio config folder. It
//! previews over a loopback HTTP server that requires a random per-run token, installs as its own
//! Studio window, and publishes to one GitHub Pages site. The site keeps a generated gallery at
//! the root and each published artifact at `a/<slug>/`, so every app gets a permanent public URL
//! that works in any browser without an AgentOS account.
//!
//! Only published artifacts leave the machine. Drafts stay local until Publish, and the site
//! repository refuses to accept files unless it carries the Studio marker or is empty.

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::{github, permissions, procs, store};

const SITE_REPO_KEY: &str = "artifacts.site.repo";
const SITE_MARKER: &str = ".agentos-artifacts.json";
const INSTALLED_KEY: &str = "artifacts.installed";
const DEFAULT_SITE_NAME: &str = "agentos-artifacts";
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_SERVE_BYTES: u64 = 8 * 1024 * 1024;
const GIT_LIMIT: Duration = Duration::from_secs(120);
const PUSH_LIMIT: Duration = Duration::from_secs(180);

type CmdResult<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Artifact {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub template: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub published_at: Option<i64>,
    pub live_url: Option<String>,
    pub folder: String,
    pub files: u32,
    pub bytes: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct ArtifactFile {
    pub path: String,
    pub bytes: u64,
    pub modified_at: i64,
}

#[derive(Serialize, Clone, Debug)]
pub struct SiteStatus {
    pub repo: String,
    pub configured: bool,
    pub pages_ready: bool,
    pub url: Option<String>,
    pub artifact_base: Option<String>,
    pub build_type: Option<String>,
    pub status: Option<String>,
    pub cname: Option<String>,
    pub https_enforced: bool,
    pub last_run: Option<Value>,
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct PublishResult {
    pub artifact: Artifact,
    pub url: String,
    pub site_repo: String,
    pub commit: String,
}

#[derive(Clone, Debug)]
struct Base {
    id: String,
    slug: String,
    name: String,
    description: String,
    template: String,
    created_at: i64,
    updated_at: i64,
    published_at: Option<i64>,
    live_url: Option<String>,
}

impl Base {
    fn finish(self) -> Artifact {
        let dir = artifact_dir(&self.slug);
        let (files, bytes) = scan_stats(&dir);
        Artifact {
            id: self.id,
            slug: self.slug,
            name: self.name,
            description: self.description,
            template: self.template,
            created_at: self.created_at,
            updated_at: self.updated_at,
            published_at: self.published_at,
            live_url: self.live_url,
            folder: dir.display().to_string(),
            files,
            bytes,
        }
    }
}

pub fn root() -> PathBuf {
    #[cfg(test)]
    {
        let mut found = None;
        TEST_ROOT.with(|c| found = c.borrow().clone());
        if let Some(p) = found {
            return p;
        }
    }
    store::studio_dir().join("artifacts")
}

#[cfg(test)]
thread_local! {
    static TEST_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn set_test_root(path: Option<PathBuf>) {
    TEST_ROOT.with(|c| *c.borrow_mut() = path);
}

pub(crate) fn artifact_dir(slug: &str) -> PathBuf {
    root().join(slug)
}

pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 48
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A stable, URL-safe name. Non-ASCII letters become separators, which keeps the published path
/// predictable on GitHub Pages.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "artifact".to_string()
    } else {
        out
    }
}

/// Text files the editor may open and save. Binary assets are allowed in a folder but not edited
/// as text.
pub fn valid_rel(path: &str) -> bool {
    let Some(p) = normal_rel(path) else {
        return false;
    };
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "html" | "css" | "js" | "mjs" | "json" | "txt" | "md" | "svg" | "webmanifest"
    )
}

/// Files Publish may copy. Adds the image and font types a page can reference.
pub fn valid_asset_rel(path: &str) -> bool {
    if valid_rel(path) {
        return true;
    }
    let Some(p) = normal_rel(path) else {
        return false;
    };
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "ico" | "woff" | "woff2" | "ttf"
    )
}

fn normal_rel(path: &str) -> Option<PathBuf> {
    if path.is_empty()
        || path.len() > 180
        || path.contains('\\')
        || path.starts_with('/')
        || path.contains(':')
    {
        return None;
    }
    let p = Path::new(path);
    for c in p.components() {
        match c {
            Component::Normal(name) => {
                let name = name.to_string_lossy();
                if name.is_empty() || name.starts_with('.') {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(p.to_path_buf())
}

pub fn valid_domain(domain: &str) -> bool {
    let d = domain.trim().trim_end_matches('.');
    !d.is_empty()
        && d.len() <= 253
        && d.contains('.')
        && d.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn scan_stats(dir: &Path) -> (u32, u64) {
    if !dir.is_dir() {
        return (0, 0);
    }
    let mut files = 0u32;
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(dir)
        .max_depth(2)
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_file() {
            files += 1;
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    (files, bytes)
}

fn mtime(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Base> {
    Ok(Base {
        id: r.get(0)?,
        slug: r.get(1)?,
        name: r.get(2)?,
        description: r.get(3)?,
        template: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        published_at: r.get(7)?,
        live_url: r.get(8)?,
    })
}

pub fn list_in(c: &Connection) -> Result<Vec<Artifact>> {
    let mut stmt = c.prepare(
        "SELECT id, slug, name, description, template, created_at, updated_at, published_at, live_url \
         FROM artifacts ORDER BY updated_at DESC, name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().map(Base::finish).collect())
}

fn get_base(c: &Connection, id: &str) -> Result<Base> {
    c.query_row(
        "SELECT id, slug, name, description, template, created_at, updated_at, published_at, live_url \
         FROM artifacts WHERE id = ?1",
        [id],
        from_row,
    )
    .optional()?
    .ok_or_else(|| anyhow!("artifact {id} does not exist"))
}

pub fn get_in(c: &Connection, id: &str) -> Result<Artifact> {
    Ok(get_base(c, id)?.finish())
}

fn unique_slug(c: &Connection, base: &str) -> Result<String> {
    for n in 1..=99 {
        let candidate = if n == 1 {
            base.to_string()
        } else {
            format!("{base}-{n}")
        };
        let taken: Option<String> = c
            .query_row(
                "SELECT id FROM artifacts WHERE slug = ?1",
                [&candidate],
                |r| r.get(0),
            )
            .optional()?;
        if taken.is_none() && !artifact_dir(&candidate).exists() {
            return Ok(candidate);
        }
    }
    bail!("could not find a free slug for {base}")
}

/// Create an artifact from a complete set of files. Generation has already validated the content;
/// this checks every path again, writes the folder, and inserts the row once, so a failed write
/// leaves no half artifact behind.
pub fn create_with_files(
    c: &Connection,
    name: &str,
    description: &str,
    files: &[(String, String)],
) -> Result<Artifact> {
    let name = name.trim();
    if name.is_empty() {
        bail!("Give the artifact a name");
    }
    if name.chars().count() > 80 {
        bail!("Artifact names are 80 characters or fewer");
    }
    let description: String = description.trim().chars().take(500).collect();
    if !files.iter().any(|(path, _)| path == "index.html") {
        bail!("An artifact needs an index.html");
    }
    let mut seen = HashSet::new();
    for (path, content) in files {
        if !valid_rel(path) {
            bail!("{path} is not a writable artifact file");
        }
        if path == "agentos.js" {
            bail!("agentos.js is generated by Studio");
        }
        if !seen.insert(path.as_str()) {
            bail!("{path} appears twice");
        }
        if content.len() as u64 > MAX_FILE_BYTES {
            bail!("{path} is larger than 1 MB");
        }
    }
    let slug = unique_slug(c, &slugify(name))?;
    let id = store::new_id();
    let now = store::now();
    let dir = artifact_dir(&slug);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    for (path, content) in files {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                let _ = fs::remove_dir_all(&dir);
                return Err(e).with_context(|| format!("creating {}", parent.display()));
            }
        }
        if let Err(e) = fs::write(&target, content) {
            let _ = fs::remove_dir_all(&dir);
            return Err(e).with_context(|| format!("writing {}", target.display()));
        }
    }

    if let Err(e) = c.execute(
        "INSERT INTO artifacts (id, slug, name, description, template, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, slug, name, description, "generated", now, now],
    ) {
        let _ = fs::remove_dir_all(&dir);
        return Err(e.into());
    }
    get_in(c, &id)
}

pub fn files_in(c: &Connection, id: &str) -> Result<Vec<ArtifactFile>> {
    let base = get_base(c, id)?;
    let dir = artifact_dir(&base.slug);
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(&dir)
        .max_depth(2)
        .into_iter()
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(&dir) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !valid_rel(&rel) {
            continue;
        }
        let meta = entry.metadata()?;
        out.push(ArtifactFile {
            path: rel,
            bytes: meta.len(),
            modified_at: mtime(&meta),
        });
    }
    out.sort_by(|a, b| {
        let rank = |p: &str| if p == "index.html" { 0 } else { 1 };
        rank(&a.path)
            .cmp(&rank(&b.path))
            .then_with(|| a.path.cmp(&b.path))
    });
    out.truncate(200);
    Ok(out)
}

fn checked_file(c: &Connection, id: &str, path: &str) -> Result<PathBuf> {
    let base = get_base(c, id)?;
    let rel = normal_rel(path).ok_or_else(|| anyhow!("{path} is not a plain artifact path"))?;
    if !valid_rel(path) {
        bail!("{path} is not an editable text file");
    }
    let dir = artifact_dir(&base.slug);
    let target = dir.join(rel);
    let canonical_dir = dir
        .canonicalize()
        .with_context(|| format!("artifact folder {} is missing", dir.display()))?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("{path} has no parent"))?
        .canonicalize()
        .with_context(|| format!("{} is missing", path))?;
    if !parent.starts_with(&canonical_dir) {
        bail!("{path} leaves the artifact folder");
    }
    Ok(target)
}

pub fn read_file_in(c: &Connection, id: &str, path: &str) -> Result<String> {
    let target = checked_file(c, id, path)?;
    let meta = fs::metadata(&target).with_context(|| format!("{path} does not exist"))?;
    if meta.len() > MAX_FILE_BYTES {
        bail!("{path} is larger than 1 MB");
    }
    let bytes = fs::read(&target)?;
    String::from_utf8(bytes).with_context(|| format!("{path} is not UTF-8 text"))
}

pub fn write_file_in(c: &Connection, id: &str, path: &str, content: &str) -> Result<Artifact> {
    if content.len() as u64 > MAX_FILE_BYTES {
        bail!("{path} is larger than 1 MB");
    }
    if normal_rel(path).is_some_and(|p| p == Path::new("agentos.js")) {
        bail!("agentos.js is generated by Studio and refreshed on every publish");
    }
    let base = get_base(c, id)?;
    if let Some(rel) = normal_rel(path) {
        if let Some(parent) = artifact_dir(&base.slug).join(rel).parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating the folder for {path}"))?;
        }
    }
    let target = checked_file(c, id, path)?;
    fs::write(&target, content).with_context(|| format!("writing {path}"))?;
    c.execute(
        "UPDATE artifacts SET updated_at = ?1 WHERE id = ?2",
        params![store::now(), id],
    )?;
    get_in(c, id)
}

fn git(dir: &Path, args: &[&str], limit: Duration) -> Result<String> {
    if procs::find("git").is_none() {
        bail!("git is not installed. Install Git and retry.");
    }
    let (code, out, stderr) = procs::run_with_timeout_env(
        "git",
        args,
        None,
        Some(dir),
        limit,
        &[("GIT_TERMINAL_PROMPT", "0"), ("GCM_INTERACTIVE", "never")],
    )?;
    if code != Some(0) {
        let msg = if stderr.trim().is_empty() {
            out.trim()
        } else {
            stderr.trim()
        };
        bail!("git {} failed: {msg}", args.join(" "));
    }
    Ok(out)
}

fn git_try(dir: &Path, args: &[&str], limit: Duration) -> Option<String> {
    let (code, out, _) = procs::run_with_timeout_env(
        "git",
        args,
        None,
        Some(dir),
        limit,
        &[("GIT_TERMINAL_PROMPT", "0"), ("GCM_INTERACTIVE", "never")],
    )
    .ok()?;
    if code == Some(0) {
        Some(out)
    } else {
        None
    }
}

fn gh_login() -> Result<String> {
    let login = github::gh(&["api", "user", "-q", ".login"], None, None)?
        .trim()
        .to_string();
    if login.is_empty() || !github::valid_repo(&format!("{login}/x")) {
        bail!("gh did not return a usable account name");
    }
    Ok(login)
}

fn configured_repo(c: &Connection) -> Result<Option<String>> {
    Ok(store::kv_get(c, SITE_REPO_KEY)?.filter(|v| github::valid_repo(v)))
}

fn site_repo(c: &Connection) -> Result<String> {
    if let Some(repo) = configured_repo(c)? {
        return Ok(repo);
    }
    let login = gh_login()?;
    Ok(format!("{login}/{DEFAULT_SITE_NAME}"))
}

pub fn site_url(owner: &str, repo: &str, cname: Option<&str>) -> String {
    if let Some(domain) = cname.filter(|d| valid_domain(d)) {
        return format!("https://{}", domain.trim().trim_end_matches('.'));
    }
    if repo.eq_ignore_ascii_case(&format!("{owner}.github.io")) {
        format!("https://{owner}.github.io")
    } else {
        format!("https://{owner}.github.io/{repo}")
    }
}

pub fn artifact_url(site_root: &str, slug: &str) -> String {
    format!("{}/a/{slug}/", site_root.trim_end_matches('/'))
}

fn split_repo(full: &str) -> Result<(&str, &str)> {
    full.split_once('/')
        .filter(|(owner, repo)| !owner.is_empty() && !repo.is_empty())
        .ok_or_else(|| anyhow!("Invalid repository name {full:?} (expected owner/name)"))
}

fn pages_json(full: &str) -> Result<Value> {
    github::gh_json(&["api", &format!("repos/{full}/pages")], None)
}

fn pages_put(full: &str, extra: &[(&str, &str)]) -> Result<()> {
    let mut args: Vec<String> = vec![
        "api".into(),
        "-X".into(),
        "PUT".into(),
        format!("repos/{full}/pages"),
        "-f".into(),
        "build_type=workflow".into(),
    ];
    for (key, value) in extra {
        args.push("-f".into());
        args.push(format!("{key}={value}"));
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    github::gh(&refs, None, None)?;
    Ok(())
}

/// Enables Actions-based Pages without dropping a custom domain or HTTPS enforcement that the
/// owner already configured. A legacy branch build is switched to workflow so the next push runs
/// the generated deployment.
fn enable_pages(full: &str) -> Result<Value> {
    let existing = pages_json(full).ok();
    if let Some(p) = existing.as_ref() {
        if p.get("build_type").and_then(Value::as_str) == Some("workflow") {
            return Ok(p.clone());
        }
        let mut extra = Vec::new();
        if let Some(cname) = p
            .get("cname")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
        {
            extra.push(("cname", cname));
        }
        if p.get("https_enforced").and_then(Value::as_bool) == Some(true) {
            extra.push(("https_enforced", "true"));
        }
        pages_put(full, &extra)?;
        return pages_json(full);
    }
    let created = github::gh(
        &[
            "api",
            "-X",
            "POST",
            &format!("repos/{full}/pages"),
            "-f",
            "build_type=workflow",
        ],
        None,
        None,
    );
    if let Err(first) = created {
        pages_put(full, &[]).map_err(|second| {
            anyhow!("GitHub Pages could not be enabled for {full}: {first}; then {second}")
        })?;
    }
    pages_json(full)
}

fn ensure_repo(full: &str) -> Result<()> {
    if github::gh(
        &["repo", "view", full, "--json", "nameWithOwner,isEmpty"],
        None,
        None,
    )
    .is_ok()
    {
        return Ok(());
    }
    github::gh(
        &[
            "repo",
            "create",
            full,
            "--public",
            "--description",
            "AgentOS Studio artifacts gallery",
        ],
        None,
        None,
    )
    .with_context(|| format!("creating {full}"))?;
    Ok(())
}

struct TempTree(PathBuf);

impl TempTree {
    fn new() -> Result<Self> {
        let path = store::studio_dir()
            .join("artifacts-publish")
            .join(uuid::Uuid::new_v4().simple().to_string());
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn installed_ids(c: &Connection) -> Result<Vec<String>> {
    Ok(store::kv_get(c, INSTALLED_KEY)?
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default())
}

fn set_installed(c: &Connection, id: &str, on: bool) -> Result<()> {
    let mut ids = installed_ids(c)?;
    ids.retain(|x| x != id);
    if on {
        ids.push(id.to_string());
    }
    store::kv_set(c, INSTALLED_KEY, &serde_json::to_string(&ids)?)?;
    Ok(())
}

static PUBLISH_LOCK: Mutex<()> = Mutex::new(());

fn require_marker(dir: &Path, full: &str) -> Result<()> {
    if dir.join(SITE_MARKER).is_file() {
        return Ok(());
    }
    let mut entries = 0usize;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy() == ".git" {
            continue;
        }
        entries += 1;
    }
    if entries > 0 {
        bail!(
            "{full} already has content that AgentOS Studio did not create. Choose an empty or dedicated site repository in Artifacts settings."
        );
    }
    Ok(())
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

fn sync_artifact_dir(src: &Path, dest: &Path, slug: &str) -> Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    fs::create_dir_all(dest)?;
    for entry in walkdir::WalkDir::new(src)
        .max_depth(3)
        .into_iter()
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(src) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !valid_asset_rel(&rel) {
            bail!("artifact {slug} contains {rel}, which is not a publishable file type");
        }
        let meta = entry.metadata()?;
        if meta.len() > MAX_FILE_BYTES {
            bail!("artifact {slug} contains {rel}, which is larger than 1 MB");
        }
        let target = dest.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(entry.path(), &target)?;
    }
    write_text(&dest.join("agentos.js"), RUNTIME_JS)?;
    if !dest.join("index.html").is_file() {
        bail!("artifact {slug} has no index.html");
    }
    Ok(())
}

fn published_artifacts(c: &Connection, include: &str) -> Result<Vec<Artifact>> {
    let mut out = list_in(c)?;
    out.retain(|a| a.published_at.is_some() || a.id == include);
    Ok(out)
}

fn dated(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.format("%d %b %Y").to_string())
        .unwrap_or_default()
}

pub fn gallery_html(artifacts: &[Artifact]) -> String {
    let cards = if artifacts.is_empty() {
        "<p class=\"empty\">Nothing published yet.</p>".to_string()
    } else {
        artifacts
            .iter()
            .map(|a| {
                let name = escape_html(&a.name);
                let desc = escape_html(&a.description);
                let desc = if desc.is_empty() {
                    "A small app built in AgentOS Studio.".to_string()
                } else {
                    desc
                };
                let kind = "Web app";
                format!(
                    "<a class=\"card\" href=\"a/{slug}/\" data-name=\"{name}\"><div class=\"top\"><span class=\"dot\"></span><span class=\"kind\">{kind}</span></div><h2>{name}</h2><p>{desc}</p><div class=\"meta\"><span>Updated {updated}</span><span>{files} files</span></div></a>",
                    slug = a.slug,
                    name = name,
                    kind = kind,
                    desc = desc,
                    updated = dated(a.updated_at),
                    files = a.files
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    GALLERY_HTML
        .replace("__CARDS__", &cards)
        .replace("__COUNT__", &artifacts.len().to_string())
}

fn manifest_json(artifacts: &[Artifact], full: &str) -> Value {
    json!({
        "site": "AgentOS Artifacts",
        "repo": full,
        "generated_at": store::now(),
        "artifacts": artifacts.iter().map(|a| json!({
            "slug": a.slug,
            "name": a.name,
            "description": a.description,
            "template": a.template,
            "path": format!("a/{}/", a.slug),
            "updated_at": a.updated_at,
        })).collect::<Vec<_>>(),
    })
}

pub fn delete_in(c: &Connection, id: &str, approved: bool) -> Result<()> {
    let base = get_base(c, id)?;
    permissions::run_guarded(
        "artifacts.delete",
        &format!("artifact {}", base.slug),
        &format!(
            "Delete \"{}\" and its files from this machine. A published copy stays online until the next publish.",
            base.name
        ),
        approved,
        || {
            let dir = artifact_dir(&base.slug);
            if dir.exists() {
                let root_dir = root().canonicalize().context("artifact root is missing")?;
                let target = dir
                    .canonicalize()
                    .with_context(|| format!("resolving {}", dir.display()))?;
                if !target.starts_with(&root_dir) {
                    bail!("{} is outside the artifact folder", dir.display());
                }
                fs::remove_dir_all(&target).with_context(|| format!("deleting {}", target.display()))?;
            }
            c.execute("DELETE FROM artifacts WHERE id = ?1", [id])?;
            set_installed(c, id, false)?;
            Ok(())
        },
    )
}

fn prune_site(parent: &Path, keep: &[Artifact]) -> Result<()> {
    if !parent.is_dir() {
        return Ok(());
    }
    let keep: HashSet<&str> = keep.iter().map(|a| a.slug.as_str()).collect();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !keep.contains(name.as_str()) {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

fn publish_impl(c: &Connection, id: &str) -> Result<PublishResult> {
    let _serial = PUBLISH_LOCK
        .lock()
        .map_err(|_| anyhow!("artifact publish lock is poisoned"))?;
    let base = get_base(c, id)?;
    let report = crate::artifact_check::check(&artifact_dir(&base.slug));
    if !report.ok {
        let mut lines: Vec<String> = report
            .issues
            .iter()
            .take(10)
            .map(|i| {
                if i.line > 0 {
                    format!("{}:{} {}", i.file, i.line, i.message)
                } else {
                    format!("{} {}", i.file, i.message)
                }
            })
            .collect();
        if report.issues.len() > 10 {
            lines.push(format!("and {} more", report.issues.len() - 10));
        }
        bail!(
            "Publish stopped: the artifact has {} problem(s).\n{}",
            report.issues.len(),
            lines.join("\n")
        );
    }
    let full = site_repo(c)?;
    let (owner, repo_name) = split_repo(&full)?;
    ensure_repo(&full)?;

    let changed_at = store::now();

    let tmp = TempTree::new()?;
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"], GIT_LIMIT)?;
    git(
        dir,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://github.com/{full}.git"),
        ],
        GIT_LIMIT,
    )?;

    let has_main = git_try(
        dir,
        &["ls-remote", "--exit-code", "--heads", "origin", "main"],
        GIT_LIMIT,
    )
    .is_some();
    if has_main {
        git(dir, &["fetch", "--depth=1", "origin", "main"], GIT_LIMIT)?;
        git(dir, &["reset", "--hard", "FETCH_HEAD"], GIT_LIMIT)?;
        require_marker(dir, &full)?;
    }

    let published = published_artifacts(c, id)?;
    prune_site(&dir.join("a"), &published)?;
    for artifact in &published {
        let src = artifact_dir(&artifact.slug);
        sync_artifact_dir(&src, &dir.join("a").join(&artifact.slug), &artifact.slug)?;
    }

    let site = gallery_html(&published);
    write_text(&dir.join("index.html"), &site)?;
    write_text(&dir.join(".nojekyll"), "")?;
    write_text(&dir.join("404.html"), SITE_404)?;
    write_text(&dir.join(SITE_MARKER), SITE_MARKER_JSON)?;
    write_text(&dir.join("runtime").join("agentos.js"), RUNTIME_JS)?;
    write_text(
        &dir.join("manifest.json"),
        &serde_json::to_string_pretty(&manifest_json(&published, &full))?,
    )?;
    write_text(
        &dir.join(".github").join("workflows").join("pages.yml"),
        PAGES_WORKFLOW,
    )?;

    git(dir, &["add", "-A"], GIT_LIMIT)?;
    let status = git(dir, &["status", "--porcelain"], GIT_LIMIT)?;
    let mut pushed = false;
    if !status.trim().is_empty() {
        let message = format!("Publish {}", base.name);
        git(
            dir,
            &[
                "-c",
                "user.name=AgentOS Studio",
                "-c",
                "user.email=studio@agentos.local",
                "commit",
                "-m",
                &message,
            ],
            GIT_LIMIT,
        )?;
        pushed = true;
    }
    let commit = git(dir, &["rev-parse", "HEAD"], GIT_LIMIT)?
        .trim()
        .to_string();
    if pushed || !has_main {
        git(dir, &["push", "origin", "HEAD:main"], PUSH_LIMIT)?;
    }

    let pages = enable_pages(&full)?;
    let cname = pages.get("cname").and_then(Value::as_str);
    let site_root = pages
        .get("html_url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| site_url(owner, repo_name, cname));
    let url = artifact_url(&site_root, &base.slug);

    c.execute(
        "UPDATE artifacts SET published_at = ?1, live_url = ?2, updated_at = ?1 WHERE id = ?3",
        params![changed_at, url, id],
    )?;

    Ok(PublishResult {
        artifact: get_in(c, id)?,
        url,
        site_repo: full,
        commit,
    })
}

pub fn publish_in(c: &Connection, id: &str, approved: bool) -> Result<PublishResult> {
    let base = get_base(c, id)?;
    let summary = format!(
        "Publish \"{}\" to the public GitHub Pages site. Draft artifacts stay private.",
        base.name
    );
    permissions::run_guarded(
        "artifacts.publish",
        &format!("artifact {}", base.slug),
        &summary,
        approved,
        || publish_impl(c, id),
    )
}

pub fn site_status_in(c: &Connection) -> Result<SiteStatus> {
    let configured = configured_repo(c)?;
    let is_configured = configured.is_some();
    let full = match configured {
        Some(repo) => repo,
        None => match gh_login() {
            Ok(login) => format!("{login}/{DEFAULT_SITE_NAME}"),
            Err(e) => {
                return Ok(SiteStatus {
                    repo: String::new(),
                    configured: false,
                    pages_ready: false,
                    url: None,
                    artifact_base: None,
                    build_type: None,
                    status: None,
                    cname: None,
                    https_enforced: false,
                    last_run: None,
                    error: Some(format!("{e:#}")),
                })
            }
        },
    };
    let (owner, repo_name) = split_repo(&full)?;
    let pages = pages_json(&full);
    let last_run = github::gh(
        &[
            "run",
            "list",
            "-R",
            &full,
            "--workflow",
            "pages.yml",
            "--limit",
            "1",
            "--json",
            "databaseId,status,conclusion,createdAt,updatedAt,url",
        ],
        None,
        None,
    )
    .ok()
    .and_then(|out| serde_json::from_str::<Value>(&out).ok())
    .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));

    match pages {
        Ok(p) => {
            let cname = p.get("cname").and_then(Value::as_str).map(str::to_string);
            let alt = site_url(owner, repo_name, cname.as_deref());
            let url = p
                .get("html_url")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| alt.clone());
            Ok(SiteStatus {
                repo: full,
                configured: is_configured,
                pages_ready: true,
                url: Some(url),
                artifact_base: Some(alt),
                build_type: p
                    .get("build_type")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                status: p.get("status").and_then(Value::as_str).map(str::to_string),
                cname,
                https_enforced: p
                    .get("https_enforced")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                last_run,
                error: None,
            })
        }
        Err(e) => {
            let message = format!("{e:#}");
            let pages_ready = !(message.contains("404") || message.contains("Not Found"));
            Ok(SiteStatus {
                repo: full.clone(),
                configured: is_configured,
                pages_ready: false,
                url: None,
                artifact_base: Some(site_url(owner, repo_name, None)),
                build_type: None,
                status: None,
                cname: None,
                https_enforced: false,
                last_run,
                error: if pages_ready { Some(message) } else { None },
            })
        }
    }
}

pub fn set_site_in(c: &Connection, repo: &str) -> Result<SiteStatus> {
    let full = repo.trim();
    if !github::valid_repo(full) {
        bail!("The site repository must be owner/name");
    }
    let login = gh_login()?;
    let (owner, _) = split_repo(full)?;
    if !owner.eq_ignore_ascii_case(&login) {
        bail!("The site repository must belong to {login}");
    }
    store::kv_set(c, SITE_REPO_KEY, full)?;
    site_status_in(c)
}

pub fn set_domain_in(c: &Connection, domain: &str, approved: bool) -> Result<SiteStatus> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if !valid_domain(&domain) {
        bail!("Enter a plain domain such as artifacts.example.com");
    }
    let full = site_repo(c)?;
    permissions::run_guarded(
        "artifacts.domain",
        &format!("site {full}"),
        &format!("Point the artifact site at {domain}. DNS must already point at GitHub Pages."),
        approved,
        || {
            github::gh(
                &[
                    "api",
                    "-X",
                    "PUT",
                    &format!("repos/{full}/pages"),
                    "-f",
                    "build_type=workflow",
                    "-f",
                    &format!("cname={domain}"),
                    "-F",
                    "https_enforced=true",
                ],
                None,
                None,
            )?;
            Ok(())
        },
    )?;
    site_status_in(c)
}

struct Preview {
    port: u16,
    token: String,
}

static PREVIEW: Mutex<Option<Preview>> = Mutex::new(None);

fn ensure_preview() -> Result<(u16, String)> {
    let mut guard = PREVIEW
        .lock()
        .map_err(|_| anyhow!("preview server lock is poisoned"))?;
    if let Some(p) = guard.as_ref() {
        return Ok((p.port, p.token.clone()));
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("starting the preview server")?;
    let port = listener.local_addr()?.port();
    let token = uuid::Uuid::new_v4().simple().to_string();
    *guard = Some(Preview {
        port,
        token: token.clone(),
    });
    std::thread::Builder::new()
        .name("artifact-preview".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        std::thread::spawn(move || {
                            let _ = serve(stream);
                        });
                    }
                    Err(_) => break,
                }
            }
        })
        .context("starting the preview thread")?;
    Ok((port, token))
}

pub fn preview_url(slug: &str) -> Result<String> {
    if !valid_slug(slug) {
        bail!("Invalid artifact slug");
    }
    if !artifact_dir(slug).is_dir() {
        bail!("artifact {slug} has no folder");
    }
    let (port, token) = ensure_preview()?;
    Ok(format!("http://127.0.0.1:{port}/{token}/{slug}/"))
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "webmanifest" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "md" | "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn respond(stream: &mut TcpStream, code: u16, kind: &str, body: &[u8], head: bool) -> Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    if !head {
        let _ = stream.write_all(body);
    }
    let _ = stream.flush();
    Ok(())
}

fn respond_html(stream: &mut TcpStream, body: &[u8], head: bool) -> Result<()> {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: frame-ancestors 'self' tauri://localhost https://tauri.localhost http://tauri.localhost http://localhost:1420 http://127.0.0.1:1420\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    if !head {
        let _ = stream.write_all(body);
    }
    let _ = stream.flush();
    Ok(())
}

/// Put the customizer script just before the closing body tag. Preview only; Publish copies the
/// folder as it is, so the public page never carries the script.
pub fn inject_customizer(html: &str, token: &str) -> String {
    if token.is_empty() || html.contains("/_customizer.js") {
        return html.to_string();
    }
    let tag = format!("<script src=\"/{token}/_customizer.js\"></script>");
    let lower = html.to_ascii_lowercase();
    match lower.rfind("</body>") {
        Some(pos) => {
            let mut out = String::with_capacity(html.len() + tag.len() + 1);
            out.push_str(&html[..pos]);
            out.push_str(&tag);
            out.push('\n');
            out.push_str(&html[pos..]);
            out
        }
        None => format!("{html}{tag}\n"),
    }
}

/// Scripts the preview server answers itself, keyed by the path after the token. The runtime is
/// only on disk after a publish, so a page that links agentos.js would 404 in the preview without
/// this.
fn served_override(rest: &[&str]) -> Option<&'static [u8]> {
    match rest {
        ["_customizer.js"] => Some(crate::artifact_customize::CUSTOMIZER_JS.as_bytes()),
        [_, "agentos.js"] => Some(RUNTIME_JS.as_bytes()),
        _ => None,
    }
}

fn serve(mut stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }
    let request = String::from_utf8_lossy(&buf[..n]);
    let first = request.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw = parts.next().unwrap_or("/");
    let head = method == "HEAD";
    if method != "GET" && !head {
        return respond(
            &mut stream,
            405,
            "text/plain; charset=utf-8",
            b"Method not allowed",
            false,
        );
    }
    if raw.contains('%') {
        return respond(
            &mut stream,
            400,
            "text/plain; charset=utf-8",
            b"Bad path",
            head,
        );
    }
    let path = raw.split(['?', '#']).next().unwrap_or("/");
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let (token, rest) = match segments.split_first() {
        Some(pair) => pair,
        None => {
            return respond(
                &mut stream,
                404,
                "text/plain; charset=utf-8",
                b"Not found",
                head,
            )
        }
    };
    let expected = PREVIEW
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.token.clone()))
        .unwrap_or_default();
    if token.is_empty() || *token != expected {
        return respond(
            &mut stream,
            404,
            "text/plain; charset=utf-8",
            b"Not found",
            head,
        );
    }
    let Some(slug) = rest.first().copied() else {
        return respond(
            &mut stream,
            404,
            "text/plain; charset=utf-8",
            b"Not found",
            head,
        );
    };
    if let Some(body) = served_override(rest) {
        return respond(
            &mut stream,
            200,
            "text/javascript; charset=utf-8",
            body,
            head,
        );
    }
    if !valid_slug(slug) {
        return respond(
            &mut stream,
            404,
            "text/plain; charset=utf-8",
            b"Not found",
            head,
        );
    }
    let canonical_root = match artifact_dir(slug).canonicalize() {
        Ok(p) => p,
        Err(_) => {
            return respond(
                &mut stream,
                404,
                "text/plain; charset=utf-8",
                b"Not found",
                head,
            )
        }
    };
    let mut file = canonical_root.clone();
    for part in &rest[1..] {
        if *part == "." || *part == ".." || part.contains('\\') || part.contains(':') {
            return respond(
                &mut stream,
                403,
                "text/plain; charset=utf-8",
                b"Forbidden",
                head,
            );
        }
        file.push(part);
    }
    if rest.len() == 1 || path.ends_with('/') {
        file.push("index.html");
    }
    let file = match file.canonicalize() {
        Ok(p) if p.starts_with(&canonical_root) => p,
        Ok(_) => {
            return respond(
                &mut stream,
                403,
                "text/plain; charset=utf-8",
                b"Forbidden",
                head,
            )
        }
        Err(_) => {
            return respond(
                &mut stream,
                404,
                "text/plain; charset=utf-8",
                b"Not found",
                head,
            )
        }
    };
    match fs::metadata(&file) {
        Ok(m) if m.is_file() && m.len() <= MAX_SERVE_BYTES => {}
        Ok(m) if m.is_file() => {
            return respond(
                &mut stream,
                413,
                "text/plain; charset=utf-8",
                b"File too large",
                head,
            )
        }
        _ => {
            return respond(
                &mut stream,
                404,
                "text/plain; charset=utf-8",
                b"Not found",
                head,
            )
        }
    }
    match fs::read(&file) {
        Ok(body) => {
            let kind = content_type(&file);
            if kind.starts_with("text/html") {
                let html = String::from_utf8_lossy(&body);
                let injected = inject_customizer(&html, &expected);
                respond_html(&mut stream, injected.as_bytes(), head)
            } else {
                respond(&mut stream, 200, kind, &body, head)
            }
        }
        Err(_) => respond(
            &mut stream,
            404,
            "text/plain; charset=utf-8",
            b"Not found",
            head,
        ),
    }
}

async fn open_artifact_window(
    app: AppHandle,
    label: String,
    title: String,
    url: String,
) -> CmdResult<()> {
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let parsed = tauri::Url::parse(&url).map_err(|e| e.to_string())?;
    WebviewWindowBuilder::new(&app, label, WebviewUrl::External(parsed))
        .title(title)
        .inner_size(1120.0, 800.0)
        .min_inner_size(420.0, 320.0)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

#[tauri::command]
pub async fn artifacts_list() -> CmdResult<Vec<Artifact>> {
    blocking(|| {
        let c = store::open()?;
        list_in(&c)
    })
    .await
}

#[tauri::command]
pub async fn artifacts_create(
    name: String,
    description: String,
    agent: Option<String>,
) -> CmdResult<crate::artifact_generate::Generated> {
    blocking(move || {
        let c = store::open()?;
        crate::artifact_generate::generate_in(
            &c,
            &name,
            &description,
            agent.as_deref(),
            crate::artifact_generate::GENERATE_LIMIT,
        )
    })
    .await
}

#[tauri::command]
pub async fn artifacts_files(id: String) -> CmdResult<Vec<ArtifactFile>> {
    blocking(move || {
        let c = store::open()?;
        files_in(&c, &id)
    })
    .await
}

#[tauri::command]
pub async fn artifacts_read_file(id: String, path: String) -> CmdResult<String> {
    blocking(move || {
        let c = store::open()?;
        read_file_in(&c, &id, &path)
    })
    .await
}

#[tauri::command]
pub async fn artifacts_write_file(
    id: String,
    path: String,
    content: String,
) -> CmdResult<Artifact> {
    blocking(move || {
        let c = store::open()?;
        write_file_in(&c, &id, &path, &content)
    })
    .await
}

#[tauri::command]
pub async fn artifacts_validate(id: String) -> CmdResult<crate::artifact_check::Report> {
    blocking(move || {
        let c = store::open()?;
        let artifact = get_in(&c, &id)?;
        Ok(crate::artifact_check::check(&artifact_dir(&artifact.slug)))
    })
    .await
}

#[tauri::command]
pub async fn artifacts_customize_agents() -> CmdResult<Vec<crate::artifact_customize::AgentChoice>>
{
    Ok(crate::artifact_customize::agents())
}

#[tauri::command]
pub async fn artifacts_customize(
    id: String,
    selection: crate::artifact_customize::Selection,
    instruction: String,
    agent: Option<String>,
) -> CmdResult<crate::artifact_customize::Plan> {
    blocking(move || {
        let c = store::open()?;
        crate::artifact_customize::customize_in(
            &c,
            &id,
            &selection,
            &instruction,
            agent.as_deref(),
            crate::artifact_customize::TURN_LIMIT,
        )
    })
    .await
}

#[tauri::command]
pub async fn artifacts_customize_apply(
    id: String,
    edits: Vec<crate::artifact_customize::Edit>,
) -> CmdResult<Artifact> {
    blocking(move || {
        let c = store::open()?;
        crate::artifact_customize::apply_in(&c, &id, &edits)
    })
    .await
}

#[tauri::command]
pub async fn artifacts_preview_url(id: String) -> CmdResult<String> {
    blocking(move || {
        let c = store::open()?;
        let artifact = get_in(&c, &id)?;
        preview_url(&artifact.slug)
    })
    .await
}

#[tauri::command]
pub async fn artifacts_site_status() -> CmdResult<SiteStatus> {
    blocking(|| {
        let c = store::open()?;
        site_status_in(&c)
    })
    .await
}

#[tauri::command]
pub async fn artifacts_set_site(repo: String) -> CmdResult<SiteStatus> {
    blocking(move || {
        let c = store::open()?;
        set_site_in(&c, &repo)
    })
    .await
}

#[tauri::command]
pub async fn artifacts_set_domain(domain: String, approved: Option<bool>) -> CmdResult<SiteStatus> {
    blocking(move || {
        let c = store::open()?;
        set_domain_in(&c, &domain, approved.unwrap_or(false))
    })
    .await
}

#[tauri::command]
pub async fn artifacts_publish(id: String, approved: Option<bool>) -> CmdResult<PublishResult> {
    blocking(move || {
        let c = store::open()?;
        publish_in(&c, &id, approved.unwrap_or(false))
    })
    .await
}

#[tauri::command]
pub async fn artifacts_remove(id: String, approved: Option<bool>) -> CmdResult<()> {
    blocking(move || {
        let c = store::open()?;
        delete_in(&c, &id, approved.unwrap_or(false))
    })
    .await
}

#[tauri::command]
pub async fn artifacts_preview_window(app: AppHandle, id: String) -> CmdResult<String> {
    let (slug, name, url) = blocking(move || {
        let c = store::open()?;
        let artifact = get_in(&c, &id)?;
        let url = preview_url(&artifact.slug)?;
        Ok((artifact.slug, artifact.name, url))
    })
    .await?;
    open_artifact_window(
        app,
        format!("artifact-preview-{slug}"),
        format!("{name} (preview)"),
        url.clone(),
    )
    .await?;
    Ok(url)
}

#[tauri::command]
pub async fn artifacts_install_window(app: AppHandle, id: String) -> CmdResult<String> {
    let (slug, name, url) = blocking(move || {
        let c = store::open()?;
        let artifact = get_in(&c, &id)?;
        let url = artifact
            .live_url
            .clone()
            .ok_or_else(|| anyhow!("Publish \"{}\" first, then install it", artifact.name))?;
        set_installed(&c, &artifact.id, true)?;
        Ok((artifact.slug, artifact.name, url))
    })
    .await?;
    open_artifact_window(app, format!("artifact-app-{slug}"), name, url.clone()).await?;
    Ok(url)
}

#[tauri::command]
pub async fn artifacts_uninstall(id: String) -> CmdResult<()> {
    blocking(move || {
        let c = store::open()?;
        set_installed(&c, &id, false)
    })
    .await
}

#[tauri::command]
pub async fn artifacts_installed() -> CmdResult<Vec<String>> {
    blocking(|| {
        let c = store::open()?;
        installed_ids(&c)
    })
    .await
}

/// Reopens every installed artifact that has a live URL. Called once when the GUI starts.
pub fn reopen_installed(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let rows = tauri::async_runtime::spawn_blocking(|| -> Result<Vec<Artifact>> {
            let c = store::open()?;
            let mut out = Vec::new();
            for id in installed_ids(&c)? {
                if let Ok(artifact) = get_in(&c, &id) {
                    if artifact.live_url.is_some() {
                        out.push(artifact);
                    }
                }
            }
            Ok(out)
        })
        .await;
        let Ok(Ok(rows)) = rows else {
            return;
        };
        for artifact in rows {
            let Some(url) = artifact.live_url.clone() else {
                continue;
            };
            if let Err(e) = open_artifact_window(
                app.clone(),
                format!("artifact-app-{}", artifact.slug),
                artifact.name,
                url,
            )
            .await
            {
                eprintln!("artifact window {}: {e}", artifact.slug);
            }
        }
    });
}

const RUNTIME_JS: &str = r#"(function () {
  var slug = document.documentElement.getAttribute("data-artifact") || location.pathname.split("/").filter(Boolean).pop() || "app";
  var prefix = "agentos:" + slug + ":";
  function key(name) { return prefix + name; }
  function get(name, fallback) {
    try {
      var raw = localStorage.getItem(key(name));
      return raw === null ? fallback : JSON.parse(raw);
    } catch (error) {
      return fallback;
    }
  }
  function set(name, value) {
    localStorage.setItem(key(name), JSON.stringify(value));
    return value;
  }
  function remove(name) { localStorage.removeItem(key(name)); }
  function all() {
    var data = {};
    for (var i = 0; i < localStorage.length; i += 1) {
      var rawKey = localStorage.key(i);
      if (rawKey && rawKey.indexOf(prefix) === 0) {
        try { data[rawKey.slice(prefix.length)] = JSON.parse(localStorage.getItem(rawKey)); } catch (error) { /* keep other keys usable */ }
      }
    }
    return data;
  }
  function clear() {
    Object.keys(all()).forEach(remove);
  }
  function exportData() {
    return JSON.stringify({ artifact: slug, exported: new Date().toISOString(), data: all() }, null, 2);
  }
  function importData(text) {
    var parsed = JSON.parse(text);
    var data = parsed && parsed.data ? parsed.data : parsed;
    Object.keys(data).forEach(function (name) { set(name, data[name]); });
    return data;
  }
  function download(filename, text) {
    var blob = new Blob([text], { type: "application/json" });
    var url = URL.createObjectURL(blob);
    var link = document.createElement("a");
    link.href = url;
    link.download = filename;
    link.click();
    URL.revokeObjectURL(url);
  }
  function fetchJSON(url, options) {
    return fetch(url, options).then(function (response) {
      if (!response.ok) { throw new Error("HTTP " + response.status); }
      return response.json();
    });
  }
  window.AgentOS = {
    slug: slug,
    get: get,
    set: set,
    remove: remove,
    all: all,
    clear: clear,
    exportData: exportData,
    importData: importData,
    download: download,
    fetchJSON: fetchJSON
  };
})();
"#;
const GALLERY_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>AgentOS Artifacts</title>
  <style>
    :root {
      color-scheme: dark;
      --bg: #07100c;
      --card: #0e1a15;
      --line: #1d2e26;
      --ink: #edf7f1;
      --muted: #8fa79b;
      --accent: #34d399;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      padding: 56px 24px 80px;
      background:
        radial-gradient(900px 500px at 15% 0%, rgba(52, 211, 153, 0.16), transparent 60%),
        radial-gradient(700px 420px at 90% 10%, rgba(56, 189, 248, 0.12), transparent 55%),
        var(--bg);
      color: var(--ink);
      font-family: ui-sans-serif, system-ui, "Segoe UI", sans-serif;
    }
    .wrap { width: min(1080px, 100%); margin: 0 auto; }
    header { display: flex; align-items: flex-end; justify-content: space-between; gap: 24px; flex-wrap: wrap; }
    .eyebrow {
      margin: 0 0 10px;
      font-size: 11px;
      font-weight: 700;
      letter-spacing: 0.16em;
      text-transform: uppercase;
      color: var(--accent);
    }
    h1 { margin: 0; font-size: clamp(38px, 7vw, 72px); line-height: 0.95; letter-spacing: -0.03em; }
    .lede { margin: 16px 0 0; max-width: 560px; font-size: 16px; line-height: 1.6; color: var(--muted); }
    .tools { display: flex; align-items: center; gap: 10px; }
    input {
      width: 230px;
      border: 1px solid var(--line);
      border-radius: 999px;
      padding: 11px 16px;
      font: inherit;
      font-size: 14px;
      color: var(--ink);
      background: rgba(14, 26, 21, 0.8);
    }
    input:focus { outline: 2px solid rgba(52, 211, 153, 0.3); border-color: var(--accent); }
    .count { font-size: 13px; color: var(--muted); white-space: nowrap; }
    .grid { margin-top: 38px; display: grid; grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); gap: 16px; }
    .card {
      position: relative;
      display: flex;
      flex-direction: column;
      min-height: 200px;
      padding: 20px;
      border: 1px solid var(--line);
      border-radius: 18px;
      background: linear-gradient(180deg, rgba(255, 255, 255, 0.035), rgba(255, 255, 255, 0.01)), var(--card);
      color: inherit;
      text-decoration: none;
      transition: transform 140ms ease, border-color 140ms ease;
    }
    .card:hover { transform: translateY(-2px); border-color: rgba(52, 211, 153, 0.5); }
    .top { display: flex; align-items: center; justify-content: space-between; }
    .dot { width: 28px; height: 28px; border-radius: 9px; background: linear-gradient(135deg, #34d399, #22d3ee); }
    .kind { font-size: 10px; font-weight: 700; letter-spacing: 0.12em; text-transform: uppercase; color: var(--muted); }
    h2 { margin: 18px 0 8px; font-size: 22px; line-height: 1.15; }
    .card p { margin: 0; color: var(--muted); font-size: 14px; line-height: 1.55; }
    .meta { margin-top: auto; padding-top: 18px; display: flex; justify-content: space-between; gap: 10px; font-size: 11px; color: #6c8679; }
    .empty { padding: 40px; border: 1px dashed var(--line); border-radius: 18px; text-align: center; color: var(--muted); }
    footer { margin-top: 46px; padding-top: 18px; border-top: 1px solid var(--line); font-size: 12px; color: #6c8679; }
    footer a { color: var(--accent); text-decoration: none; }
    .hide { display: none; }
  </style>
</head>
<body>
  <div class="wrap">
    <header>
      <div>
        <p class="eyebrow">Published from AgentOS Studio</p>
        <h1>Artifacts</h1>
        <p class="lede">Small apps with permanent public URLs. Every card is a plain web page you can open in any browser, install as a window in Studio, and keep offline after the first visit.</p>
      </div>
      <div class="tools">
        <input id="search" type="search" placeholder="Find an artifact" aria-label="Find an artifact">
        <span class="count">__COUNT__ apps</span>
      </div>
    </header>
    <section id="grid" class="grid">
      __CARDS__
    </section>
    <footer>Built with <a href="https://github.com/konethegreat/agentos-studio">AgentOS Studio</a>. Each app keeps its data in your browser.</footer>
  </div>
  <script>
    var search = document.querySelector("#search");
    search.addEventListener("input", function () {
      var query = search.value.trim().toLowerCase();
      document.querySelectorAll(".card").forEach(function (card) {
        var name = (card.getAttribute("data-name") || "").toLowerCase();
        card.classList.toggle("hide", query.length > 0 && name.indexOf(query) === -1);
      });
    });
  </script>
</body>
</html>
"##;

const PAGES_WORKFLOW: &str = r#"name: Deploy artifacts
on:
  push:
    branches: [main]
  workflow_dispatch:
permissions:
  contents: read
  pages: write
  id-token: write
concurrency:
  group: pages
  cancel-in-progress: false
jobs:
  deploy:
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: actions/configure-pages@v5
      - uses: actions/upload-pages-artifact@v4
        with:
          path: .
      - id: deployment
        uses: actions/deploy-pages@v4
"#;

const SITE_404: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Not found</title>
  <style>
    body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: #07100c; color: #edf7f1; font-family: ui-sans-serif, system-ui, sans-serif; }
    main { text-align: center; padding: 32px; }
    h1 { font-size: 28px; }
    a { color: #34d399; }
  </style>
</head>
<body>
  <main>
    <h1>That artifact is not here.</h1>
    <p><a href="/">Open the gallery</a></p>
  </main>
</body>
</html>
"#;

const SITE_MARKER_JSON: &str = "{\n  \"managed_by\": \"AgentOS Studio\",\n  \"purpose\": \"Artifacts gallery and published apps\",\n  \"version\": 1\n}\n";

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().expect("temp dir");
        set_test_root(Some(dir.path().join("artifacts")));
        (dir, store::test_conn())
    }

    fn seed(c: &Connection, name: &str) -> Artifact {
        let title = escape_html(name);
        let files = vec![
            (
                "index.html".to_string(),
                format!("<title>{title}</title><h1>{title}</h1>"),
            ),
            (
                "styles.css".to_string(),
                "body { color: #111; }\n".to_string(),
            ),
            (
                "app.js".to_string(),
                "console.log('ready');\n".to_string(),
            ),
        ];
        create_with_files(c, name, "A test app", &files).expect("create")
    }

    #[test]
    fn slugify_is_url_safe_and_stable() {
        assert_eq!(slugify("My Todo!"), "my-todo");
        assert_eq!(slugify("  ***  "), "artifact");
        assert_eq!(slugify("Cafe Notes"), "cafe-notes");
        assert_eq!(slugify("a---b"), "a-b");
        assert_eq!(slugify(&"x".repeat(80)).len(), 48);
        assert!(valid_slug(&slugify("Hello World")));
        assert!(!valid_slug("Hello"));
        assert!(!valid_slug("-hello"));
        assert!(!valid_slug("hello-"));
        assert!(!valid_slug("hello_world"));
    }

    #[test]
    fn paths_stay_inside_the_artifact() {
        assert!(valid_rel("index.html"));
        assert!(valid_rel("assets/app.js"));
        assert!(valid_rel("README.md"));
        assert!(valid_asset_rel("assets/logo.png"));
        assert!(!valid_rel("../index.html"));
        assert!(!valid_rel("a/../../index.html"));
        assert!(!valid_rel("/index.html"));
        assert!(!valid_rel("a\\index.html"));
        assert!(!valid_rel(".git/config"));
        assert!(!valid_rel("index.exe"));
        assert!(!valid_rel(""));
        assert!(!valid_asset_rel("../logo.png"));
    }

    #[test]
    fn the_preview_serves_the_customizer_and_the_runtime() {
        let body = |rest: &[&str]| served_override(rest).map(|b| b.len());
        assert_eq!(body(&["_customizer.js"]), Some(crate::artifact_customize::CUSTOMIZER_JS.len()));
        assert_eq!(body(&["probe", "agentos.js"]), Some(RUNTIME_JS.len()));
        assert_eq!(body(&["probe", "app.js"]), None);
        assert_eq!(body(&["agentos.js"]), None);
    }

    #[test]
    fn created_files_land_in_the_folder_and_an_index_is_required() {
        let (_dir, c) = fixture();
        let a = seed(&c, "My App");
        let files = files_in(&c, &a.id).expect("files");
        let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(names.contains(&"index.html"));
        assert!(names.contains(&"styles.css"));
        assert!(names.contains(&"app.js"));
        assert!(!names.contains(&"agentos.js"), "runtime is added at publish");
        assert_eq!(a.template, "generated");
        let html = read_file_in(&c, &a.id, "index.html").expect("read");
        assert!(html.contains("<h1>My App</h1>"));

        let no_index = vec![("app.js".to_string(), "let a = 1;\n".to_string())];
        let e = create_with_files(&c, "No Index", "", &no_index)
            .unwrap_err()
            .to_string();
        assert!(e.contains("index.html"), "{e}");
        let generated = vec![
            ("index.html".to_string(), "<p>x</p>".to_string()),
            ("agentos.js".to_string(), "bad".to_string()),
        ];
        let e = create_with_files(&c, "Runtime", "", &generated)
            .unwrap_err()
            .to_string();
        assert!(e.contains("generated by Studio"), "{e}");
        let doubled = vec![
            ("index.html".to_string(), "<p>x</p>".to_string()),
            ("index.html".to_string(), "<p>y</p>".to_string()),
        ];
        let e = create_with_files(&c, "Doubled", "", &doubled)
            .unwrap_err()
            .to_string();
        assert!(e.contains("twice"), "{e}");
        set_test_root(None);
    }

    #[test]
    fn names_collide_without_overwriting() {
        let (_dir, c) = fixture();
        let first = seed(&c, "Same Name");
        let second = seed(&c, "Same Name");
        assert_ne!(first.slug, second.slug);
        assert_eq!(second.slug, "same-name-2");
        assert!(artifact_dir(&first.slug).is_dir());
        assert!(artifact_dir(&second.slug).is_dir());
        set_test_root(None);
    }

    #[test]
    fn read_and_write_are_bounded_to_the_artifact() {
        let (_dir, c) = fixture();
        let a = seed(&c, "Editor");
        let before = read_file_in(&c, &a.id, "index.html").expect("read");
        assert!(before.contains("Editor"));
        write_file_in(&c, &a.id, "index.html", "<h1>Changed</h1>").expect("write");
        assert_eq!(
            read_file_in(&c, &a.id, "index.html").expect("read back"),
            "<h1>Changed</h1>"
        );
        assert!(read_file_in(&c, &a.id, "../secret.txt").is_err());
        assert!(write_file_in(&c, &a.id, "../escape.html", "no").is_err());
        assert!(write_file_in(&c, &a.id, "agentos.js", "overwritten").is_err());
        set_test_root(None);
    }

    #[test]
    fn drafts_stay_out_of_the_published_set() {
        let (_dir, c) = fixture();
        let live = seed(&c, "Live");
        let draft = seed(&c, "Draft");
        c.execute(
            "UPDATE artifacts SET published_at = 1 WHERE id = ?1",
            [&live.id],
        )
        .expect("mark live");
        let rows = published_artifacts(&c, &draft.id).expect("rows");
        let slugs: Vec<&str> = rows.iter().map(|a| a.slug.as_str()).collect();
        assert!(slugs.contains(&live.slug.as_str()));
        assert!(slugs.contains(&draft.slug.as_str()));
        let rows = published_artifacts(&c, "missing").expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slug, live.slug);
        set_test_root(None);
    }

    #[test]
    fn delete_removes_files_and_the_row() {
        let (fixture_dir, c) = fixture();
        store::set_test_db(Some(fixture_dir.path().join("store.db")));
        let a = seed(&c, "Gone");
        let dir = artifact_dir(&a.slug);
        assert!(dir.is_dir());
        delete_in(&c, &a.id, true).expect("delete");
        assert!(!dir.exists());
        assert!(list_in(&c).expect("list").is_empty());
        assert!(get_in(&c, &a.id).is_err());
        set_test_root(None);
        store::set_test_db(None);
    }

    #[test]
    fn installed_ids_round_trip() {
        let (_dir, c) = fixture();
        let a = seed(&c, "App");
        set_installed(&c, &a.id, true).expect("install");
        assert_eq!(installed_ids(&c).expect("ids"), vec![a.id.clone()]);
        set_installed(&c, &a.id, false).expect("uninstall");
        assert!(installed_ids(&c).expect("ids").is_empty());
        set_test_root(None);
    }

    #[test]
    fn gallery_escapes_names_and_descriptions() {
        let (_dir, c) = fixture();
        let a = seed(&c, "<script>alert(1)</script>");
        let html = gallery_html(&[a]);
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("A test app"));
    }

    #[test]
    fn site_urls_cover_project_user_and_custom_domains() {
        assert_eq!(
            site_url("konethegreat", "agentos-artifacts", None),
            "https://konethegreat.github.io/agentos-artifacts"
        );
        assert_eq!(
            site_url("konethegreat", "konethegreat.github.io", None),
            "https://konethegreat.github.io"
        );
        assert_eq!(
            site_url(
                "konethegreat",
                "agentos-artifacts",
                Some("apps.example.com")
            ),
            "https://apps.example.com"
        );
        assert_eq!(
            artifact_url(
                "https://konethegreat.github.io/agentos-artifacts/",
                "my-app"
            ),
            "https://konethegreat.github.io/agentos-artifacts/a/my-app/"
        );
    }

    #[test]
    fn domains_are_plain_and_valid() {
        assert!(valid_domain("apps.example.com"));
        assert!(valid_domain("a-b.example.co.uk"));
        assert!(!valid_domain("https://example.com"));
        assert!(!valid_domain("-bad.example.com"));
        assert!(!valid_domain("localhost"));
        assert!(!valid_domain("bad..example.com"));
    }

    #[test]
    fn pages_workflow_keeps_its_yaml_indentation() {
        for block in [
            "on:\n  push:\n    branches: [main]\n",
            "permissions:\n  contents: read\n  pages: write\n  id-token: write\n",
            "jobs:\n  deploy:\n    environment:\n      name: github-pages\n",
            "    steps:\n      - uses: actions/checkout@v6\n",
            "        with:\n          path: .\n",
        ] {
            assert!(
                PAGES_WORKFLOW.contains(block),
                "missing indented block {block:?} in:\n{PAGES_WORKFLOW}"
            );
        }
    }

    #[test]
    fn manifest_lists_only_the_published_set() {
        let (_dir, c) = fixture();
        let a = seed(&c, "One");
        let manifest = manifest_json(std::slice::from_ref(&a), "owner/site");
        let items = manifest["artifacts"].as_array().expect("array");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["slug"], a.slug);
        assert_eq!(items[0]["path"], format!("a/{}/", a.slug));
        set_test_root(None);
    }
}
