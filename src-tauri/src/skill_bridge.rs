//! The skill bridge: Qoder, Claude Code and pack skills made readable to Hermes, without copying
//! them into Hermes' own tree and without touching the source stores.
//!
//! Studio keeps one folder, `hermes/skills-external` by default, with one entry per bridged skill
//! named `<source>--<skill>`. Each entry is a directory junction (a symlink off Windows) to the
//! skill's own folder; when a link cannot be made (another volume), it is a copy whose hash must
//! equal the source. Hermes reads the folder through `skills.external_dirs`, which it treats as
//! read-only, and its own skills win when names collide. Studio refuses a bridge that would be
//! shadowed anyway, a skill whose folder escapes itself through a link, and a skill that refers to
//! scripts that do not exist. Provenance (source, original path, license, author, hash) is kept in
//! `.agentos-bridge.json` in the bridge folder, never in the bridged skill.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

pub const LEDGER: &str = ".agentos-bridge.json";
pub const ACTION: &str = "hermes.config.skills_external";

#[derive(Clone, Debug)]
pub struct Bridge {
    /// The folder Studio manages and Hermes reads.
    pub dir: PathBuf,
    /// Hermes' own skills, which always win.
    pub native: PathBuf,
}

impl Bridge {
    pub fn live() -> Bridge {
        let h = crate::procs::hermes_home();
        Bridge {
            dir: h.join("skills-external"),
            native: h.join("skills"),
        }
    }
    fn ledger_path(&self) -> PathBuf {
        self.dir.join(LEDGER)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub folder: String,
    pub name: String,
    pub source: String,
    pub source_path: String,
    pub license: Option<String>,
    pub author: Option<String>,
    pub hash: String,
    /// `junction` (a link to the source folder) or `copy`.
    pub mode: String,
    pub added_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Ledger {
    pub entries: BTreeMap<String, Entry>,
}

fn read_ledger(b: &Bridge) -> Result<Ledger> {
    match fs::read_to_string(b.ledger_path()) {
        Ok(s) => Ok(serde_json::from_str(&s).context(LEDGER)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
        Err(e) => Err(e.into()),
    }
}

fn write_ledger(b: &Bridge, l: &Ledger) -> Result<()> {
    fs::create_dir_all(&b.dir)?;
    let tmp = b.dir.join(format!("{LEDGER}.tmp"));
    fs::write(&tmp, serde_json::to_string_pretty(l)? + "\n")?;
    fs::rename(&tmp, b.ledger_path())?;
    Ok(())
}

/// `<source>--<name>`, lower case, only letters, digits, dots, underscores and hyphens. Pure.
pub fn folder_name(source: &str, name: &str) -> String {
    let clean = |s: &str| -> String {
        s.trim()
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string()
    };
    format!("{}--{}", clean(source), clean(name))
}

fn fm_str(fm: &Option<serde_yaml::Value>, key: &str) -> Option<String> {
    let v = fm.as_ref()?.get(key)?;
    match v {
        serde_yaml::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

/// Names Hermes already has natively: frontmatter names and folder names under its skills tree.
pub fn native_names(native: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in WalkDir::new(native)
        .follow_links(false)
        .max_depth(6)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file() && e.file_name() == "SKILL.md")
    {
        let fm = fs::read_to_string(e.path())
            .ok()
            .and_then(|t| crate::skills::frontmatter(&t));
        if let Some(n) = fm_str(&fm, "name") {
            out.push(crate::catalog::group_key(&n));
        }
        if let Some(d) = e.path().parent().and_then(|p| p.file_name()) {
            out.push(crate::catalog::group_key(&d.to_string_lossy()));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A symlink or, on Windows, any reparse point (a junction included).
fn is_link(p: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(p) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// What a skill folder must pass before it is bridged. Pure over the filesystem.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    pub license: Option<String>,
    pub author: Option<String>,
    pub hash: String,
    pub problems: Vec<String>,
}

pub fn check(skill_dir: &Path, b: &Bridge) -> Result<Check> {
    let skill_md = skill_dir.join("SKILL.md");
    let text = fs::read_to_string(&skill_md)
        .with_context(|| format!("{} has no SKILL.md", skill_dir.display()))?;
    let fm = crate::skills::frontmatter(&text);
    let name = fm_str(&fm, "name").unwrap_or_else(|| {
        skill_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    let meta = fm.as_ref().and_then(|f| f.get("metadata")).cloned();
    let author = fm_str(&fm, "author").or_else(|| fm_str(&meta, "author"));
    let mut problems = Vec::new();
    let base = fs::canonicalize(skill_dir)?;
    for e in WalkDir::new(skill_dir).follow_links(false).max_depth(12) {
        let e = e?;
        if e.depth() == 0 || !is_link(e.path()) {
            continue;
        }
        let rel = e
            .path()
            .strip_prefix(skill_dir)
            .unwrap_or(e.path())
            .display()
            .to_string();
        match fs::canonicalize(e.path()) {
            Ok(real) if real.starts_with(&base) => {}
            Ok(real) => problems.push(format!(
                "{rel} links outside the skill, to {}",
                real.display()
            )),
            Err(_) => problems.push(format!("{rel} is a broken link")),
        }
    }
    for c in crate::catalog::compat_report(&text, skill_dir) {
        if let Some(missing) = c.strip_prefix("missing-file:") {
            if missing.starts_with("scripts/") {
                problems.push(format!("it runs {missing}, which does not exist"));
            }
        }
    }
    let key = crate::catalog::group_key(&name);
    if native_names(&b.native).contains(&key) {
        problems.push(format!(
            "Hermes has its own skill named {name}; the native one always wins"
        ));
    }
    if let Some(other) = read_ledger(b)?
        .entries
        .values()
        .find(|e| crate::catalog::group_key(&e.name) == key)
    {
        problems.push(format!(
            "{name} is already bridged from {} ({})",
            other.source, other.folder
        ));
    }
    Ok(Check {
        hash: crate::catalog::hash_dir(skill_dir),
        name,
        license: fm_str(&fm, "license"),
        author,
        problems,
    })
}

/// Drop the `\\?\` verbatim prefix `canonicalize` adds on Windows, so stored and shown paths
/// are the ones a person would type. Pure.
pub fn plain(p: PathBuf) -> PathBuf {
    let s = p.display().to_string();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}

fn link_dir(target: &Path, link: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let out = crate::procs::command(Path::new("cmd"))
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .context("mklink")?;
        if !out.status.success() {
            bail!(
                "mklink /J failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link).context("symlink")
    }
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    for e in WalkDir::new(src).follow_links(false) {
        let e = e?;
        let rel = e.path().strip_prefix(src).unwrap_or(e.path());
        let to = dst.join(rel);
        if e.file_type().is_dir() {
            fs::create_dir_all(&to)?;
        } else if e.file_type().is_file() {
            fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

/// Remove a bridge entry without ever following it into the source: a link is removed as a
/// link, a copy as a folder.
fn remove_entry(path: &Path) -> Result<()> {
    if !path.exists() && fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    if is_link(path) {
        // A junction or directory symlink: remove_dir deletes the link, not its target.
        fs::remove_dir(path)
            .or_else(|_| fs::remove_file(path))
            .with_context(|| format!("remove link {}", path.display()))
    } else {
        fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))
    }
}

/// Bridge one skill folder. Refuses anything `check` reports.
pub fn add(skill_dir: &Path, source: &str, b: &Bridge) -> Result<Entry> {
    let c = check(skill_dir, b)?;
    if !c.problems.is_empty() {
        bail!("not bridged: {}", c.problems.join("; "));
    }
    fs::create_dir_all(&b.dir)?;
    let folder = folder_name(source, &c.name);
    let link = b.dir.join(&folder);
    if fs::symlink_metadata(&link).is_ok() {
        bail!("{folder} already exists in the bridge folder");
    }
    let target = plain(fs::canonicalize(skill_dir)?);
    let mode = match link_dir(&target, &link) {
        Ok(()) => "junction",
        Err(_) => {
            copy_dir(&target, &link)?;
            if crate::catalog::hash_dir(&link) != c.hash {
                let _ = fs::remove_dir_all(&link);
                bail!("the copy of {} does not match its source", c.name);
            }
            "copy"
        }
    };
    let entry = Entry {
        folder: folder.clone(),
        name: c.name,
        source: source.to_string(),
        source_path: target.display().to_string(),
        license: c.license,
        author: c.author,
        hash: c.hash,
        mode: mode.into(),
        added_at: chrono::Utc::now().timestamp(),
    };
    let mut l = read_ledger(b)?;
    l.entries.insert(folder, entry.clone());
    write_ledger(b, &l)?;
    Ok(entry)
}

/// Take one skill out of the bridge. The source folder is never touched.
pub fn remove(folder: &str, b: &Bridge) -> Result<Entry> {
    let mut l = read_ledger(b)?;
    let e = l
        .entries
        .remove(folder)
        .ok_or_else(|| anyhow!("{folder} is not bridged"))?;
    remove_entry(&b.dir.join(folder))?;
    write_ledger(b, &l)?;
    Ok(e)
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Status {
    pub entry: Entry,
    /// `ok`, or what is wrong; a broken bridge is reported, never hidden.
    pub state: String,
}

pub fn status(b: &Bridge) -> Result<Vec<Status>> {
    let l = read_ledger(b)?;
    let mut out: Vec<Status> = l
        .entries
        .values()
        .map(|e| {
            let p = b.dir.join(&e.folder);
            let state = if fs::symlink_metadata(&p).is_err() {
                "missing: the bridge entry is gone".to_string()
            } else if !p.join("SKILL.md").is_file() {
                if e.mode == "junction" {
                    format!("broken: {} no longer exists", e.source_path)
                } else {
                    "broken: the copy has no SKILL.md".to_string()
                }
            } else if e.mode == "copy"
                && crate::catalog::hash_dir(Path::new(&e.source_path)) != e.hash
            {
                "stale: the source changed since it was copied".to_string()
            } else {
                "ok".to_string()
            };
            Status {
                entry: e.clone(),
                state,
            }
        })
        .collect();
    // Folders in the bridge that the ledger does not know are reported too.
    if let Ok(rd) = fs::read_dir(&b.dir) {
        for d in rd.flatten() {
            let name = d.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || l.entries.contains_key(&name) {
                continue;
            }
            out.push(Status {
                entry: Entry {
                    folder: name.clone(),
                    name,
                    source: "unknown".into(),
                    source_path: String::new(),
                    license: None,
                    author: None,
                    hash: String::new(),
                    mode: "unknown".into(),
                    added_at: 0,
                },
                state: "unmanaged: not added by Studio".into(),
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Hermes config
// ---------------------------------------------------------------------------------------------

/// The YAML block Hermes needs, for a copy-paste apply. Pure.
pub fn config_snippet(b: &Bridge) -> String {
    format!(
        "skills:\n  external_dirs:\n    - {}\n",
        b.dir.display().to_string().replace('\\', "/")
    )
}

fn hermes(args: &[&str]) -> Result<String> {
    let exe = crate::procs::find("hermes").ok_or_else(|| anyhow!("hermes is not installed"))?;
    let out = crate::procs::command(&exe)
        .args(args)
        .output()
        .context("hermes")?;
    if !out.status.success() {
        bail!(
            "hermes {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Parse `hermes config get skills.external_dirs` output: a JSON or YAML flow list. Pure.
pub fn parse_dirs(out: &str) -> Vec<String> {
    let t = out.trim();
    if let Ok(v) = serde_json::from_str::<Vec<String>>(t) {
        return v;
    }
    // A block list, possibly indented the way `hermes config get` prints it.
    let lines: Vec<&str> = t.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if !lines.is_empty() && lines.iter().all(|l| l.starts_with("- ")) {
        return lines
            .iter()
            .map(|l| l[2..].trim().trim_matches(['"', '\'']).to_string())
            .collect();
    }
    if let Ok(serde_yaml::Value::Sequence(s)) = serde_yaml::from_str::<serde_yaml::Value>(t) {
        return s
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
    }
    Vec::new()
}

fn same_dir(a: &str, b: &Path) -> bool {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_lowercase();
    norm(a) == norm(&b.display().to_string())
}

#[derive(Serialize, Clone, Debug)]
pub struct ConfigState {
    pub dirs: Vec<String>,
    pub applied: bool,
    pub snippet: String,
}

pub fn config_state(b: &Bridge) -> Result<ConfigState> {
    let dirs = parse_dirs(&hermes(&["config", "get", "skills.external_dirs"])?);
    Ok(ConfigState {
        applied: dirs.iter().any(|d| same_dir(d, &b.dir)),
        dirs,
        snippet: config_snippet(b),
    })
}

/// Add or remove the bridge folder in Hermes' `skills.external_dirs`, keeping every other entry.
/// Backs up `config.yaml` first and runs under the `hermes.config.skills_external` guard.
pub fn set_config(b: &Bridge, enable: bool, approved: bool) -> Result<ConfigState> {
    let summary = if enable {
        format!("Add {} to Hermes skills.external_dirs", b.dir.display())
    } else {
        format!(
            "Remove {} from Hermes skills.external_dirs",
            b.dir.display()
        )
    };
    let approved = approved && std::env::var_os(crate::permissions::UNATTENDED).is_none();
    crate::permissions::run_guarded(ACTION, "hermes config.yaml", &summary, approved, || {
        let cfg = crate::procs::hermes_home().join("config.yaml");
        let backups = crate::store::studio_dir()
            .join("backups")
            .join("hermes-config");
        fs::create_dir_all(&backups)?;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        fs::copy(&cfg, backups.join(format!("config-{stamp}.yaml")))
            .context("back up config.yaml")?;
        let mut dirs = parse_dirs(&hermes(&["config", "get", "skills.external_dirs"])?);
        dirs.retain(|d| !same_dir(d, &b.dir));
        if enable {
            fs::create_dir_all(&b.dir)?;
            dirs.push(b.dir.display().to_string().replace('\\', "/"));
        }
        hermes(&[
            "config",
            "set",
            "skills.external_dirs",
            &serde_json::to_string(&dirs)?,
        ])?;
        let after = config_state(b)?;
        if after.applied != enable {
            bail!(
                "Hermes reports skills.external_dirs = {:?} after the change",
                after.dirs
            );
        }
        Ok(after)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    fn skill(dir: &Path, name: &str, body: &str) {
        write(
            &dir.join("SKILL.md"),
            &format!(
                "---\nname: {name}\ndescription: d\nlicense: MIT\nauthor: Someone\n---\n{body}\n"
            ),
        );
    }

    fn setup() -> (tempfile::TempDir, Bridge) {
        let t = tempfile::tempdir().unwrap();
        let b = Bridge {
            dir: t.path().join("hermes").join("skills-external"),
            native: t.path().join("hermes").join("skills"),
        };
        fs::create_dir_all(&b.native).unwrap();
        (t, b)
    }

    #[test]
    fn bridge_prefers_native_over_external() {
        let (t, b) = setup();
        skill(&b.native.join("devops").join("deploy"), "deploy", "native");
        let ext = t.path().join("qoder").join("deploy");
        skill(&ext, "Deploy", "external");
        let e = add(&ext, "qoder", &b).unwrap_err().to_string();
        assert!(e.contains("native one always wins"), "{e}");
        assert!(!b.dir.join("qoder--deploy").exists());
        // The catalog orders the native copy first as well.
        assert_eq!(crate::catalog::PRECEDENCE[0], "hermes");
        assert_eq!(crate::catalog::PRECEDENCE[1], "bridge");
    }

    #[test]
    fn bridge_refuses_path_escape() {
        let (t, b) = setup();
        let outside = t.path().join("secret");
        write(&outside.join("token.txt"), "x");
        let s = t.path().join("claude").join("sneaky");
        skill(&s, "sneaky", "see refs");
        link_dir(&outside, &s.join("refs")).unwrap();
        let e = add(&s, "claude", &b).unwrap_err().to_string();
        assert!(e.contains("links outside the skill"), "{e}");
        let m = t.path().join("claude").join("missing-script");
        skill(&m, "missing-script", "Run `scripts/go.sh` first.");
        let e2 = add(&m, "claude", &b).unwrap_err().to_string();
        assert!(e2.contains("scripts/go.sh, which does not exist"), "{e2}");
    }

    #[test]
    fn bridge_reports_broken_junction() {
        let (t, b) = setup();
        let s = t.path().join("qoder").join("temp-skill");
        skill(&s, "temp-skill", "x");
        let e = add(&s, "qoder", &b).unwrap();
        assert_eq!(e.mode, "junction");
        assert_eq!(status(&b).unwrap()[0].state, "ok");
        fs::remove_dir_all(&s).unwrap();
        let st = status(&b).unwrap();
        assert!(st[0].state.starts_with("broken"), "{:?}", st);
        // Removing a broken entry works and leaves nothing behind.
        remove(&e.folder, &b).unwrap();
        assert!(fs::symlink_metadata(b.dir.join(&e.folder)).is_err());
    }

    #[test]
    fn bridge_preserves_provenance() {
        let (t, b) = setup();
        let s = t.path().join("claude").join("Release Notes");
        skill(&s, "release-notes", "notes");
        write(&s.join("references").join("style.md"), "style");
        let before = crate::catalog::hash_dir(&s);
        let e = add(&s, "claude", &b).unwrap();
        assert_eq!(e.folder, "claude--release-notes");
        assert_eq!(e.source, "claude");
        assert_eq!(e.license.as_deref(), Some("MIT"));
        assert_eq!(e.author.as_deref(), Some("Someone"));
        assert_eq!(e.hash, before);
        assert!(Path::new(&e.source_path).ends_with("Release Notes"));
        // Hermes sees the files through the bridge.
        assert!(b
            .dir
            .join("claude--release-notes")
            .join("references")
            .join("style.md")
            .is_file());
        let led: Ledger =
            serde_json::from_str(&fs::read_to_string(b.dir.join(LEDGER)).unwrap()).unwrap();
        assert_eq!(led.entries["claude--release-notes"], e);
        // Rollback: removing the bridge leaves the source exactly as it was.
        remove(&e.folder, &b).unwrap();
        assert_eq!(crate::catalog::hash_dir(&s), before);
        assert!(s.join("references").join("style.md").is_file());
        // Same name from another store is refused while one is bridged.
        add(&s, "claude", &b).unwrap();
        let q = t.path().join("qoder").join("rn");
        skill(&q, "release-notes", "other");
        assert!(add(&q, "qoder", &b)
            .unwrap_err()
            .to_string()
            .contains("already bridged"));
    }

    /// Check (and with BRIDGE_ADD=1, bridge) real skills into the live bridge folder.
    /// Run: BRIDGE_SKILLS="qoder=<dir>;claude=<dir>" cargo test --lib skill_bridge::tests::live_bridge -- --ignored --nocapture
    #[test]
    #[ignore = "touches the live Hermes bridge folder"]
    fn live_bridge() {
        let b = Bridge::live();
        let spec = std::env::var("BRIDGE_SKILLS").expect("BRIDGE_SKILLS");
        for pair in spec.split(';').filter(|p| !p.trim().is_empty()) {
            let (source, dir) = pair.split_once('=').expect("source=dir");
            let c = check(Path::new(dir), &b).unwrap();
            println!("{source} {}: problems {:?}", c.name, c.problems);
            if std::env::var("BRIDGE_ADD").as_deref() == Ok("1") && c.problems.is_empty() {
                println!("added {:?}", add(Path::new(dir), source, &b).unwrap());
            }
        }
        for s in status(&b).unwrap() {
            println!("status {} {}", s.entry.folder, s.state);
        }
    }

    #[test]
    fn folder_names_and_config_parsing() {
        assert_eq!(
            parse_dirs("  - C:/a\r\n  - D:/b\r\n"),
            vec!["C:/a".to_string(), "D:/b".to_string()]
        );
        assert_eq!(
            plain(PathBuf::from(r"\\?\C:\x\y")),
            PathBuf::from(r"C:\x\y")
        );
        assert_eq!(plain(PathBuf::from(r"C:\x")), PathBuf::from(r"C:\x"));
        assert_eq!(
            folder_name("Qoder", "Azure AI / Gateway"),
            "qoder--azure-ai---gateway"
        );
        assert_eq!(parse_dirs("[]"), Vec::<String>::new());
        assert_eq!(
            parse_dirs(r#"["C:/a", "D:/b"]"#),
            vec!["C:/a".to_string(), "D:/b".to_string()]
        );
        assert_eq!(
            parse_dirs("- C:/a\n- D:/b"),
            vec!["C:/a".to_string(), "D:/b".to_string()]
        );
        assert!(same_dir(
            "C:/Users/x/skills-external/",
            Path::new("c:\\users\\x\\skills-external")
        ));
        let b = Bridge {
            dir: PathBuf::from("C:\\h\\skills-external"),
            native: PathBuf::from("C:\\h\\skills"),
        };
        assert_eq!(
            config_snippet(&b),
            "skills:\n  external_dirs:\n    - C:/h/skills-external\n"
        );
    }

    #[test]
    fn config_change_is_guarded_by_skills_external_action() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let (_t, b) = setup();

        let err = set_config(&b, true, false).unwrap_err().to_string();
        assert!(
            err.starts_with(crate::permissions::APPROVAL_PREFIX),
            "unapproved config change must ask: {err}"
        );
        assert!(err.contains(ACTION), "{err}");

        {
            let c = crate::store::open().unwrap();
            crate::permissions::set_policy_in(&c, ACTION, "deny").unwrap();
        }
        let denied = set_config(&b, true, true).unwrap_err().to_string();
        assert!(
            denied.contains("Blocked by your permission policy"),
            "{denied}"
        );

        crate::store::set_test_db(None);
    }
}
