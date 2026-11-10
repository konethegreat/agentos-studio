//! The capability fabric: a read-only index of every skill, plugin, agent, command and MCP
//! server on this machine, across Hermes, Qoder and Claude Code.
//!
//! Studio reads `SKILL.md` frontmatter, plugin manifests and agent files only, and writes nothing
//! into any source store. Each record carries where it came from, a content hash over its folder,
//! the capabilities it asks for (in the closed vocabulary of the trust pipeline), and a
//! compatibility report. Skills with the same name in several stores form one duplicate group
//! with a stated precedence: Hermes native, then Studio's bridge, then Qoder, then Claude Code,
//! then other local roots. Nothing is merged silently; every copy stays visible.
//!
//! Folders the scanner may not read (Hermes keeps its plugin folders ACL-denied) are skipped and
//! reported, never fatal. Symlinks are not followed and depth is capped.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

/// The closed capability vocabulary shared with the trust pipeline.
pub const CAPABILITIES: [&str; 8] = [
    "fs.read",
    "fs.write",
    "net.hosts",
    "shell.exec",
    "mcp.tools",
    "secrets.read",
    "hooks.exec",
    "model.use",
];

/// Source precedence for duplicate groups, best first.
pub const PRECEDENCE: [&str; 5] = ["hermes", "bridge", "qoder", "claude", "local"];

const MAX_DEPTH: usize = 9;
const HASH_MAX_FILES: usize = 2000;
const HASH_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// Frontmatter keys some runtime reads: the Agent Skills fields plus the ones Hermes uses.
const KNOWN_FIELDS: [&str; 15] = [
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
    "allowed-tools",
    "version",
    "author",
    "tags",
    "related_skills",
    "platforms",
    "prerequisites",
    "required_environment_variables",
    "required_commands",
    "category",
];

/// Where to look and what to expect there.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Root {
    pub source: String,
    /// `skills`, `plugins` (manifests with nested skills, agents and commands), `agents`,
    /// `hermes-plugins` (Hermes' install metadata only).
    pub layout: String,
    pub path: String,
    /// False for a marketplace clone: listed, not installed.
    pub installed: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Record {
    pub id: String,
    /// `skill`, `plugin`, `agent`, `command` or `mcp`.
    pub kind: String,
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub source: String,
    pub installed: bool,
    pub source_path: String,
    pub publisher: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    /// SHA-256 over the folder's relative paths and contents, or `too-large`.
    pub hash: String,
    pub capabilities: Vec<String>,
    pub compat: Vec<String>,
    /// The plugin a nested skill, agent or command belongs to.
    pub plugin: Option<String>,
    /// Duplicate group key (normalized name) for skills; empty otherwise.
    pub group: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Group {
    pub name: String,
    /// Member ids, best precedence first; the first one is the copy a runtime should prefer.
    pub members: Vec<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Index {
    pub records: Vec<Record>,
    /// source -> kind -> count
    pub counts: BTreeMap<String, BTreeMap<String, usize>>,
    pub groups: Vec<Group>,
    /// Roots or folders that could not be read, with the reason.
    pub skipped: Vec<String>,
    pub roots: Vec<Root>,
    pub scanned_at: i64,
}

fn root(source: &str, layout: &str, path: PathBuf, installed: bool) -> Root {
    Root {
        source: source.into(),
        layout: layout.into(),
        path: path.display().to_string(),
        installed,
    }
}

/// The three stores' default locations on this machine, plus Studio's bridge folder.
pub fn default_roots() -> Vec<Root> {
    let home = crate::settings::home();
    let hermes = crate::procs::hermes_home();
    let mut out = vec![
        root("hermes", "skills", hermes.join("skills"), true),
        root("bridge", "skills", hermes.join("skills-external"), true),
        root("hermes", "hermes-plugins", hermes.join("plugins"), true),
        root("qoder", "skills", home.join(".qoder").join("skills"), true),
        root(
            "qoder",
            "plugins",
            home.join(".qoder").join("plugins").join("cache"),
            true,
        ),
        root(
            "claude",
            "skills",
            home.join(".claude").join("skills"),
            true,
        ),
        root(
            "claude",
            "plugins",
            home.join(".claude").join("plugins").join("synced"),
            true,
        ),
        root(
            "claude",
            "plugins",
            home.join(".claude").join("plugins").join("marketplaces"),
            false,
        ),
        root(
            "claude",
            "agents",
            home.join(".claude").join("agents"),
            true,
        ),
    ];
    // Extra local roots the owner adds (kv `catalog.extra_roots`, a JSON list of folders).
    let extra = crate::store::open()
        .ok()
        .and_then(|c| {
            crate::store::kv_get(&c, "catalog.extra_roots")
                .ok()
                .flatten()
        })
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default();
    for p in extra {
        out.push(root("local", "skills", PathBuf::from(p), true));
    }
    out
}

/// A stable hash over a folder: relative paths in order, then each file's bytes. Pure over the
/// filesystem; symlinks are not followed.
pub fn hash_dir(dir: &Path) -> String {
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .follow_links(false)
        .max_depth(MAX_DEPTH)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    if files.len() > HASH_MAX_FILES {
        return "too-large".into();
    }
    files.sort();
    let mut h = Sha256::new();
    let mut total = 0u64;
    for f in files {
        let rel = f
            .strip_prefix(dir)
            .unwrap_or(&f)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(bytes) = fs::read(&f) else {
            continue;
        };
        total += bytes.len() as u64;
        if total > HASH_MAX_BYTES {
            return "too-large".into();
        }
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update(&bytes);
        h.update([0u8]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A cheap fingerprint of a folder from metadata only: relative paths, sizes and modification
/// times. When it matches the cached one, the cached content hash still holds. Pure over the
/// filesystem.
pub fn fingerprint(dir: &Path) -> String {
    let mut rows: Vec<String> = WalkDir::new(dir)
        .follow_links(false)
        .max_depth(MAX_DEPTH)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let rel = e
                .path()
                .strip_prefix(dir)
                .unwrap_or(e.path())
                .to_string_lossy()
                .replace('\\', "/");
            Some(format!("{rel}\0{}\0{t}", m.len()))
        })
        .collect();
    rows.sort();
    let mut h = Sha256::new();
    for r in rows {
        h.update(r.as_bytes());
        h.update([1u8]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Folder path -> (fingerprint, content hash), kept between scans so an unchanged folder is
/// never read twice.
pub type HashCache = BTreeMap<String, (String, String)>;

/// The content hash through the cache: reads file contents only when the folder's fingerprint
/// changed since the last scan.
pub fn cached_hash(dir: &Path, cache: &mut HashCache) -> String {
    let key = dir.display().to_string();
    let fp = fingerprint(dir);
    if let Some((f, h)) = cache.get(&key) {
        if *f == fp {
            return h.clone();
        }
    }
    let h = hash_dir(dir);
    cache.insert(key, (fp, h.clone()));
    h
}

fn load_cache(path: Option<&Path>) -> HashCache {
    path.and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_cache(path: Option<&Path>, cache: &HashCache) {
    if let Some(p) = path {
        if let Ok(s) = serde_json::to_string(cache) {
            let tmp = p.with_extension("json.tmp");
            if fs::write(&tmp, s).is_ok() {
                let _ = fs::rename(&tmp, p);
            }
        }
    }
}

/// Where Studio keeps the hash cache (its own data folder, never a source store).
pub fn cache_path() -> PathBuf {
    crate::store::studio_dir().join("catalog-hashes.json")
}

fn ystr(v: Option<&serde_yaml::Value>) -> Option<String> {
    match v? {
        serde_yaml::Value::String(s) => Some(s.trim().to_string()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

fn yseq(v: Option<&serde_yaml::Value>) -> Vec<String> {
    match v {
        Some(serde_yaml::Value::Sequence(s)) => s.iter().filter_map(|x| ystr(Some(x))).collect(),
        Some(serde_yaml::Value::String(s)) => s
            .split([',', ' '])
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Map a tool name a skill or agent asks for onto the capability vocabulary. Pure.
pub fn tool_capability(tool: &str) -> Option<&'static str> {
    let t = tool.trim().to_lowercase();
    let base = t.split('(').next().unwrap_or(&t);
    match base {
        "bash" | "powershell" | "shell" | "terminal" | "execute_code" => Some("shell.exec"),
        "write" | "edit" | "multiedit" | "notebookedit" | "write_file" | "patch" => {
            Some("fs.write")
        }
        "read" | "glob" | "grep" | "ls" | "read_file" | "search_files" => Some("fs.read"),
        "webfetch" | "websearch" | "web_extract" | "web_search" | "browser" => Some("net.hosts"),
        b if b.starts_with("mcp__") => Some("mcp.tools"),
        _ => None,
    }
}

fn refs_re() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?:^|[\s`(\[])((?:scripts|references|templates|assets)/[A-Za-z0-9_./\-]+)")
            .expect("refs pattern")
    })
}

fn abs_re() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?:\b[A-Za-z]:\\[A-Za-z]|/Users/[A-Za-z]|/home/[a-z])").expect("abs pattern")
    })
}

/// The compatibility report for one skill: unknown frontmatter fields, referenced files that do
/// not exist, absolute paths, and whether it has a description at all. Pure over `dir`.
pub fn compat_report(text: &str, dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    match crate::skills::frontmatter(text) {
        Some(serde_yaml::Value::Mapping(m)) => {
            let mut unknown: Vec<String> = m
                .keys()
                .filter_map(|k| k.as_str())
                .filter(|k| !KNOWN_FIELDS.contains(k))
                .map(String::from)
                .collect();
            unknown.sort();
            for k in unknown {
                out.push(format!("unknown-field:{k}"));
            }
            if ystr(m.get("description")).is_none() {
                out.push("no-description".into());
            }
        }
        _ => out.push("no-frontmatter".into()),
    }
    let mut missing: Vec<String> = refs_re()
        .captures_iter(text)
        .filter_map(|c| {
            c.get(1)
                .map(|m| m.as_str().trim_end_matches(['.', ',', ')']).to_string())
        })
        .filter(|rel| !dir.join(rel).exists())
        .collect();
    missing.sort();
    missing.dedup();
    for m in missing {
        out.push(format!("missing-file:{m}"));
    }
    if abs_re().is_match(text) {
        out.push("absolute-path".into());
    }
    out
}

fn skill_caps(fm: Option<&serde_yaml::Value>, dir: &Path) -> Vec<String> {
    let mut caps: Vec<String> = yseq(fm.and_then(|f| f.get("allowed-tools")))
        .iter()
        .filter_map(|t| tool_capability(t))
        .map(String::from)
        .collect();
    let has_scripts = WalkDir::new(dir.join("scripts"))
        .max_depth(3)
        .follow_links(false)
        .into_iter()
        .flatten()
        .any(|e| {
            e.file_type().is_file()
                && e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| matches!(x, "sh" | "py" | "ps1" | "js" | "ts" | "bat" | "cmd"))
        });
    if has_scripts {
        caps.push("shell.exec".into());
    }
    caps.sort();
    caps.dedup();
    caps
}

/// Normalized duplicate key for a skill name. Pure.
pub fn group_key(name: &str) -> String {
    name.trim().to_lowercase().replace(['_', ' '], "-")
}

fn skill_record(
    file: &Path,
    r: &Root,
    plugin: Option<&str>,
    cache: &mut HashCache,
) -> Option<Record> {
    let text = fs::read_to_string(file).ok()?;
    let dir = file.parent()?;
    let fm = crate::skills::frontmatter(&text);
    let name = ystr(fm.as_ref().and_then(|f| f.get("name"))).unwrap_or_else(|| {
        dir.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    let meta = fm.as_ref().and_then(|f| f.get("metadata"));
    Some(Record {
        id: format!("skill:{}:{}", r.source, dir.display()),
        kind: "skill".into(),
        description: ystr(fm.as_ref().and_then(|f| f.get("description"))).unwrap_or_default(),
        version: ystr(fm.as_ref().and_then(|f| f.get("version")))
            .or_else(|| ystr(meta.and_then(|m| m.get("version")))),
        source: r.source.clone(),
        installed: r.installed,
        source_path: file.display().to_string(),
        publisher: ystr(fm.as_ref().and_then(|f| f.get("author")))
            .or_else(|| ystr(meta.and_then(|m| m.get("author")))),
        license: ystr(fm.as_ref().and_then(|f| f.get("license"))),
        homepage: None,
        hash: cached_hash(dir, cache),
        capabilities: skill_caps(fm.as_ref(), dir),
        compat: compat_report(&text, dir),
        plugin: plugin.map(String::from),
        group: group_key(&name),
        name,
    })
}

fn jstr(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn plugin_record(manifest: &Path, r: &Root, cache: &mut HashCache) -> Option<(Record, PathBuf)> {
    let text = fs::read_to_string(manifest).ok()?;
    let j: Value = serde_json::from_str(&text).ok()?;
    let dir = manifest.parent()?.parent()?.to_path_buf();
    let name = jstr(&j, "name")?;
    let mut caps = Vec::new();
    if j.get("hooks").is_some() || dir.join("hooks").is_dir() {
        caps.push("hooks.exec".to_string());
    }
    if j.get("mcpServers").is_some() || dir.join(".mcp.json").is_file() {
        caps.push("mcp.tools".to_string());
    }
    let mut compat = Vec::new();
    if caps.iter().any(|c| c == "hooks.exec") {
        compat.push("hooks".into());
    }
    let publisher = match j.get("author") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(a) => jstr(a, "name"),
        None => None,
    };
    Some((
        Record {
            id: format!("plugin:{}:{}", r.source, dir.display()),
            kind: "plugin".into(),
            description: jstr(&j, "description").unwrap_or_default(),
            version: jstr(&j, "version"),
            source: r.source.clone(),
            installed: r.installed,
            source_path: manifest.display().to_string(),
            publisher,
            license: jstr(&j, "license"),
            homepage: jstr(&j, "homepage").or_else(|| jstr(&j, "repository")),
            hash: cached_hash(&dir, cache),
            capabilities: caps,
            compat,
            plugin: None,
            group: String::new(),
            name,
        },
        dir,
    ))
}

fn md_record(kind: &str, file: &Path, r: &Root, plugin: Option<&str>) -> Option<Record> {
    let text = fs::read_to_string(file).ok()?;
    let fm = crate::skills::frontmatter(&text);
    let stem = file.file_stem()?.to_string_lossy().to_string();
    let tools = yseq(fm.as_ref().and_then(|f| f.get("tools")));
    let mut caps: Vec<String> = tools
        .iter()
        .filter_map(|t| tool_capability(t))
        .map(String::from)
        .collect();
    if fm.as_ref().and_then(|f| f.get("model")).is_some() {
        caps.push("model.use".into());
    }
    caps.sort();
    caps.dedup();
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    Some(Record {
        id: format!("{kind}:{}:{}", r.source, file.display()),
        kind: kind.into(),
        name: ystr(fm.as_ref().and_then(|f| f.get("name"))).unwrap_or(stem),
        description: ystr(fm.as_ref().and_then(|f| f.get("description"))).unwrap_or_default(),
        version: None,
        source: r.source.clone(),
        installed: r.installed,
        source_path: file.display().to_string(),
        publisher: None,
        license: None,
        homepage: None,
        hash: h.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        capabilities: caps,
        compat: Vec::new(),
        plugin: plugin.map(String::from),
        group: String::new(),
    })
}

/// Hermes plugins: their folders are not readable, so only the install metadata is used.
fn hermes_plugins(r: &Root, out: &mut Vec<Record>, skipped: &mut Vec<String>) {
    let meta = Path::new(&r.path).join(".install-metadata.json");
    let Ok(text) = fs::read_to_string(&meta) else {
        skipped.push(format!("{}: no readable install metadata", r.path));
        return;
    };
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) else {
        skipped.push(format!(
            "{}: install metadata is not an object",
            meta.display()
        ));
        return;
    };
    for (name, v) in m {
        let cat = v.get("catalog").cloned().unwrap_or(Value::Null);
        let mut h = Sha256::new();
        h.update(v.to_string().as_bytes());
        out.push(Record {
            id: format!("plugin:hermes:{name}"),
            kind: "plugin".into(),
            description: String::new(),
            version: jstr(&v, "revision").map(|s| s.chars().take(12).collect()),
            source: "hermes".into(),
            installed: true,
            source_path: meta.display().to_string(),
            publisher: jstr(&cat, "tier")
                .map(|t| format!("{t} ({})", jstr(&cat, "repo").unwrap_or_default())),
            license: None,
            homepage: jstr(&cat, "repo").or_else(|| jstr(&v, "source")),
            hash: h.finalize().iter().map(|b| format!("{b:02x}")).collect(),
            capabilities: Vec::new(),
            compat: vec!["metadata-only".into()],
            plugin: None,
            group: String::new(),
            name,
        });
    }
}

/// `follow` is set only for Studio's bridge folder, whose entries are links to skill folders by
/// design; walkdir reports a link loop as an error, which lands in `skipped`.
fn walk(dir: &Path, follow: bool, skipped: &mut Vec<String>) -> Vec<walkdir::DirEntry> {
    let mut out = Vec::new();
    for e in WalkDir::new(dir).follow_links(follow).max_depth(MAX_DEPTH) {
        match e {
            Ok(e) => out.push(e),
            Err(err) => {
                let at = err
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| dir.display().to_string());
                skipped.push(format!(
                    "{at}: {}",
                    err.io_error()
                        .map(|i| i.to_string())
                        .unwrap_or_else(|| err.to_string())
                ));
            }
        }
    }
    out
}

fn is_named(e: &walkdir::DirEntry, name: &str) -> bool {
    e.file_type().is_file() && e.file_name().to_string_lossy() == name
}

fn under(e: &walkdir::DirEntry, folder: &str) -> bool {
    e.path()
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n.to_string_lossy() == folder)
}

/// Scan one root into records. Unreadable roots and folders are reported in `skipped`.
pub fn scan_root(
    r: &Root,
    out: &mut Vec<Record>,
    skipped: &mut Vec<String>,
    cache: &mut HashCache,
) {
    let base = PathBuf::from(&r.path);
    if r.layout == "hermes-plugins" {
        hermes_plugins(r, out, skipped);
        return;
    }
    if !base.is_dir() {
        skipped.push(format!("{}: not a folder on this machine", r.path));
        return;
    }
    // Deleted items some apps keep beside their skills (Claude Code's `.trash`) are not skills.
    let entries: Vec<walkdir::DirEntry> = walk(&base, r.source == "bridge", skipped)
        .into_iter()
        .filter(|e| {
            !e.path()
                .strip_prefix(&base)
                .unwrap_or(e.path())
                .components()
                .any(|c| c.as_os_str() == ".trash" || c.as_os_str() == "node_modules")
        })
        .collect();
    let md = |e: &walkdir::DirEntry| {
        e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "md")
    };
    match r.layout.as_str() {
        "skills" => {
            for e in entries.iter().filter(|e| is_named(e, "SKILL.md")) {
                if let Some(rec) = skill_record(e.path(), r, None, cache) {
                    out.push(rec);
                }
            }
        }
        "agents" => {
            for e in entries.iter().filter(|e| md(e)) {
                if let Some(rec) = md_record("agent", e.path(), r, None) {
                    out.push(rec);
                }
            }
        }
        _ => {
            // Plugins: each manifest owns everything under its folder.
            let manifests: Vec<PathBuf> = entries
                .iter()
                .filter(|e| {
                    is_named(e, "plugin.json")
                        && e.path()
                            .parent()
                            .and_then(|p| p.file_name())
                            .is_some_and(|n| n == ".claude-plugin" || n == ".qoder-plugin")
                })
                .map(|e| e.path().to_path_buf())
                .collect();
            let mut plugins: Vec<(String, PathBuf)> = Vec::new();
            for m in manifests {
                if let Some((rec, dir)) = plugin_record(&m, r, cache) {
                    plugins.push((rec.name.clone(), dir));
                    out.push(rec);
                }
            }
            // The deepest plugin folder that contains a path owns it.
            plugins.sort_by_key(|(_, d)| std::cmp::Reverse(d.components().count()));
            let owner = |p: &Path| {
                plugins
                    .iter()
                    .find(|(_, d)| p.starts_with(d))
                    .map(|(n, _)| n.as_str())
            };
            for e in &entries {
                let Some(name) = owner(e.path()) else {
                    continue;
                };
                if is_named(e, "SKILL.md") {
                    if let Some(rec) = skill_record(e.path(), r, Some(name), cache) {
                        out.push(rec);
                    }
                } else if md(e) && under(e, "agents") {
                    if let Some(rec) = md_record("agent", e.path(), r, Some(name)) {
                        out.push(rec);
                    }
                } else if md(e) && under(e, "commands") {
                    if let Some(rec) = md_record("command", e.path(), r, Some(name)) {
                        out.push(rec);
                    }
                }
            }
        }
    }
}

/// MCP servers Studio knows how to add, from the registry reader (`mcp_manager::catalog_entries`,
/// metadata only). Secret parameter names come from the install specs: names, never values.
fn mcp_records(vault: &Path) -> Vec<Record> {
    let secrets: BTreeMap<String, Vec<String>> = crate::mcp_manager::catalog(vault)
        .into_iter()
        .map(|c| {
            (
                c.id.to_string(),
                c.params
                    .iter()
                    .filter(|p| p.secret)
                    .map(|p| format!("needs-secret:{}", p.key))
                    .collect(),
            )
        })
        .collect();
    crate::mcp_manager::catalog_entries(vault)
        .into_iter()
        .map(|c| {
            let mut h = Sha256::new();
            h.update(c.id.as_bytes());
            h.update(c.transport.as_bytes());
            h.update(c.package.as_bytes());
            let nonempty = |s: String| Some(s).filter(|x| !x.trim().is_empty());
            Record {
                id: format!("mcp:studio:{}", c.id),
                kind: "mcp".into(),
                compat: secrets.get(&c.id).cloned().unwrap_or_default(),
                name: c.name,
                description: c.description,
                version: None,
                source: "local".into(),
                installed: false,
                source_path: format!(
                    "AgentOS Studio MCP registry ({}, {})",
                    c.transport, c.trust_tier
                ),
                publisher: nonempty(c.publisher),
                license: nonempty(c.license),
                homepage: nonempty(c.homepage),
                hash: h.finalize().iter().map(|b| format!("{b:02x}")).collect(),
                capabilities: vec!["mcp.tools".into()],
                plugin: None,
                group: String::new(),
            }
        })
        .collect()
}

fn rank(source: &str) -> usize {
    PRECEDENCE
        .iter()
        .position(|s| *s == source)
        .unwrap_or(PRECEDENCE.len())
}

/// Duplicate groups over installed skills, members best precedence first, then by path so the
/// order is the same on every scan. Pure.
pub fn groups(records: &[Record]) -> Vec<Group> {
    let mut by: BTreeMap<String, Vec<&Record>> = BTreeMap::new();
    for r in records
        .iter()
        .filter(|r| r.kind == "skill" && r.installed && !r.group.is_empty())
    {
        by.entry(r.group.clone()).or_default().push(r);
    }
    by.into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(name, mut v)| {
            v.sort_by(|a, b| {
                rank(&a.source)
                    .cmp(&rank(&b.source))
                    .then_with(|| a.source_path.cmp(&b.source_path))
            });
            Group {
                name,
                members: v.iter().map(|r| r.id.clone()).collect(),
            }
        })
        .collect()
}

/// Index the given roots (plus the MCP catalog when a vault is given). Never writes to a
/// source store; the only file written is the hash cache at `cache`, when one is given.
pub fn index(roots: &[Root], vault: Option<&Path>, cache: Option<&Path>) -> Index {
    let mut records = Vec::new();
    let mut skipped = Vec::new();
    let mut hashes = load_cache(cache);
    let before = hashes.len();
    for r in roots {
        scan_root(r, &mut records, &mut skipped, &mut hashes);
    }
    // Drop folders that no longer exist so the cache does not grow without bound.
    hashes.retain(|k, _| Path::new(k).is_dir());
    if hashes.len() != before || cache.is_some() {
        save_cache(cache, &hashes);
    }
    if let Some(v) = vault {
        records.extend(mcp_records(v));
    }
    records.sort_by(|a, b| a.id.cmp(&b.id));
    records.dedup_by(|a, b| a.id == b.id);
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for r in &records {
        *counts
            .entry(r.source.clone())
            .or_default()
            .entry(r.kind.clone())
            .or_default() += 1;
    }
    Index {
        groups: groups(&records),
        records,
        counts,
        skipped,
        roots: roots.to_vec(),
        scanned_at: chrono::Utc::now().timestamp(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    fn skill(dir: &Path, name: &str, extra: &str) {
        write(
            &dir.join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: Does {name}\n{extra}---\n\n# {name}\n"),
        );
    }

    fn r(source: &str, layout: &str, p: &Path) -> Root {
        root(source, layout, p.to_path_buf(), true)
    }

    #[test]
    fn missing_and_unreadable_roots_are_reported_not_fatal() {
        let t = tempfile::tempdir().unwrap();
        let file_root = t.path().join("not-a-dir");
        write(&file_root, "x");
        skill(&t.path().join("ok").join("alpha"), "alpha", "");
        let idx = index(
            &[
                r("hermes", "skills", &t.path().join("nope")),
                r("qoder", "skills", &file_root),
                r("hermes", "hermes-plugins", &t.path().join("plugins")),
                r("claude", "skills", &t.path().join("ok")),
            ],
            None,
            None,
        );
        assert_eq!(idx.records.len(), 1);
        assert_eq!(idx.skipped.len(), 3, "{:?}", idx.skipped);
        assert_eq!(idx.counts["claude"]["skill"], 1);
    }

    #[test]
    fn duplicates_group_by_precedence_whatever_the_scan_order() {
        let t = tempfile::tempdir().unwrap();
        for s in ["claude", "qoder", "hermes", "bridge"] {
            skill(&t.path().join(s).join("Code_Review"), "Code_Review", "");
        }
        skill(&t.path().join("qoder").join("solo"), "solo", "");
        let mut roots: Vec<Root> = ["claude", "qoder", "hermes", "bridge"]
            .iter()
            .map(|s| r(s, "skills", &t.path().join(s)))
            .collect();
        let a = index(&roots, None, None);
        roots.reverse();
        let b = index(&roots, None, None);
        assert_eq!(a.groups, b.groups, "stable");
        assert_eq!(a.groups.len(), 1);
        let g = &a.groups[0];
        assert_eq!(g.name, "code-review");
        let sources: Vec<&str> = g
            .members
            .iter()
            .map(|id| id.split(':').nth(1).unwrap())
            .collect();
        assert_eq!(sources, ["hermes", "bridge", "qoder", "claude"]);
    }

    #[test]
    fn the_hash_changes_when_any_file_in_the_folder_changes() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("s");
        skill(&d, "s", "");
        write(&d.join("references").join("a.md"), "one");
        let h1 = hash_dir(&d);
        assert_eq!(h1, hash_dir(&d), "stable");
        write(&d.join("references").join("a.md"), "two");
        let h2 = hash_dir(&d);
        assert_ne!(h1, h2);
        write(&d.join("scripts").join("new.sh"), "echo");
        assert_ne!(h2, hash_dir(&d), "a new file changes it too");
    }

    #[test]
    fn compatibility_flags_missing_scripts_unknown_fields_and_absolute_paths() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("s");
        write(&d.join("scripts").join("run.sh"), "echo");
        let text = "---\nname: s\ndescription: d\nallowed-tools: Bash(git *), Read\nwhen_to_use: x\n---\n\nRun `scripts/run.sh` then `scripts/gone.py`, see references/guide.md and C:\\Users\\x\\file.\n";
        write(&d.join("SKILL.md"), text);
        let c = compat_report(text, &d);
        assert!(
            c.contains(&"missing-file:scripts/gone.py".to_string()),
            "{c:?}"
        );
        assert!(c.contains(&"missing-file:references/guide.md".to_string()));
        assert!(!c.iter().any(|x| x.contains("run.sh")));
        assert!(c.contains(&"unknown-field:when_to_use".to_string()));
        assert!(c.contains(&"absolute-path".to_string()));
        let rec = skill_record(
            &d.join("SKILL.md"),
            &r("qoder", "skills", t.path()),
            None,
            &mut HashCache::new(),
        )
        .unwrap();
        assert_eq!(
            rec.capabilities,
            vec!["fs.read".to_string(), "shell.exec".to_string()]
        );
        assert!(compat_report("no frontmatter", &d).contains(&"no-frontmatter".to_string()));
    }

    #[test]
    fn plugins_own_their_nested_skills_agents_and_commands() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("mkt").join("tools").join("1.0.0");
        write(
            &p.join(".qoder-plugin").join("plugin.json"),
            r#"{"name":"tools","version":"1.0.0","description":"d","author":{"name":"ACME"},"license":"MIT","hooks":"./hooks/hooks.json"}"#,
        );
        skill(&p.join("skills").join("lint"), "lint", "");
        write(
            &p.join("agents").join("reviewer.md"),
            "---\nname: reviewer\ndescription: r\ntools: Read, Grep\nmodel: sonnet\n---\nbody",
        );
        write(
            &p.join("commands").join("fix.md"),
            "---\ndescription: f\n---\nbody",
        );
        let idx = index(&[r("qoder", "plugins", &t.path().join("mkt"))], None, None);
        let kinds: Vec<(&str, &str)> = idx
            .records
            .iter()
            .map(|x| (x.kind.as_str(), x.name.as_str()))
            .collect();
        assert!(kinds.contains(&("plugin", "tools")));
        assert!(kinds.contains(&("skill", "lint")));
        assert!(kinds.contains(&("agent", "reviewer")));
        assert!(kinds.contains(&("command", "fix")));
        let plugin = idx.records.iter().find(|x| x.kind == "plugin").unwrap();
        assert_eq!(plugin.publisher.as_deref(), Some("ACME"));
        assert!(plugin.capabilities.contains(&"hooks.exec".to_string()));
        let agent = idx.records.iter().find(|x| x.kind == "agent").unwrap();
        assert_eq!(agent.plugin.as_deref(), Some("tools"));
        assert_eq!(
            agent.capabilities,
            vec!["fs.read".to_string(), "model.use".to_string()]
        );
    }

    #[test]
    fn the_cache_skips_unchanged_folders_and_notices_changes() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("s");
        skill(&d, "s", "");
        let mut c = HashCache::new();
        let h1 = cached_hash(&d, &mut c);
        assert_eq!(h1, hash_dir(&d));
        // A planted entry with the right fingerprint is trusted: proof the contents were not read.
        let key = d.display().to_string();
        c.insert(key.clone(), (fingerprint(&d), "planted".into()));
        assert_eq!(cached_hash(&d, &mut c), "planted");
        write(&d.join("references").join("new.md"), "x");
        let h2 = cached_hash(&d, &mut c);
        assert_ne!(h2, "planted");
        assert_eq!(h2, hash_dir(&d));
    }

    #[test]
    fn the_only_file_an_index_writes_is_its_own_cache() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        skill(&src.join("a"), "a", "");
        let before = hash_dir(&src);
        let cache = t.path().join("cache.json");
        let idx = index(&[r("hermes", "skills", &src)], None, Some(&cache));
        assert_eq!(before, hash_dir(&src));
        assert!(cache.is_file());
        let again = index(&[r("hermes", "skills", &src)], None, Some(&cache));
        assert_eq!(idx.records, again.records);
    }

    #[test]
    fn scanning_writes_nothing() {
        let t = tempfile::tempdir().unwrap();
        skill(&t.path().join("a"), "a", "");
        let before = hash_dir(t.path());
        let _ = index(&[r("hermes", "skills", t.path())], None, None);
        assert_eq!(before, hash_dir(t.path()));
    }

    #[test]
    fn tool_names_map_onto_the_closed_vocabulary() {
        for (t, c) in [
            ("Bash(git *)", Some("shell.exec")),
            ("PowerShell", Some("shell.exec")),
            ("Edit", Some("fs.write")),
            ("Read", Some("fs.read")),
            ("WebFetch", Some("net.hosts")),
            ("mcp__agentos-hub__recall", Some("mcp.tools")),
            ("TodoWrite", None),
        ] {
            assert_eq!(tool_capability(t), c, "{t}");
            if let Some(c) = c {
                assert!(CAPABILITIES.contains(&c));
            }
        }
    }

    /// The real machine: counts per store, groups, and what could not be read.
    /// Run: cargo test --lib catalog::tests::this_machine -- --ignored --nocapture
    #[test]
    #[ignore = "reads the real skill stores"]
    fn this_machine() {
        let t = std::time::Instant::now();
        let idx = index(&default_roots(), None, Some(&cache_path()));
        println!("first pass {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let again = index(&default_roots(), None, Some(&cache_path()));
        println!("second pass {:?}", t.elapsed());
        assert_eq!(idx.records.len(), again.records.len());
        println!("{} records", idx.records.len());
        for (s, k) in &idx.counts {
            println!("{s}: {k:?}");
        }
        println!("{} duplicate groups", idx.groups.len());
        for g in idx.groups.iter().take(10) {
            println!("  {}: {}", g.name, g.members.len());
        }
        for s in idx.skipped.iter().take(10) {
            println!("skipped {s}");
        }
    }
}
