//! The learning loop: finished work becomes memory.
//!
//! Two paths, both additive. A finished mission goes through one short headless Hermes turn that
//! answers with a JSON array of lessons, each written through the vault's typed-memory path. A
//! repository whose reviews keep rejecting work for the same reason gets one convention note that
//! names the missions, so the next attempt starts with that knowledge instead of rediscovering it.
//!
//! Nothing here is destructive and nothing runs the reviewer: `review_agent::review` moves a real
//! mission, while this module only reads what the review already recorded.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use rusqlite::params;
use serde::Serialize;
use serde_json::Value;

use crate::missions::{self, Mission};
use crate::procs;
use crate::store;
use crate::vault::{slug, MemKind, MemMeta, Vault};

/// A repository is warned about the same rejection reason only inside this window.
const REJECT_WINDOW_SECS: i64 = 30 * 24 * 60 * 60;
/// Two matching rejects are enough to turn a one-off into a convention.
const REJECT_THRESHOLD: usize = 2;
/// The conventions block handed to a future implementer, in characters.
const CONVENTIONS_CAP: usize = 1500;
const MAX_TOPIC_CHARS: usize = 120;
const MAX_CONTENT_CHARS: usize = 2000;
const MAX_PR_CONTEXT_CHARS: usize = 2000;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Lesson {
    pub kind: String,
    pub topic: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LearningReport {
    pub mission: String,
    /// The mission was already extracted; nothing ran and nothing was written.
    pub skipped: bool,
    /// Vault write results, one per note.
    pub notes: Vec<String>,
    /// Convention notes written or updated by the repeated-reject pass.
    pub conventions: Vec<String>,
    pub session: Option<String>,
    /// Why the model's answer was dropped, when it was not valid.
    pub dropped: Option<String>,
}

/// The JSON contract the learning turn must follow. An answer with any malformed entry is
/// refused as a whole: half a lesson set is worse than none, because the next run would append
/// the same notes again.
pub fn parse_lessons(text: &str) -> Result<Vec<Lesson>> {
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
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let n = i + 1;
        let obj = item
            .as_object()
            .ok_or_else(|| anyhow!("lesson {n} is not an object"))?;
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_lowercase())
            .filter(|s| matches!(s.as_str(), "decision" | "convention" | "gotcha"))
            .ok_or_else(|| anyhow!("lesson {n} has no valid type"))?;
        let topic = obj
            .get("topic")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("lesson {n} has no topic"))?;
        let content = obj
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("lesson {n} has no content"))?;
        if topic.chars().count() > MAX_TOPIC_CHARS {
            bail!("lesson {n} has a topic longer than {MAX_TOPIC_CHARS} characters");
        }
        out.push(Lesson {
            kind,
            topic: topic.to_string(),
            content: content.chars().take(MAX_CONTENT_CHARS).collect(),
        });
    }
    Ok(out)
}

/// The prompt for the learning turn. Pure, so the contract can be tested without a model.
pub fn lessons_prompt(m: &Mission, files: &str, pr_body: &str) -> String {
    let verdict = m.review_verdict.as_deref().unwrap_or("none");
    let note = m
        .review_note
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("No review note was recorded.");
    let body = if pr_body.trim().is_empty() {
        "No pull request summary was recorded."
    } else {
        pr_body.trim()
    };
    format!(
        "You are reviewing a finished mission to extract durable lessons for future work in this \
repository.\n\n\
Repository: {repo}\n\
Mission: {id}\n\
Goal: {goal}\n\n\
Acceptance criteria:\n{acceptance}\n\n\
Review verdict: {verdict}\n\
Reviewer note: {note}\n\n\
Files the pull request changed:\n{files}\n\n\
Pull request summary:\n{body}\n\n\
Write between one and five lessons that a future engineer in this repository would need and \
could not learn by reading the code. Each lesson must be specific, factual and short. Reply with \
a single JSON array and nothing else:\n\
[{{\"type\":\"decision\"|\"convention\"|\"gotcha\",\"topic\":\"short note title\",\"content\":\"one to three sentences\"}}]\n\
Valid types are decision, convention and gotcha. Do not repeat the acceptance criteria. Do not \
invent facts.",
        repo = m.repo,
        id = m.id,
        goal = m.goal,
        acceptance = m.acceptance,
        verdict = verdict,
        note = note,
        files = files.trim(),
        body = body
    )
}

/// A rejected review's reasons: its bullet list when it has one, otherwise its first sentence.
/// Pure, so the matching can be tested without a store.
pub fn reject_reasons(note: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in note.lines() {
        let line = line.trim();
        let bullet = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| numbered(line));
        if let Some(rest) = bullet {
            let reason = normalise(rest);
            if !reason.is_empty() {
                out.push(reason);
            }
        }
    }
    if out.is_empty() {
        if let Some(reason) = first_sentence(note) {
            out.push(reason);
        }
    }
    out
}

/// The canonical form two rejects share when they were rejected for the same reasons.
pub fn reason_key(note: &str) -> Option<String> {
    let mut reasons = reject_reasons(note);
    reasons.sort();
    reasons.dedup();
    if reasons.is_empty() {
        None
    } else {
        Some(reasons.join(" | "))
    }
}

/// `1. text` or `1) text`, the two numbered forms a reviewer note uses.
fn numbered(line: &str) -> Option<&str> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = &line[digits..];
    rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))
}

fn normalise(text: &str) -> String {
    let lowered = text.trim().to_lowercase();
    let joined = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.trim_end_matches(['.', ';', ':']).trim().to_string()
}

fn first_sentence(note: &str) -> Option<String> {
    let line = note.lines().map(str::trim).find(|l| !l.is_empty())?;
    let cut = line
        .find(['.', '!', '?'])
        .map(|i| &line[..i])
        .unwrap_or(line);
    let reason = normalise(cut);
    if reason.is_empty() {
        None
    } else {
        Some(reason)
    }
}

/// The directory a mission's work lives in. A project slug resolves through Hermes' projects; an
/// absolute path is accepted so a sandbox mission can point at a scratch checkout.
pub fn workspace_for(repo: &str) -> Option<PathBuf> {
    let path = PathBuf::from(repo);
    if path.is_absolute() && path.is_dir() {
        return Some(path);
    }
    crate::review_agent::project_dir(repo).filter(|d| d.is_dir())
}

/// Write or update the one convention note for a repository when two or more rejects inside the
/// window share the same reasons. Idempotent: the mission ids already named in the note are
/// recorded in the store, so a rerun writes nothing.
pub fn repeated_rejects(v: &Vault, repo: &str) -> Result<Vec<String>> {
    let repo = repo.trim();
    if repo.is_empty() {
        return Ok(Vec::new());
    }
    let since = chrono::Utc::now().timestamp() - REJECT_WINDOW_SECS;
    let c = store::open()?;
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    {
        let mut st = c.prepare(
            "SELECT id, review_note FROM missions
             WHERE lower(repo) = lower(?1) AND review_verdict = 'reject' AND updated_at >= ?2",
        )?;
        let rows = st.query_map(params![repo, since], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (id, note) = row?;
            if let Some(key) = reason_key(note.as_deref().unwrap_or("")) {
                groups.entry(key).or_default().push(id);
            }
        }
    }

    let mut written = Vec::new();
    for (key, mut ids) in groups {
        if ids.len() < REJECT_THRESHOLD {
            continue;
        }
        ids.sort();
        ids.dedup();
        let kv_key = format!(
            "learning.rejects.{}.{:016x}",
            slug(repo, 40).to_lowercase(),
            fnv(key.as_bytes())
        );
        let seen = store::kv_get(&c, &kv_key)?.unwrap_or_default();
        let seen: Vec<&str> = seen.split(',').filter(|s| !s.is_empty()).collect();
        if ids.iter().all(|id| seen.contains(&id.as_str())) {
            continue;
        }
        let links = ids
            .iter()
            .map(|id| format!("[[{id}]]"))
            .collect::<Vec<_>>()
            .join(", ");
        let topic = format!("Review blockers in {repo}");
        let content = format!(
            "The same review blocker came back in {} rejected missions: {key}.\n\n\
Missions: {links}\n\n\
Check for this before submitting work in this repository.",
            ids.len()
        );
        written.push(v.write_typed_memory(
            &topic,
            &content,
            "convention",
            &MemMeta {
                kind: Some(MemKind::Convention),
                repo: Some(repo.to_string()),
                agent: Some("agentos-learning".into()),
                ..Default::default()
            },
        )?);
        store::kv_set(&c, &kv_key, &ids.join(","))?;
    }
    Ok(written)
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// The active conventions a future implementer in this repository receives, newest first and
/// capped so the block stays small enough to carry in an instructions file.
pub fn conventions_for(v: &Vault, repo: &str) -> String {
    let mut rows: Vec<crate::memory_index::NoteRow> = crate::memory_index::notes(v)
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.kind == "convention" && row.status == "active")
        .filter(|row| {
            row.repo
                .as_deref()
                .map(|r| r.eq_ignore_ascii_case(repo.trim()))
                .unwrap_or(false)
        })
        .collect();
    // The index keeps mtimes to the second, so notes written in one run tie. The file's own
    // modified time has sub-second precision and gives a stable newest-first order.
    rows.sort_by_key(|row| std::cmp::Reverse(file_modified(v, &row.path)));
    let mut out = String::new();
    for row in rows {
        let Ok(raw) = std::fs::read_to_string(v.root.join(&row.path)) else {
            continue;
        };
        let body = condense(&crate::vault::Doc::parse(&raw).body);
        if body.is_empty() {
            continue;
        }
        let entry = format!("- {}: {}\n", row.title, body);
        if out.chars().count() + entry.chars().count() > CONVENTIONS_CAP {
            break;
        }
        out.push_str(&entry);
    }
    out.trim_end().to_string()
}

fn file_modified(v: &Vault, rel: &str) -> std::time::SystemTime {
    std::fs::metadata(v.root.join(rel))
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// Note bodies carry a title heading and a "See also" line; neither belongs in an instructions
/// block. Paragraphs are joined so a multi-section note stays one line.
fn condense(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| !l.to_lowercase().starts_with("see also:"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Run one headless Hermes turn for a finished mission and write what it learned. Idempotent per
/// mission: `learning.done.<mission id>` records a completed extraction, valid or empty.
pub fn lessons_for(v: &Vault, m: &Mission) -> Result<LearningReport> {
    lessons_with(v, m, &run_lessons_turn)
}

/// The model turn seam: the answer text and, when Hermes reports one, the session id.
type Turn<'a> = dyn Fn(&Path, &Path) -> Result<(String, Option<String>)> + 'a;

/// The write path with the model turn injected, so everything except the Hermes call is testable
/// without one.
fn lessons_with(v: &Vault, m: &Mission, turn: &Turn<'_>) -> Result<LearningReport> {
    if !matches!(m.status.as_str(), "done" | "failed") {
        bail!(
            "{} is {}; lessons are extracted after a mission is done or failed",
            m.id,
            m.status
        );
    }
    let c = store::open()?;
    let done_key = format!("learning.done.{}", m.id);
    if store::kv_get(&c, &done_key)?.is_some() {
        return Ok(LearningReport {
            mission: m.id.clone(),
            skipped: true,
            ..Default::default()
        });
    }
    let dir = workspace_for(&m.repo).ok_or_else(|| {
        anyhow!(
            "repository '{}' has no folder to run in, so no lessons can be read",
            m.repo
        )
    })?;
    let (files, body) = pull_request_context(m, &dir);
    let prompt_text = lessons_prompt(m, &files, &body);
    let qdir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("agentos-studio")
        .join("learning");
    std::fs::create_dir_all(&qdir)?;
    let qpath = qdir.join(format!("{}.md", slug(&m.id, 40)));
    std::fs::write(&qpath, &prompt_text)?;

    let (text, session) = turn(&dir, &qpath)?;
    let lessons = match parse_lessons(&text) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("learning: dropped {}'s answer: {e:#}", m.id);
            return Ok(LearningReport {
                mission: m.id.clone(),
                session,
                dropped: Some(format!("{e:#}")),
                ..Default::default()
            });
        }
    };
    let mut notes = Vec::new();
    for lesson in &lessons {
        notes.push(v.write_typed_memory(
            &lesson.topic,
            &lesson.content,
            &lesson.kind,
            &MemMeta {
                kind: Some(MemKind::parse(&lesson.kind)),
                repo: Some(m.repo.clone()),
                source_session: session.clone(),
                agent: Some("agentos-learning".into()),
                ..Default::default()
            },
        )?);
    }
    store::kv_set(&c, &done_key, &chrono::Utc::now().timestamp().to_string())?;
    Ok(LearningReport {
        mission: m.id.clone(),
        skipped: false,
        notes,
        conventions: Vec::new(),
        session,
        dropped: None,
    })
}

/// Extract lessons for one mission, then fold in the repeated-reject check for its repository.
pub fn extract_lessons(v: &Vault, id: &str) -> Result<LearningReport> {
    let m = missions::get(id)?.ok_or_else(|| anyhow!("no mission '{id}'"))?;
    let mut report = lessons_for(v, &m)?;
    report.conventions = repeated_rejects(v, &m.repo)?;
    Ok(report)
}

/// What the review path runs when a mission is approved: the same extraction the "Extract
/// lessons" button runs, for the mission a reviewer just finished. Called in the background by
/// `missions::review`; idempotent through the same `learning.done.<id>` key.
pub fn on_mission_reviewed(v: &Vault, id: &str) -> Result<LearningReport> {
    extract_lessons(v, id)
}

/// `gh pr diff --name-only` and the PR body, both optional: a mission without a pull request can
/// still teach something from its review.
fn pull_request_context(m: &Mission, dir: &Path) -> (String, String) {
    let Some(number) = missions::pr_number(m) else {
        return ("No pull request was recorded.".into(), String::new());
    };
    let number = number.to_string();
    let files = gh_text(dir, &["pr", "diff", &number, "--name-only"])
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.chars().take(MAX_PR_CONTEXT_CHARS).collect())
        .unwrap_or_else(|| "No files could be read from the pull request.".into());
    let body = gh_text(dir, &["pr", "view", &number, "--json", "body"])
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.get("body").and_then(Value::as_str).map(str::to_string))
        .map(|s| s.chars().take(MAX_PR_CONTEXT_CHARS).collect())
        .unwrap_or_default();
    (files, body)
}

fn gh_text(dir: &Path, args: &[&str]) -> Option<String> {
    let exe = procs::find("gh")?;
    let out = procs::command(&exe)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// One short headless turn on the reviewer's profile when one is configured, otherwise the default
/// profile. Quiet mode plus the default oneshot source keeps it out of the owner's chat list.
fn run_lessons_turn(dir: &Path, qpath: &Path) -> Result<(String, Option<String>)> {
    let mut args: Vec<String> = crate::hermes_surfaces::profile_for_role("reviewer")
        .map(|p| vec!["-p".into(), p])
        .unwrap_or_default();
    args.extend(["chat", "-Q", "--oneshot", "--query-file"].map(String::from));
    args.push(qpath.display().to_string());
    args.extend(["--in".into(), dir.display().to_string()]);
    let exe = procs::find("hermes")
        .ok_or_else(|| anyhow!("hermes is not installed, so it cannot extract lessons"))?;
    let out = procs::command(&exe)
        .args(&args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .output()?;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() && text.trim().is_empty() {
        bail!("the learning turn failed: {}", stderr.trim());
    }
    if text.trim().is_empty() {
        bail!("the learning turn returned nothing: {}", stderr.trim());
    }
    Ok((text, crate::review_agent::session_id_in(&stderr)))
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

    fn with_db<T>(f: impl FnOnce() -> T) -> T {
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let out = f();
        crate::store::set_test_db(None);
        out
    }

    fn mission() -> Mission {
        Mission {
            id: "M-001".into(),
            repo: "agentos-studio".into(),
            goal: "Add the learning loop".into(),
            acceptance: "A finished mission yields at least one note".into(),
            role: "implementer".into(),
            agent: "hermes".into(),
            status: "done".into(),
            provider: "claude-subscription".into(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: Some("TASK-1".into()),
            reviewer: Some("qoder".into()),
            review_verdict: Some("approve".into()),
            review_note: Some("covers the empty case".into()),
            pr_url: Some("https://github.com/konethegreat/agentos-studio/pull/42".into()),
            created_at: 0,
            updated_at: 0,
            verify: None,
        }
    }

    fn seed_reject(repo: &str, note: &str, at: i64) -> String {
        let m = missions::create(
            repo,
            "Fix the parser",
            "Handles empty input",
            "implementer",
            "hermes",
            "",
            "",
            0.0,
        )
        .unwrap();
        let c = store::open().unwrap();
        c.execute(
            "UPDATE missions SET status = 'queued', reviewer = 'qoder', review_verdict = 'reject',
                review_note = ?1, updated_at = ?2 WHERE id = ?3",
            params![note, at, m.id],
        )
        .unwrap();
        m.id
    }

    #[test]
    fn lessons_parse_good_bad_and_partial() {
        let good = "Here is the answer:\n```json\n[{\"type\":\"gotcha\",\"topic\":\"Guard port\",\
\"content\":\"47823 is taken by the single instance guard.\"},\
{\"type\":\"decision\",\"topic\":\"Index store\",\"content\":\"Derive it from the vault path.\"}]\n```";
        let lessons = parse_lessons(good).unwrap();
        assert_eq!(lessons.len(), 2);
        assert_eq!(lessons[0].kind, "gotcha");
        assert_eq!(lessons[1].topic, "Index store");

        assert!(parse_lessons("no json here").is_err());
        assert!(parse_lessons("[{\"type\":\"idea\",\"topic\":\"t\",\"content\":\"c\"}]").is_err());
        assert!(
            parse_lessons("[{\"type\":\"gotcha\",\"topic\":\" \",\"content\":\"c\"}]").is_err()
        );
        // One bad entry refuses the whole answer: a partial set is never written.
        assert!(parse_lessons(
            "[{\"type\":\"gotcha\",\"topic\":\"t\",\"content\":\"c\"},\
             {\"type\":\"fact\",\"topic\":\"t\",\"content\":\"c\"}]"
        )
        .is_err());
        assert!(parse_lessons("[]").unwrap().is_empty());
    }

    #[test]
    fn the_prompt_names_the_mission_and_the_contract() {
        let p = lessons_prompt(&mission(), "src/a.rs", "Makes the loop idempotent.");
        assert!(p.contains("Add the learning loop"));
        assert!(p.contains("A finished mission yields at least one note"));
        assert!(p.contains("covers the empty case"));
        assert!(p.contains("src/a.rs"));
        assert!(p.contains("Makes the loop idempotent."));
        assert!(p.contains("\"type\":\"decision\""));
    }

    #[test]
    fn reject_reasons_prefer_the_blocker_list() {
        let note =
            "Two blockers:\n- Tests do not cover the empty case.\n- The route is not documented.";
        assert_eq!(
            reject_reasons(note),
            vec![
                "tests do not cover the empty case",
                "the route is not documented"
            ]
        );
        // A note without a list falls back to its normalised first sentence.
        assert_eq!(
            reject_reasons("Tests DO NOT cover the empty case. Add one."),
            vec!["tests do not cover the empty case"]
        );
        // Order and numbering do not change the key; spelling and punctuation do not either.
        assert_eq!(reason_key("1. two\n2. one"), reason_key("- One\n- Two."));
        assert_eq!(reason_key("   "), None);
    }

    #[test]
    fn two_matching_rejects_write_one_convention_and_a_rerun_changes_nothing() {
        with_db(|| {
            let (_d, v) = vault();
            let repo = "learning-test-repo";
            let now = chrono::Utc::now().timestamp();
            let first = seed_reject(
                repo,
                "Two blockers:\n- Tests do not cover the empty case.\n- The route is not documented.",
                now,
            );
            // Same review blockers, different punctuation and order: this is the same reason.
            let second = seed_reject(
                repo,
                "1. The route is not documented\n2. tests DO NOT cover the empty case",
                now,
            );
            // Different reasons, so this one is not part of the group.
            seed_reject(repo, "The name is wrong.", now);

            let written = repeated_rejects(&v, repo).unwrap();
            assert_eq!(written.len(), 1, "{written:?}");
            let path = v.memory().join("Review_blockers_in_learning_test_repo.md");
            let before = std::fs::read_to_string(&path).unwrap();
            assert!(before.contains(&format!("[[{first}]]")), "{before}");
            assert!(before.contains(&format!("[[{second}]]")), "{before}");
            assert!(!before.contains("name is wrong"));

            let again = repeated_rejects(&v, repo).unwrap();
            assert!(again.is_empty(), "a rerun writes nothing: {again:?}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        });
    }

    #[test]
    fn one_reject_is_below_the_threshold_and_old_rejects_are_ignored() {
        with_db(|| {
            let (_d, v) = vault();
            let now = chrono::Utc::now().timestamp();
            seed_reject("solo-repo", "Tests do not cover the empty case.", now);
            assert!(repeated_rejects(&v, "solo-repo").unwrap().is_empty());

            let old = now - REJECT_WINDOW_SECS - 60;
            seed_reject("old-repo", "Tests do not cover the empty case.", old);
            seed_reject("old-repo", "Tests do not cover the empty case.", old);
            assert!(repeated_rejects(&v, "old-repo").unwrap().is_empty());
        });
    }

    #[test]
    fn conventions_are_repo_scoped_newest_first_and_capped() {
        with_db(|| {
            let (_d, v) = vault();
            for i in 0..8 {
                let content = format!("Convention {i}. ") + &"Always run the checks. ".repeat(20);
                let topic = format!("Convention {i}");
                v.write_typed_memory(
                    &topic,
                    &content,
                    "convention",
                    &MemMeta {
                        kind: Some(MemKind::Convention),
                        repo: Some("repo-a".into()),
                        ..Default::default()
                    },
                )
                .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            v.write_typed_memory(
                "Other repo",
                "Other repo rule.",
                "convention",
                &MemMeta {
                    kind: Some(MemKind::Convention),
                    repo: Some("repo-b".into()),
                    ..Default::default()
                },
            )
            .unwrap();

            let text = conventions_for(&v, "repo-a");
            assert!(
                text.chars().count() <= CONVENTIONS_CAP,
                "{} chars",
                text.chars().count()
            );
            assert!(text.starts_with("- Convention 7:"), "newest first: {text}");
            assert!(!text.contains("Other repo rule"));
            assert!(conventions_for(&v, "nobody").is_empty());
        });
    }

    #[test]
    fn an_extracted_mission_is_skipped_without_running_anything() {
        with_db(|| {
            let (_d, v) = vault();
            let c = store::open().unwrap();
            store::kv_set(&c, "learning.done.M-001", "1").unwrap();
            let report = lessons_for(&v, &mission()).unwrap();
            assert!(report.skipped);
            assert!(report.notes.is_empty());
            assert!(report.dropped.is_none());
        });
    }

    #[test]
    fn only_a_finished_mission_can_teach() {
        with_db(|| {
            let (_d, v) = vault();
            let mut m = mission();
            m.status = "queued".into();
            assert!(lessons_for(&v, &m).is_err());
        });
    }

    fn finished_in(dir: &std::path::Path) -> Mission {
        let mut m = mission();
        m.repo = dir.display().to_string();
        m.pr_url = None;
        m
    }

    #[test]
    fn a_valid_turn_writes_notes_and_a_rerun_runs_nothing() {
        with_db(|| {
            let (_d, v) = vault();
            let dir = tempfile::tempdir().unwrap();
            let m = finished_in(dir.path());
            let answer = "```json\n[{\"type\":\"gotcha\",\"topic\":\"Guard port\",\
\"content\":\"47823 is taken by the single instance guard.\"}]\n```";
            let fake = |workspace: &Path, query: &Path| {
                assert_eq!(workspace, dir.path());
                let prompt = std::fs::read_to_string(query).unwrap();
                assert!(prompt.contains("Add the learning loop"), "{prompt}");
                Ok((answer.to_string(), Some("sess-1".to_string())))
            };
            let report = lessons_with(&v, &m, &fake).unwrap();
            assert!(!report.skipped);
            assert_eq!(report.notes.len(), 1, "{:?}", report.notes);
            assert!(
                report.notes[0].contains("Guard_port.md"),
                "{:?}",
                report.notes
            );
            assert_eq!(report.session.as_deref(), Some("sess-1"));
            assert!(report.dropped.is_none());

            let written: Vec<String> = v
                .list_memory_notes()
                .into_iter()
                .filter_map(|n| v.read_note(&n.relative_path).ok())
                .filter(|text| text.contains("agent: agentos-learning"))
                .collect();
            assert_eq!(written.len(), 1, "{written:?}");
            let doc = crate::vault::Doc::parse(&written[0]);
            assert_eq!(doc.get("repo"), Some(m.repo.as_str()), "{written:?}");
            assert_eq!(doc.get("source_session"), Some("sess-1"));
            assert_eq!(doc.get("type"), Some("gotcha"));
            assert_eq!(doc.get("agent"), Some("agentos-learning"));

            let again = lessons_with(&v, &m, &|_: &Path, _: &Path| {
                panic!("an extracted mission must not run the turn again")
            })
            .unwrap();
            assert!(again.skipped);
            assert!(again.notes.is_empty());
        });
    }

    #[test]
    fn an_invalid_answer_is_dropped_and_a_retry_is_allowed() {
        with_db(|| {
            let (_d, v) = vault();
            let dir = tempfile::tempdir().unwrap();
            let m = finished_in(dir.path());
            let bad = lessons_with(&v, &m, &|_: &Path, _: &Path| {
                Ok((
                    "I think the work was fine.".to_string(),
                    Some("sess-2".to_string()),
                ))
            })
            .unwrap();
            assert!(bad.dropped.is_some(), "{bad:?}");
            assert!(bad.notes.is_empty());
            assert!(!bad.skipped);

            // A dropped answer is not recorded as done, so the next run may try again.
            let answer = "[{\"type\":\"decision\",\"topic\":\"Retry budget\",\
\"content\":\"One retry per transient failure.\"}]";
            let good = lessons_with(&v, &m, &|_: &Path, _: &Path| {
                Ok((answer.to_string(), Some("sess-3".to_string())))
            })
            .unwrap();
            assert!(!good.skipped);
            assert_eq!(good.notes.len(), 1);
            assert_eq!(good.session.as_deref(), Some("sess-3"));
        });
    }
}
