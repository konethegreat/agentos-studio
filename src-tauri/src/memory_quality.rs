//! Memory quality control: stale references and contradictory notes.
//!
//! A stale note names a repository in its frontmatter and points at a path or function that no
//! longer exists in that repository's checkout. A contradiction is a pair of active notes with the
//! same normalised topic and type, different content, and a cheap opposition signal; one short
//! Hermes turn decides which note to keep. Both outcomes mark the losing note superseded through
//! the frontmatter the vault already uses. Nothing here deletes a note, and a note edited in the
//! last seven days is reported instead of changed.
//!
//! The model turn is injected, so every path except the Hermes call runs in unit tests.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::memory_index;
use crate::procs;
use crate::store;
use crate::vault::{now, Doc, Vault};

pub const WEEKLY_JOB: &str = "agentos-memory-quality";
pub const WEEKLY_SCHEDULE: &str = "0 6 * * 1";
const LAST_RUN_KEY: &str = "memory_quality.last_run";
const PROTECT_SECS: u64 = 7 * 24 * 60 * 60;
const MAX_REFS: usize = 5;
const MAX_BODY: usize = 600;
const MAX_REASON: usize = 500;
const MAX_CANDIDATES: usize = 20;

/// One note the check would change, or did change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// The note's file stem, the form a wikilink uses.
    pub note: String,
    /// The vault-relative path.
    pub path: String,
    /// `stale` or `contradiction`.
    pub kind: String,
    /// The note that wins a contradiction.
    pub superseded_by: Option<String>,
    pub reason: String,
    /// True when this run wrote the supersede. False in a dry run or for a protected note.
    pub written: bool,
    /// True when the note changed in the last seven days, so it is reported only.
    pub protected: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryQualityReport {
    pub ran_at: String,
    pub dry_run: bool,
    pub scanned: usize,
    pub stale: Vec<Change>,
    pub contradictions: Vec<Change>,
    /// Why the contradiction turn's answer was dropped, when it was not valid.
    pub dropped: Option<String>,
}

impl MemoryQualityReport {
    pub fn superseded(&self) -> usize {
        self.stale
            .iter()
            .chain(self.contradictions.iter())
            .filter(|c| c.written)
            .count()
    }
}

#[derive(Debug, Clone)]
struct Note {
    rel: String,
    slug: String,
    kind: String,
    status: String,
    repo: Option<String>,
    topic: String,
    body: String,
    protected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefKind {
    Path,
    Function,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Ref {
    token: String,
    kind: RefKind,
}

/// Backticked paths and functions a note body points at. Pure, so the extraction is tested
/// without a repository.
fn references(body: &str) -> Vec<Ref> {
    let mut out = Vec::new();
    for token in backticked(body) {
        if let Some(r) = classify(&token) {
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out
}

fn backticked(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else { break };
        let token = rest[..close].trim();
        if !token.is_empty() {
            out.push(token.to_string());
        }
        rest = &rest[close + 1..];
    }
    out
}

fn classify(raw: &str) -> Option<Ref> {
    let token = raw
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | '!' | '?'))
        .trim_end_matches('.');
    if token.is_empty() || token.contains("..") {
        return None;
    }
    if ["src/", "src-tauri/", "docs/"]
        .iter()
        .any(|p| token.starts_with(p))
    {
        let rel = strip_line_suffix(token);
        if !rel.is_empty() && !rel.contains(' ') && Path::new(rel).is_relative() {
            return Some(Ref {
                token: rel.to_string(),
                kind: RefKind::Path,
            });
        }
    }
    function_name(token).map(|name| Ref {
        token: name,
        kind: RefKind::Function,
    })
}

fn strip_line_suffix(token: &str) -> &str {
    let token = token.split("#L").next().unwrap_or(token);
    if let Some(i) = token.rfind(':') {
        if i + 1 < token.len() && token[i + 1..].chars().all(|c| c.is_ascii_digit()) {
            return &token[..i];
        }
    }
    token
}

fn function_name(token: &str) -> Option<String> {
    let name = if let Some(rest) = token.strip_suffix("()") {
        rest
    } else if token.contains("::") {
        token.rsplit("::").next()?
    } else {
        return None;
    };
    valid_ident(name).then(|| name.to_string())
}

fn valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// References missing from the checkout. A function is checked by a word match in tracked files;
/// when the folder is not a git repository the check is skipped rather than guessed.
fn missing_refs(body: &str, dir: &Path) -> Vec<Ref> {
    references(body)
        .into_iter()
        .filter(|r| match r.kind {
            RefKind::Path => safe_join(dir, &r.token)
                .map(|p| !p.exists())
                .unwrap_or(true),
            RefKind::Function => git_grep_word(dir, &r.token) == Some(false),
        })
        .collect()
}

fn safe_join(dir: &Path, rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute()
        || p.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    Some(dir.join(p))
}

fn git_grep_word(dir: &Path, name: &str) -> Option<bool> {
    let exe = procs::find("git")?;
    let out = procs::command(&exe)
        .args(["grep", "-q", "-w", "-e", name])
        .current_dir(dir)
        .output()
        .ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// The folder a repository field points at. A project slug resolves through the projects list; an
/// absolute path is accepted so a sandbox checkout can stand in for a repository.
fn repo_dir(repo: &str) -> Option<PathBuf> {
    let trimmed = repo.trim().trim_end_matches('/');
    let path = PathBuf::from(trimmed);
    if path.is_absolute() && path.is_dir() {
        return Some(path);
    }
    let name = trimmed.rsplit('/').next()?.trim_end_matches(".git");
    crate::review_agent::project_dir(name).filter(|d| d.is_dir())
}

fn load_notes(v: &Vault) -> Vec<Note> {
    let now = SystemTime::now();
    memory_index::notes(v)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| {
            let path = v.root.join(&row.path);
            let raw = fs::read_to_string(&path).ok()?;
            let doc = Doc::parse(&raw);
            let slug = path.file_stem()?.to_string_lossy().to_string();
            let modified = fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(UNIX_EPOCH);
            let topic = doc
                .get("topic")
                .map(str::to_string)
                .or_else(|| doc_title(&doc))
                .unwrap_or_else(|| row.title.clone());
            Some(Note {
                rel: row.path,
                slug,
                kind: row.kind,
                status: doc.get("status").unwrap_or(&row.status).to_string(),
                repo: doc.get("repo").map(str::to_string).or(row.repo),
                topic,
                body: doc.body,
                protected: now
                    .duration_since(modified)
                    .map(|d| d < Duration::from_secs(PROTECT_SECS))
                    .unwrap_or(true),
            })
        })
        .collect()
}

fn doc_title(doc: &Doc) -> Option<String> {
    doc.body
        .lines()
        .find_map(|l| l.trim().strip_prefix("# ").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
}

fn stale_changes(
    v: &Vault,
    notes: &[Note],
    dry_run: bool,
    marked: &mut HashSet<String>,
) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    for note in notes.iter().filter(|n| n.status == "active") {
        let Some(repo) = note.repo.as_deref() else {
            continue;
        };
        let Some(dir) = repo_dir(repo) else {
            continue;
        };
        let refs = missing_refs(&note.body, &dir);
        if refs.is_empty() {
            continue;
        }
        let listing: Vec<String> = refs
            .iter()
            .take(MAX_REFS)
            .map(|r| format!("`{}`", r.token))
            .collect();
        let reason = format!("{} no longer has {}", repo.trim(), listing.join(", "));
        let mut written = false;
        if !note.protected && !dry_run {
            supersede(v, &note.rel, None, &format!("Stale reference: {reason}"))?;
            written = true;
            marked.insert(note.rel.clone());
        }
        out.push(Change {
            note: note.slug.clone(),
            path: note.rel.clone(),
            kind: "stale".into(),
            superseded_by: None,
            reason,
            written,
            protected: note.protected,
        });
    }
    Ok(out)
}

/// Pairs of active notes with the same type and normalised topic, different content, and a cheap
/// opposition signal. The signal keeps the model turn small: similar notes with no polarity or
/// value difference are not candidates.
fn candidate_pairs(notes: &[Note]) -> Vec<(usize, usize)> {
    let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (i, n) in notes.iter().enumerate() {
        if n.status != "active" || !matches!(n.kind.as_str(), "convention" | "fact") {
            continue;
        }
        let topic = norm_topic(&n.topic);
        if topic.is_empty() {
            continue;
        }
        groups.entry((n.kind.clone(), topic)).or_default().push(i);
    }
    let mut out = Vec::new();
    for ids in groups.values() {
        for a in 0..ids.len() {
            for b in a + 1..ids.len() {
                let (i, j) = (ids[a], ids[b]);
                if condensed(&notes[i].body) == condensed(&notes[j].body) {
                    continue;
                }
                if !opposition(&notes[i].body, &notes[j].body) {
                    continue;
                }
                out.push((i, j));
                if out.len() >= MAX_CANDIDATES {
                    return out;
                }
            }
        }
    }
    out
}

fn norm_topic(topic: &str) -> String {
    topic
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn condensed(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| !l.to_lowercase().starts_with("see also:"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn opposition(a: &str, b: &str) -> bool {
    if let (Some(x), Some(y)) = (polarity(a), polarity(b)) {
        if x != y {
            return true;
        }
    }
    let (na, nb) = (numbers(a), numbers(b));
    !na.is_empty() && !nb.is_empty() && na != nb
}

fn polarity(text: &str) -> Option<bool> {
    let s = text.to_lowercase();
    if s.contains("never") || s.contains("do not") || s.contains("don't") || s.contains("must not")
    {
        return Some(true);
    }
    if s.contains("always") || s.contains("must ") || s.contains(" do ") || s.starts_with("do ") {
        return Some(false);
    }
    None
}

fn numbers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
        .filter(|w| w.chars().any(|c| c.is_ascii_digit()))
        .map(str::to_lowercase)
        .collect();
    out.sort();
    out.dedup();
    out
}

fn verdict_prompt(notes: &[Note], pairs: &[(usize, usize)]) -> String {
    let mut p = String::from(
        "You check AgentOS memory notes for contradictions. Two notes contradict only when \
following both would lead to different actions or different values. Wording, extra detail and a \
narrower scope are compatible.\n\n\
Reply with a single JSON array and nothing else. One entry per pair, in order:\n\
[{\"pair\":1,\"verdict\":\"contradiction\"|\"compatible\",\"keep\":\"slug of the note to keep\",\
\"reason\":\"one sentence\"}]\n\
The keep value must be one of the two slugs in the pair. Never suggest deleting a note.\n\n",
    );
    for (n, (i, j)) in pairs.iter().enumerate() {
        let a = &notes[*i];
        let b = &notes[*j];
        p.push_str(&format!(
            "Pair {}\nNote A: {}\nTopic: {}\nKind: {}\n{}\n\nNote B: {}\nTopic: {}\nKind: {}\n{}\n\n",
            n + 1,
            a.slug,
            a.topic,
            a.kind,
            clip(&condensed(&a.body), MAX_BODY),
            b.slug,
            b.topic,
            b.kind,
            clip(&condensed(&b.body), MAX_BODY)
        ));
    }
    p
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect::<String>() + "..."
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Verdict {
    pair: usize,
    contradiction: bool,
    keep: String,
    reason: String,
}

/// Strict: every pair must be answered exactly once, and every kept note must be one of the pair.
/// One malformed entry refuses the whole answer, so a half answer never changes a note.
fn parse_verdicts(text: &str, pairs: &[(String, String)]) -> Result<Vec<Verdict>> {
    let start = text
        .find('[')
        .ok_or_else(|| anyhow!("the answer has no JSON array"))?;
    let end = text
        .rfind(']')
        .ok_or_else(|| anyhow!("the answer has no closing bracket"))?;
    if end <= start {
        bail!("the answer has no JSON array");
    }
    let value: Value = serde_json::from_str(&text[start..=end])
        .map_err(|e| anyhow!("the answer is not valid JSON: {e}"))?;
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("the answer is not a JSON array"))?;
    if items.len() != pairs.len() {
        bail!(
            "the answer has {} verdicts for {} pairs",
            items.len(),
            pairs.len()
        );
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, item) in items.iter().enumerate() {
        let n = i + 1;
        let obj = item
            .as_object()
            .ok_or_else(|| anyhow!("verdict {n} is not an object"))?;
        let pair = obj
            .get("pair")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .ok_or_else(|| anyhow!("verdict {n} has no pair number"))?;
        if pair == 0 || pair > pairs.len() || !seen.insert(pair) {
            bail!("verdict {n} names a duplicate or unknown pair");
        }
        let verdict = obj
            .get("verdict")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_lowercase())
            .ok_or_else(|| anyhow!("verdict {n} has no verdict"))?;
        let contradiction = match verdict.as_str() {
            "contradiction" => true,
            "compatible" => false,
            _ => bail!("verdict {n} is neither contradiction nor compatible"),
        };
        let keep = obj
            .get("keep")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("verdict {n} has no keep"))?;
        let (a, b) = &pairs[pair - 1];
        if keep != a && keep != b {
            bail!("verdict {n} keeps a note that is not in the pair");
        }
        let reason = obj
            .get("reason")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("verdict {n} has no reason"))?;
        out.push(Verdict {
            pair,
            contradiction,
            keep: keep.to_string(),
            reason: reason.chars().take(MAX_REASON).collect(),
        });
    }
    out.sort_by_key(|v| v.pair);
    Ok(out)
}

fn contradiction_changes(
    v: &Vault,
    notes: &[Note],
    pairs: &[(usize, usize)],
    verdicts: &[Verdict],
    dry_run: bool,
    marked: &mut HashSet<String>,
) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    for verdict in verdicts.iter().filter(|v| v.contradiction) {
        let (i, j) = pairs[verdict.pair - 1];
        let (a, b) = (&notes[i], &notes[j]);
        if marked.contains(&a.rel) || marked.contains(&b.rel) {
            continue;
        }
        let (winner, loser) = if verdict.keep == a.slug {
            (a, b)
        } else {
            (b, a)
        };
        let reason = format!("Contradicts [[{}]]: {}", winner.slug, verdict.reason);
        let mut written = false;
        if !loser.protected && !dry_run {
            supersede(v, &loser.rel, Some(&winner.slug), &reason)?;
            written = true;
            marked.insert(loser.rel.clone());
        }
        out.push(Change {
            note: loser.slug.clone(),
            path: loser.rel.clone(),
            kind: "contradiction".into(),
            superseded_by: Some(winner.slug.clone()),
            reason,
            written,
            protected: loser.protected,
        });
    }
    Ok(out)
}

fn supersede(v: &Vault, rel: &str, winner: Option<&str>, reason: &str) -> Result<()> {
    let path = v.resolve_rel(rel)?;
    let mut doc = Doc::parse(&fs::read_to_string(&path)?);
    doc.set_str("status", "superseded");
    if let Some(w) = winner {
        doc.set_str("superseded_by", w);
    }
    doc.set_str("superseded_reason", reason);
    doc.set_str("updated", now());
    v.write_note(rel, &doc.render())
}

/// The model turn seam: the answer text. The workspace and the query file are passed so the call
/// is visible to a test.
type Turn<'a> = dyn Fn(&Path, &Path) -> Result<String> + 'a;

/// Run the check and record the report for the Brain view. A dry run writes nothing at all.
pub fn run(v: &Vault, dry_run: bool) -> Result<MemoryQualityReport> {
    let report = run_with(v, dry_run, &run_quality_turn)?;
    if !dry_run {
        let c = store::open()?;
        store::kv_set(&c, LAST_RUN_KEY, &serde_json::to_string(&report)?)?;
    }
    Ok(report)
}

fn run_with(v: &Vault, dry_run: bool, turn: &Turn<'_>) -> Result<MemoryQualityReport> {
    let notes = load_notes(v);
    let mut marked = HashSet::new();
    let stale = stale_changes(v, &notes, dry_run, &mut marked)?;
    let pairs = candidate_pairs(&notes);
    let mut contradictions = Vec::new();
    let mut dropped = None;
    if !pairs.is_empty() {
        let prompt = verdict_prompt(&notes, &pairs);
        let qdir = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("agentos-studio")
            .join("memory-quality");
        fs::create_dir_all(&qdir)?;
        let qpath = qdir.join("verdicts.md");
        fs::write(&qpath, &prompt)?;
        let slugs: Vec<(String, String)> = pairs
            .iter()
            .map(|(i, j)| (notes[*i].slug.clone(), notes[*j].slug.clone()))
            .collect();
        match turn(&qdir, &qpath)
            .and_then(|text| parse_verdicts(&text, &slugs))
            .and_then(|verdicts| {
                contradiction_changes(v, &notes, &pairs, &verdicts, dry_run, &mut marked)
            }) {
            Ok(changes) => contradictions = changes,
            Err(e) => {
                eprintln!("memory quality: dropped the verdict answer: {e:#}");
                dropped = Some(format!("{e:#}"));
            }
        }
    }
    Ok(MemoryQualityReport {
        ran_at: now(),
        dry_run,
        scanned: notes.len(),
        stale,
        contradictions,
        dropped,
    })
}

/// The last recorded run, for the Brain view. The weekly job and the app both record here.
pub fn last_report() -> Option<MemoryQualityReport> {
    let c = store::open().ok()?;
    let raw = store::kv_get(&c, LAST_RUN_KEY).ok()??;
    serde_json::from_str(&raw).ok()
}

/// One short headless turn on the reviewer's profile when one is configured, otherwise the default
/// profile. Quiet mode plus the default oneshot source keeps it out of the owner's chat list.
fn run_quality_turn(dir: &Path, qpath: &Path) -> Result<String> {
    let mut args: Vec<String> = crate::hermes_surfaces::profile_for_role("reviewer")
        .map(|p| vec!["-p".into(), p])
        .unwrap_or_default();
    args.extend(["chat", "-Q", "--oneshot", "--query-file"].map(String::from));
    args.push(qpath.display().to_string());
    args.extend(["--in".into(), dir.display().to_string()]);
    let exe = procs::find("hermes")
        .ok_or_else(|| anyhow!("hermes is not installed, so contradictions cannot be checked"))?;
    let out = procs::command(&exe)
        .args(&args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .output()?;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() && text.trim().is_empty() {
        bail!("the quality turn failed: {}", stderr.trim());
    }
    if text.trim().is_empty() {
        bail!("the quality turn returned nothing: {}", stderr.trim());
    }
    Ok(text)
}

/// The Python script Hermes cron runs. Empty on success is fine; the report stays in the Brain
/// view, and stdout is delivered by the job when it has something to say.
pub fn weekly_script(exe: &Path, vault: &Path) -> String {
    format!(
        "import subprocess\nimport sys\n\n\
exe = {exe:?}\n\
vault = {vault:?}\n\
result = subprocess.run([exe, \"--memory-quality\", \"--vault\", vault], capture_output=True, text=True)\n\
sys.stdout.write(result.stdout)\n\
if result.returncode != 0:\n\
    sys.stderr.write(result.stderr)\n\
    sys.exit(result.returncode)\n",
        exe = exe.display().to_string(),
        vault = vault.display().to_string()
    )
}

/// Create the weekly Hermes cron job that runs the headless flag. The script lives under
/// `~/.hermes/scripts/`, where `hermes cron` requires it, and the job names it by file name alone:
/// Hermes resolves the name inside the profile's scripts folder and rejects an absolute path.
/// Idempotent by job name.
pub fn schedule(v: &Vault, exe: &Path) -> Result<String> {
    if let Some(job) = crate::hermes_surfaces::cron_jobs()
        .iter()
        .find(|j| j.name.as_deref() == Some(WEEKLY_JOB))
    {
        return Ok(format!("Weekly check is already scheduled as {}", job.id));
    }
    let scripts = procs::hermes_home().join("scripts");
    fs::create_dir_all(&scripts)?;
    let script = scripts.join(format!("{WEEKLY_JOB}.py"));
    fs::write(&script, weekly_script(exe, &v.root))?;
    crate::hermes_surfaces::cron_create(&crate::hermes_surfaces::CronSpec {
        schedule: WEEKLY_SCHEDULE.into(),
        prompt: String::new(),
        name: Some(WEEKLY_JOB.into()),
        workdir: None,
        paused: false,
        no_agent: true,
        script: Some(format!("{WEEKLY_JOB}.py")),
    })
}

#[derive(Debug, PartialEq)]
struct Cli {
    vault: PathBuf,
    dry_run: bool,
}

fn parse_cli(args: &[String]) -> Result<Cli> {
    let mut vault = None;
    let mut dry_run = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--memory-quality" => {}
            "--dry-run" => dry_run = true,
            "--vault" => {
                i += 1;
                let path = args.get(i).ok_or_else(|| anyhow!("--vault needs a path"))?;
                vault = Some(PathBuf::from(path));
            }
            _ if i == 0 => {}
            other => bail!("unknown argument {other}"),
        }
        i += 1;
    }
    let vault = vault.ok_or_else(|| anyhow!("--vault is required"))?;
    if !vault.is_dir() {
        bail!("vault folder does not exist: {}", vault.display());
    }
    Ok(Cli { vault, dry_run })
}

/// Entry point for `agentos-studio.exe --memory-quality --vault <vault> [--dry-run]`.
pub fn run_from_args(args: &[String]) -> i32 {
    match parse_cli(args) {
        Ok(cli) => {
            let v = Vault::new(cli.vault);
            match run(&v, cli.dry_run) {
                Ok(report) => {
                    print_report(&report);
                    0
                }
                Err(e) => {
                    eprintln!("memory quality failed: {e:#}");
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("memory quality: {e:#}");
            2
        }
    }
}

fn print_report(report: &MemoryQualityReport) {
    let mode = if report.dry_run { "dry run" } else { "run" };
    println!(
        "Memory quality {mode}: {} notes scanned, {} stale, {} contradictory, {} superseded.",
        report.scanned,
        report.stale.len(),
        report.contradictions.len(),
        report.superseded()
    );
    for change in report.stale.iter().chain(report.contradictions.iter()) {
        let action = if change.written {
            "superseded".to_string()
        } else if change.protected {
            "reported only (edited in the last 7 days)".to_string()
        } else {
            "would supersede".to_string()
        };
        match &change.superseded_by {
            Some(winner) => println!(
                "- {} {} -> {}: {}",
                change.note, action, winner, change.reason
            ),
            None => println!("- {} {}: {}", change.note, action, change.reason),
        }
    }
    if let Some(dropped) = &report.dropped {
        println!("- contradiction answer dropped: {dropped}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> (tempfile::TempDir, Vault) {
        let d = tempfile::tempdir().unwrap();
        crate::memory_index::set_test_index_dir(Some(d.path().to_path_buf()));
        let v = Vault::new(d.path().join("vault"));
        v.scaffold().unwrap();
        (d, v)
    }

    fn write_note(v: &Vault, file: &str, topic: &str, kind: &str, repo: Option<&str>, body: &str) {
        let mut doc = Doc::default();
        doc.set_str("type", kind);
        doc.set_str("status", "active");
        doc.set_str("topic", topic);
        doc.set_str("created", now());
        doc.set_str("updated", now());
        if let Some(repo) = repo {
            doc.set_str("repo", repo);
        }
        doc.body = format!("# {topic}\n\n{body}\n");
        v.write_note(&format!("03_Memory/{file}"), &doc.render())
            .unwrap();
    }

    fn backdate(v: &Vault, file: &str, days: u64) {
        let path = v.resolve_rel(&format!("03_Memory/{file}")).unwrap();
        let handle = fs::File::options().write(true).open(&path).unwrap();
        handle
            .set_modified(SystemTime::now() - Duration::from_secs(days * 86_400))
            .unwrap();
    }

    fn git_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let path = d.path().join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, body).unwrap();
        }
        for args in [["init", "-q"].as_slice(), ["add", "-A"].as_slice()] {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(d.path())
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?} failed");
        }
        d
    }

    fn no_turn(_: &Path, _: &Path) -> Result<String> {
        panic!("no contradiction turn is expected here")
    }

    #[test]
    fn references_find_paths_and_functions() {
        let body = "See `src/parser.rs`, `src-tauri/src/lib.rs:120`, `docs/ARCHITECTURE.md#L4`, \
`lessons_for()`, `vault::Doc` and `plain_words`. Ignore `src/../secret` and `src/no space`.";
        let refs = references(body);
        assert_eq!(
            refs,
            vec![
                Ref {
                    token: "src/parser.rs".into(),
                    kind: RefKind::Path
                },
                Ref {
                    token: "src-tauri/src/lib.rs".into(),
                    kind: RefKind::Path
                },
                Ref {
                    token: "docs/ARCHITECTURE.md".into(),
                    kind: RefKind::Path
                },
                Ref {
                    token: "lessons_for".into(),
                    kind: RefKind::Function
                },
                Ref {
                    token: "Doc".into(),
                    kind: RefKind::Function
                },
            ]
        );
        assert!(references("nothing backticked").is_empty());
    }

    #[test]
    fn a_deleted_file_makes_a_note_stale() {
        let (_d, v) = vault();
        let repo = git_repo(&[("src/kept.rs", "pub fn kept_fn() {}\n")]);
        write_note(
            &v,
            "Gone_file.md",
            "Gone file",
            "fact",
            Some(repo.path().to_str().unwrap()),
            "The parser lives in `src/gone.rs` and calls `kept_fn()`.",
        );
        let report = run_with(&v, true, &no_turn).unwrap();
        assert_eq!(report.stale.len(), 1, "{report:?}");
        assert!(report.stale[0].reason.contains("src/gone.rs"));
        assert!(!report.stale[0].reason.contains("kept_fn"));
        assert!(!report.stale[0].written);
    }

    #[test]
    fn a_renamed_function_makes_a_note_stale() {
        let (_d, v) = vault();
        let repo = git_repo(&[("src/lib.rs", "pub fn new_name() {}\n")]);
        write_note(
            &v,
            "Renamed_fn.md",
            "Renamed function",
            "gotcha",
            Some(repo.path().to_str().unwrap()),
            "`src/lib.rs` calls `old_name()` through `vault::old_helper`.",
        );
        let report = run_with(&v, true, &no_turn).unwrap();
        assert_eq!(report.stale.len(), 1, "{report:?}");
        assert!(report.stale[0].reason.contains("old_name"));
        assert!(report.stale[0].reason.contains("old_helper"));
        assert!(!report.stale[0].reason.contains("src/lib.rs"));
    }

    #[test]
    fn an_untouched_note_is_not_stale() {
        let (_d, v) = vault();
        let repo = git_repo(&[("src/lib.rs", "pub fn new_name() {}\n")]);
        write_note(
            &v,
            "Current.md",
            "Current code",
            "fact",
            Some(repo.path().to_str().unwrap()),
            "`src/lib.rs` defines `new_name()`.",
        );
        let report = run_with(&v, true, &no_turn).unwrap();
        assert!(report.stale.is_empty(), "{report:?}");
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let (_d, v) = vault();
        let repo = git_repo(&[("src/kept.rs", "pub fn kept_fn() {}\n")]);
        write_note(
            &v,
            "Old_stale.md",
            "Old stale",
            "fact",
            Some(repo.path().to_str().unwrap()),
            "`src/gone.rs` is gone.",
        );
        backdate(&v, "Old_stale.md", 8);
        let before = fs::read_to_string(v.resolve_rel("03_Memory/Old_stale.md").unwrap()).unwrap();
        let report = run_with(&v, true, &no_turn).unwrap();
        assert_eq!(report.stale.len(), 1);
        assert!(!report.stale[0].written);
        let after = fs::read_to_string(v.resolve_rel("03_Memory/Old_stale.md").unwrap()).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn a_recent_note_is_reported_and_not_changed() {
        let (_d, v) = vault();
        let repo = git_repo(&[("src/kept.rs", "pub fn kept_fn() {}\n")]);
        write_note(
            &v,
            "Recent_stale.md",
            "Recent stale",
            "fact",
            Some(repo.path().to_str().unwrap()),
            "`src/gone.rs` is gone.",
        );
        let before =
            fs::read_to_string(v.resolve_rel("03_Memory/Recent_stale.md").unwrap()).unwrap();
        let report = run_with(&v, false, &no_turn).unwrap();
        assert_eq!(report.stale.len(), 1);
        assert!(report.stale[0].protected);
        assert!(!report.stale[0].written);
        assert_eq!(report.superseded(), 0);
        let after =
            fs::read_to_string(v.resolve_rel("03_Memory/Recent_stale.md").unwrap()).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn an_old_stale_note_is_superseded_and_never_deleted() {
        let (_d, v) = vault();
        let repo = git_repo(&[("src/kept.rs", "pub fn kept_fn() {}\n")]);
        write_note(
            &v,
            "Old_stale.md",
            "Old stale",
            "fact",
            Some(repo.path().to_str().unwrap()),
            "`src/gone.rs` is gone.",
        );
        backdate(&v, "Old_stale.md", 8);
        let report = run_with(&v, false, &no_turn).unwrap();
        assert_eq!(report.stale.len(), 1);
        assert!(report.stale[0].written);
        assert_eq!(report.superseded(), 1);

        let path = v.resolve_rel("03_Memory/Old_stale.md").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let doc = Doc::parse(&text);
        assert_eq!(doc.get("status"), Some("superseded"));
        assert!(doc
            .get("superseded_reason")
            .unwrap()
            .contains("src/gone.rs"));

        // The note stays on disk, and a rerun has nothing left to change.
        let again = run_with(&v, false, &no_turn).unwrap();
        assert!(again.stale.is_empty(), "{again:?}");
        assert!(path.exists());
    }

    #[test]
    fn contradictory_notes_mark_the_loser_with_an_injected_turn() {
        let (_d, v) = vault();
        write_note(
            &v,
            "A_retry.md",
            "Retry budget",
            "convention",
            None,
            "Always retry the request twice.",
        );
        write_note(
            &v,
            "B_retry.md",
            "Retry budget",
            "convention",
            None,
            "Never retry the request twice.",
        );
        backdate(&v, "A_retry.md", 8);
        backdate(&v, "B_retry.md", 8);
        let fake = |_: &Path, query: &Path| {
            let prompt = fs::read_to_string(query).unwrap();
            assert!(prompt.contains("A_retry"), "{prompt}");
            assert!(prompt.contains("B_retry"), "{prompt}");
            Ok(
                "```json\n[{\"pair\":1,\"verdict\":\"contradiction\",\"keep\":\"A_retry\",\
\"reason\":\"one says always, the other never\"}]\n```"
                    .to_string(),
            )
        };
        let report = run_with(&v, false, &fake).unwrap();
        assert!(report.dropped.is_none(), "{report:?}");
        assert_eq!(report.contradictions.len(), 1, "{report:?}");
        assert!(report.contradictions[0].written);
        assert_eq!(
            report.contradictions[0].superseded_by.as_deref(),
            Some("A_retry")
        );

        let a = fs::read_to_string(v.resolve_rel("03_Memory/A_retry.md").unwrap()).unwrap();
        assert_eq!(Doc::parse(&a).get("status"), Some("active"));
        let b = fs::read_to_string(v.resolve_rel("03_Memory/B_retry.md").unwrap()).unwrap();
        let doc = Doc::parse(&b);
        assert_eq!(doc.get("status"), Some("superseded"));
        assert_eq!(doc.get("superseded_by"), Some("A_retry"));
        assert!(doc
            .get("superseded_reason")
            .unwrap()
            .contains("[[A_retry]]"));
    }

    #[test]
    fn a_compatible_verdict_changes_nothing() {
        let (_d, v) = vault();
        write_note(
            &v,
            "A_retry.md",
            "Retry budget",
            "convention",
            None,
            "Always retry the request twice.",
        );
        write_note(
            &v,
            "B_retry.md",
            "Retry budget",
            "convention",
            None,
            "Never retry the request twice.",
        );
        backdate(&v, "A_retry.md", 8);
        backdate(&v, "B_retry.md", 8);
        let fake = |_: &Path, _: &Path| {
            Ok(
                "[{\"pair\":1,\"verdict\":\"compatible\",\"keep\":\"A_retry\",\
\"reason\":\"different scope\"}]"
                    .to_string(),
            )
        };
        let report = run_with(&v, false, &fake).unwrap();
        assert!(report.contradictions.is_empty(), "{report:?}");
        let b = fs::read_to_string(v.resolve_rel("03_Memory/B_retry.md").unwrap()).unwrap();
        assert_eq!(Doc::parse(&b).get("status"), Some("active"));
    }

    #[test]
    fn an_invalid_verdict_answer_is_dropped_and_nothing_is_written() {
        let (_d, v) = vault();
        write_note(
            &v,
            "A_retry.md",
            "Retry budget",
            "convention",
            None,
            "Always retry the request twice.",
        );
        write_note(
            &v,
            "B_retry.md",
            "Retry budget",
            "convention",
            None,
            "Never retry the request twice.",
        );
        backdate(&v, "A_retry.md", 8);
        backdate(&v, "B_retry.md", 8);
        let fake = |_: &Path, _: &Path| {
            Ok(
                "[{\"pair\":1,\"verdict\":\"contradiction\",\"keep\":\"C_other\",\
\"reason\":\"wrong note\"}]"
                    .to_string(),
            )
        };
        let report = run_with(&v, false, &fake).unwrap();
        assert!(report.dropped.is_some(), "{report:?}");
        assert!(report.contradictions.is_empty());
        let b = fs::read_to_string(v.resolve_rel("03_Memory/B_retry.md").unwrap()).unwrap();
        assert_eq!(Doc::parse(&b).get("status"), Some("active"));
    }

    #[test]
    fn verdicts_are_strict() {
        let pairs = vec![
            ("A".to_string(), "B".to_string()),
            ("C".to_string(), "D".to_string()),
        ];
        let good = "[{\"pair\":2,\"verdict\":\"compatible\",\"keep\":\"D\",\"reason\":\"ok\"},\
{\"pair\":1,\"verdict\":\"contradiction\",\"keep\":\"A\",\"reason\":\"yes\"}]";
        let verdicts = parse_verdicts(good, &pairs).unwrap();
        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts[0].pair, 1);
        assert!(verdicts[0].contradiction);
        assert_eq!(verdicts[1].pair, 2);

        // Missing pair, duplicate pair, unknown keep and an invented verdict are all refused.
        assert!(parse_verdicts(
            "[{\"pair\":1,\"verdict\":\"compatible\",\"keep\":\"A\",\"reason\":\"ok\"}]",
            &pairs
        )
        .is_err());
        assert!(parse_verdicts(
            "[{\"pair\":1,\"verdict\":\"compatible\",\"keep\":\"A\",\"reason\":\"ok\"},\
{\"pair\":1,\"verdict\":\"compatible\",\"keep\":\"B\",\"reason\":\"ok\"}]",
            &pairs
        )
        .is_err());
        assert!(parse_verdicts(
            "[{\"pair\":1,\"verdict\":\"compatible\",\"keep\":\"C\",\"reason\":\"ok\"},\
{\"pair\":2,\"verdict\":\"compatible\",\"keep\":\"D\",\"reason\":\"ok\"}]",
            &pairs
        )
        .is_err());
        assert!(parse_verdicts("not json", &pairs).is_err());
    }

    #[test]
    fn topics_and_the_opposition_signal_are_normalised() {
        assert_eq!(norm_topic("Retry budget"), norm_topic("retry,  BUDGET"));
        assert_ne!(norm_topic("Retry budget"), norm_topic("Retry count"));
        assert!(opposition(
            "Always run the checks.",
            "Never run the checks."
        ));
        assert!(opposition("Do run the checks.", "Do not run the checks."));
        assert!(opposition("Wait 30 seconds.", "Wait 60 seconds."));
        assert!(!opposition("Use the vault.", "Use the vault for notes."));
        assert!(!opposition("Wait 30 seconds.", "Wait 30 seconds now."));
    }

    #[test]
    fn the_weekly_script_runs_the_headless_flag() {
        let script = weekly_script(
            Path::new("C:/bin/agentos-studio.exe"),
            Path::new("C:/vault"),
        );
        assert!(script.contains("--memory-quality"));
        assert!(script.contains("--vault"));
        assert!(script.contains("agentos-studio.exe"));
        assert!(script.contains("C:/vault"));
        assert!(script.contains("sys.exit"));
    }

    #[test]
    fn the_cli_needs_a_vault_that_exists() {
        let d = tempfile::tempdir().unwrap();
        let vault = d.path().to_str().unwrap().to_string();
        let cli = parse_cli(&[
            "agentos-studio.exe".into(),
            "--memory-quality".into(),
            "--vault".into(),
            vault,
            "--dry-run".into(),
        ])
        .unwrap();
        assert!(cli.dry_run);
        assert_eq!(cli.vault, d.path());

        assert!(parse_cli(&["agentos-studio.exe".into(), "--memory-quality".into()]).is_err());
        assert!(parse_cli(&[
            "agentos-studio.exe".into(),
            "--memory-quality".into(),
            "--vault".into(),
            d.path().join("missing").display().to_string(),
        ])
        .is_err());
    }
}
