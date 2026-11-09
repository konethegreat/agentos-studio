//! Provenance: why is this line here? A mission merge can append a trailer block to its commit
//! message, and this module reads a blamed line back to the commit, its trailers and the mission
//! that produced it. When a commit has no trailer (a human commit, or work merged before the
//! trailers existed) the merged pull request that carries the commit is matched to a mission by
//! its `pr_url`. Read only: git blame, git log and `gh pr list`.

use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::missions::{self, Mission};

const LIMIT: Duration = Duration::from_secs(30);
const MISSION_SCAN: usize = 500;
const MAX_SESSIONS: usize = 8;
const MAX_PR_CANDIDATES: u32 = 10;

/// The trailer block a mission merge appends to its commit message.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Trailers {
    pub mission: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub reviewer: Option<String>,
    pub cost: Option<String>,
}

/// `flat` for subscription work, `$0.00` for a local model, and the measured spend otherwise.
pub fn cost_label(m: &Mission) -> String {
    match crate::agents::cost_model(&m.agent, &m.provider) {
        "flat" => "flat".to_string(),
        "local" => "$0.00".to_string(),
        _ => format!("${:.2}", m.spent_usd.max(0.0)),
    }
}

fn model_label(m: &Mission) -> &str {
    let model = m.model.trim();
    if model.is_empty() {
        "unknown"
    } else {
        model
    }
}

fn reviewer_label(m: &Mission) -> &str {
    m.reviewer
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("none")
}

/// The trailer block for a finished mission. `Goal:` waits for Phase D, which introduces goals.
pub fn trailers(m: &Mission) -> String {
    format!(
        "Mission: {}\nAgent: {}\nModel: {}\nReviewer: {}\nCost: {}\n",
        m.id,
        m.agent,
        model_label(m),
        reviewer_label(m),
        cost_label(m),
    )
}

/// Read the known trailers from a commit message. Unknown keys and prose are ignored; the last
/// occurrence of a key wins, so a trailer block appended at the end overrides an earlier line.
pub fn parse_trailers(message: &str) -> Trailers {
    let mut t = Trailers::default();
    for line in message.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let slot = match key.trim() {
            "Mission" => &mut t.mission,
            "Agent" => &mut t.agent,
            "Model" => &mut t.model,
            "Reviewer" => &mut t.reviewer,
            "Cost" => &mut t.cost,
            _ => continue,
        };
        *slot = Some(value.to_string());
    }
    t
}

#[derive(Serialize, Clone, Debug)]
pub struct MissionRef {
    pub id: String,
    pub goal: String,
    pub status: String,
    pub agent: String,
    pub model: String,
    pub reviewer: Option<String>,
    pub cost: String,
    pub pr_url: Option<String>,
    pub sessions: Vec<String>,
}

impl MissionRef {
    fn of(m: &Mission) -> Self {
        MissionRef {
            id: m.id.clone(),
            goal: m.goal.clone(),
            status: m.status.clone(),
            agent: m.agent.clone(),
            model: model_label(m).to_string(),
            reviewer: m.reviewer.clone().filter(|r| !r.trim().is_empty()),
            cost: cost_label(m),
            pr_url: m.pr_url.clone(),
            sessions: sessions_for(&m.id),
        }
    }
}

/// One blamed line and what produced it. `source` says how the mission was found:
/// `trailers`, `pull request` or `none`.
#[derive(Serialize, Clone, Debug)]
pub struct Provenance {
    pub repo: String,
    pub file: String,
    pub line: u64,
    pub sha: String,
    pub short: String,
    pub subject: String,
    pub committed_at: i64,
    pub source: String,
    pub trailers: Trailers,
    pub mission: Option<MissionRef>,
    pub pull_request: Option<String>,
    /// Set when the merged-pull-request lookup could not run (no gh, no GitHub remote, offline).
    pub note: Option<String>,
}

/// The Hermes sessions the mission's exchange log names, newest last, without repeats.
fn sessions_for(id: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    crate::agent_log::for_mission(id)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            let (_, sid) = e.detail.rsplit_once("Hermes session: ")?;
            let sid = sid.trim().to_string();
            (!sid.is_empty() && seen.insert(sid.clone())).then_some(sid)
        })
        .take(MAX_SESSIONS)
        .collect()
}

/// Reject anything that could leave the repository: absolute paths, `..`, drive letters and
/// anything that is not a plain relative path. Backslashes are accepted and normalised.
fn clean_file(file: &str) -> Result<String> {
    let f = file.trim().replace('\\', "/");
    if f.is_empty() {
        bail!("Give a file path inside the repository");
    }
    if f.contains('\0') || f.contains('\n') || f.contains('\r') {
        bail!("The file path contains a control character");
    }
    let p = Path::new(&f);
    let drive = f.len() > 2 && f.as_bytes()[1] == b':' && f.as_bytes()[0].is_ascii_alphabetic();
    if drive || p.is_absolute() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        bail!("'{file}' is outside the repository; give a relative path such as src/main.rs");
    }
    Ok(f)
}

/// The local checkout for a project slug, an absolute folder, or an `owner/name` known to a project.
fn checkout_for(repo: &str) -> Result<std::path::PathBuf> {
    let repo = repo.trim();
    if repo.is_empty() {
        bail!("Name the repository: a project slug or a local folder");
    }
    let as_path = Path::new(repo);
    if as_path.is_absolute() && as_path.is_dir() {
        return Ok(as_path.to_path_buf());
    }
    if let Some(dir) = crate::learning::workspace_for(repo) {
        return Ok(dir);
    }
    if crate::github::valid_repo(repo) {
        if let Some(dir) = project_checkout_for_remote(repo) {
            return Ok(dir);
        }
    }
    bail!("No local checkout for '{repo}'. Use a project slug or the clone's folder path.");
}

/// A project folder whose `origin` remote is this `owner/name`. Small fleet, one git call each.
fn project_checkout_for_remote(repo: &str) -> Option<std::path::PathBuf> {
    for p in crate::projects::list().ok()? {
        let Some(dir) = p
            .primary_path
            .clone()
            .or_else(|| p.folders.first().map(|f| f.path.clone()))
        else {
            continue;
        };
        let path = std::path::PathBuf::from(&dir);
        if !path.is_dir() {
            continue;
        }
        let Ok(out) = crate::procs::run_with_timeout(
            "git",
            &["remote", "get-url", "origin"],
            None,
            Some(&path),
            LIMIT,
        ) else {
            continue;
        };
        if out.0 == Some(0) && remote_matches(out.1.trim(), repo) {
            return Some(path);
        }
    }
    None
}

/// True when a git remote points at `owner/name` (https, ssh or git protocol, with or without .git).
fn remote_matches(remote: &str, repo: &str) -> bool {
    let want = repo.trim_end_matches(".git").to_ascii_lowercase();
    let remote = remote.trim().trim_end_matches('/');
    let tail = if let Some((_, t)) = remote.rsplit_once("github.com:") {
        t
    } else if let Some((_, t)) = remote.rsplit_once("github.com/") {
        t
    } else {
        return false;
    };
    tail.trim_end_matches(".git").eq_ignore_ascii_case(&want)
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let (code, out, err) = crate::procs::run_with_timeout("git", args, None, Some(dir), LIMIT)?;
    if code != Some(0) {
        let msg = err.trim();
        let msg = if msg.is_empty() { out.trim() } else { msg };
        bail!("git {} failed: {msg}", args.first().copied().unwrap_or(""));
    }
    Ok(out)
}

fn real_blame(dir: &Path, file: &str, line: u64) -> Result<String> {
    let range = format!("{line},{line}");
    let out = git(dir, &["blame", "--porcelain", "-L", &range, "--", file])?;
    parse_blame_sha(&out)
}

/// The commit of the first blame record. A line that only exists in the working tree blames to an
/// all-zero id, which is an answer ("not committed yet"), not a failure to read.
fn parse_blame_sha(out: &str) -> Result<String> {
    let header = out.lines().next().unwrap_or("").trim();
    let sha = header.split_whitespace().next().unwrap_or("");
    if sha.len() < 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("git blame returned an unexpected record");
    }
    if sha.chars().all(|c| c == '0') {
        bail!("this line is not committed yet");
    }
    Ok(sha.to_string())
}

fn real_message(dir: &Path, sha: &str) -> Result<(String, i64)> {
    let out = git(dir, &["log", "-1", "--format=%ct%x00%B", sha])?;
    let (at, body) = out
        .split_once('\0')
        .ok_or_else(|| anyhow!("git log returned an unexpected record"))?;
    Ok((body.trim_end().to_string(), at.trim().parse().unwrap_or(0)))
}

fn gh_repo(repo: &str, dir: &Path) -> Result<String> {
    if crate::github::valid_repo(repo) {
        return Ok(repo.to_string());
    }
    if let Ok(name) = crate::github::name_with_owner(repo) {
        return Ok(name);
    }
    let view = crate::github::repo_for_dir(&dir.to_string_lossy())?;
    view.get("nameWithOwner")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("could not name the remote of {}", dir.display()))
}

fn real_pr_for_sha(repo: &str, dir: &Path, sha: &str) -> Result<Option<String>> {
    let name = gh_repo(repo, dir)?;
    let (code, out, err) = crate::procs::run_with_timeout(
        "gh",
        &[
            "pr",
            "list",
            "-R",
            &name,
            "--state",
            "merged",
            "--search",
            sha,
            "--limit",
            &MAX_PR_CANDIDATES.to_string(),
            "--json",
            "url,mergeCommit,headRefOid",
        ],
        None,
        None,
        LIMIT,
    )?;
    if code != Some(0) {
        let msg = err.trim();
        let msg = if msg.is_empty() { out.trim() } else { msg };
        bail!("gh pr list failed: {msg}");
    }
    let list: Value = serde_json::from_str(out.trim())
        .map_err(|e| anyhow!("gh returned unexpected output: {e}"))?;
    Ok(find_pr_url(&list, sha))
}

/// The URL of the merged pull request whose merge commit or head is this sha.
fn find_pr_url(list: &Value, sha: &str) -> Option<String> {
    list.as_array()?.iter().find_map(|pr| {
        let merge = pr
            .get("mergeCommit")
            .and_then(|c| c.get("oid"))
            .and_then(Value::as_str);
        let head = pr.get("headRefOid").and_then(Value::as_str);
        if merge == Some(sha) || head == Some(sha) {
            pr.get("url").and_then(Value::as_str).map(str::to_string)
        } else {
            None
        }
    })
}

fn mission_for_pr(url: &str) -> Result<Option<Mission>> {
    let want = url.trim().trim_end_matches('/');
    Ok(missions::list(MISSION_SCAN)?.into_iter().find(|m| {
        m.pr_url
            .as_deref()
            .is_some_and(|u| u.trim().trim_end_matches('/') == want)
    }))
}

/// Why is `file:line` here? Resolves the local checkout, blames the line, and reads the commit's
/// trailers first and the merged pull request second.
pub fn why(repo: &str, file: &str, line: u64) -> Result<Provenance> {
    why_with(
        repo,
        file,
        line,
        &real_blame,
        &real_message,
        &real_pr_for_sha,
    )
}

/// Seams for the tests: blame, commit message and merged-pull-request lookup.
type Blame<'a> = dyn Fn(&Path, &str, u64) -> Result<String> + 'a;
type Message<'a> = dyn Fn(&Path, &str) -> Result<(String, i64)> + 'a;
type PrFor<'a> = dyn Fn(&str, &Path, &str) -> Result<Option<String>> + 'a;

fn why_with(
    repo: &str,
    file: &str,
    line: u64,
    blame: &Blame<'_>,
    message: &Message<'_>,
    pr_for_sha: &PrFor<'_>,
) -> Result<Provenance> {
    let file = clean_file(file)?;
    if line == 0 {
        bail!("Line numbers start at 1");
    }
    let dir = checkout_for(repo)?;
    let sha = blame(&dir, &file, line)?;
    if !sha.is_empty() && sha.chars().all(|c| c == '0') {
        bail!("this line is not committed yet");
    }
    let (body, committed_at) = message(&dir, &sha)?;
    let subject = body.lines().next().unwrap_or("").trim().to_string();
    let trailers = parse_trailers(&body);

    let mut mission = match trailers.mission.as_deref() {
        Some(id) => missions::get(id)?.map(|m| MissionRef::of(&m)),
        None => None,
    };
    let mut source = if trailers.mission.is_some() {
        "trailers"
    } else {
        "none"
    };
    let mut pull_request = None;
    let mut note = None;
    if mission.is_none() {
        match pr_for_sha(repo, &dir, &sha) {
            Ok(Some(url)) => {
                mission = mission_for_pr(&url)?.map(|m| MissionRef::of(&m));
                pull_request = Some(url);
                if source == "none" {
                    source = "pull request";
                }
            }
            Ok(None) => {}
            Err(e) => note = Some(format!("Pull request lookup skipped: {e}")),
        }
    }

    Ok(Provenance {
        repo: repo.trim().to_string(),
        file,
        line,
        short: sha.chars().take(10).collect(),
        sha,
        subject,
        committed_at,
        source: source.to_string(),
        trailers,
        mission,
        pull_request,
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::missions;
    use serde_json::json;

    fn mission() -> Mission {
        Mission {
            id: "M-012".into(),
            repo: "agentos-studio".into(),
            goal: "Add one provenance line".into(),
            acceptance: "The panel names the mission".into(),
            role: "implementer".into(),
            agent: "hermes".into(),
            status: "done".into(),
            provider: "openrouter".into(),
            model: "anthropic/claude-sonnet-4".into(),
            budget_usd: 1.0,
            spent_usd: 0.423,
            task_id: None,
            reviewer: Some("qoder".into()),
            review_verdict: Some("approve".into()),
            pr_url: None,
            review_note: None,
            created_at: 100,
            updated_at: 200,
            verify: None,
        }
    }

    fn with_db<T>(f: impl FnOnce() -> T) -> T {
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        let out = f();
        crate::store::set_test_db(None);
        out
    }

    fn checkout() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn trailers_only(sha: &'static str) -> impl Fn(&Path, &str, u64) -> Result<String> {
        move |_, _, _| Ok(sha.to_string())
    }

    fn message(text: String) -> impl Fn(&Path, &str) -> Result<(String, i64)> {
        move |_, _| Ok((text.clone(), 1_700_000_000))
    }

    fn no_pr(_: &str, _: &Path, _: &str) -> Result<Option<String>> {
        Ok(None)
    }

    #[test]
    fn a_mission_round_trips_through_trailers() {
        let m = mission();
        let text = trailers(&m);
        assert!(text.contains("Mission: M-012"));
        assert!(text.contains("Cost: $0.42"));
        let back = parse_trailers(&text);
        assert_eq!(back.mission.as_deref(), Some("M-012"));
        assert_eq!(back.agent.as_deref(), Some("hermes"));
        assert_eq!(back.model.as_deref(), Some("anthropic/claude-sonnet-4"));
        assert_eq!(back.reviewer.as_deref(), Some("qoder"));
        assert_eq!(back.cost.as_deref(), Some("$0.42"));
    }

    #[test]
    fn subscription_work_is_flat_and_local_work_is_free() {
        let mut m = mission();
        m.agent = "claude-code".into();
        assert_eq!(cost_label(&m), "flat");
        assert!(trailers(&m).contains("Cost: flat"));
        m.agent = "hermes".into();
        m.provider = "ollama".into();
        assert_eq!(cost_label(&m), "$0.00");
    }

    #[test]
    fn parse_ignores_prose_and_keeps_the_last_value() {
        let t = parse_trailers(
            "Fix the merge gate\n\nAgent: claude-code\nnot a trailer\nCost: flat\nAgent: qoder\n",
        );
        assert_eq!(t.mission, None);
        assert_eq!(t.reviewer, None);
        assert_eq!(t.agent.as_deref(), Some("qoder"));
        assert_eq!(t.cost.as_deref(), Some("flat"));
    }

    #[test]
    fn why_reads_the_mission_from_trailers() {
        with_db(|| {
            let m = missions::create(
                "agentos-studio",
                "Add provenance",
                "The panel answers",
                "implementer",
                "hermes",
                "openrouter",
                "anthropic/claude-sonnet-4",
                1.0,
            )
            .unwrap();
            let body = format!("{} wrote it\n\n{}", m.agent, trailers(&m));
            let dir = checkout();
            let got = why_with(
                &dir.path().to_string_lossy(),
                "src/main.rs",
                7,
                &trailers_only("abc1234def5678901234567890abcdef12345678"),
                &message(body),
                &no_pr,
            )
            .unwrap();
            assert_eq!(got.source, "trailers");
            assert_eq!(got.file, "src/main.rs");
            assert_eq!(got.line, 7);
            assert_eq!(got.short, "abc1234def");
            assert_eq!(got.subject, "hermes wrote it");
            assert_eq!(got.committed_at, 1_700_000_000);
            let found = got.mission.unwrap();
            assert_eq!(found.id, m.id);
            assert_eq!(found.cost, "$0.00");
        });
    }

    #[test]
    fn why_falls_back_to_the_merged_pull_request() {
        with_db(|| {
            let m = missions::create(
                "agentos-studio",
                "Add provenance",
                "The panel answers",
                "implementer",
                "hermes",
                "openrouter",
                "anthropic/claude-sonnet-4",
                1.0,
            )
            .unwrap();
            missions::set_pr(&m.id, "https://github.com/o/r/pull/7").unwrap();
            let dir = checkout();
            let pr = |_: &str, _: &Path, _: &str| Ok(Some("https://github.com/o/r/pull/7".into()));
            let got = why_with(
                &dir.path().to_string_lossy(),
                "src/main.rs",
                1,
                &trailers_only("abc1234def5678901234567890abcdef12345678"),
                &message("Human commit".into()),
                &pr,
            )
            .unwrap();
            assert_eq!(got.source, "pull request");
            assert_eq!(
                got.pull_request.as_deref(),
                Some("https://github.com/o/r/pull/7")
            );
            assert_eq!(got.mission.expect("the PR maps to a mission").id, m.id);
        });
    }

    #[test]
    fn a_pull_request_without_a_mission_still_names_the_commit() {
        with_db(|| {
            let dir = checkout();
            let pr = |_: &str, _: &Path, _: &str| Ok(Some("https://github.com/o/r/pull/9".into()));
            let got = why_with(
                &dir.path().to_string_lossy(),
                "src/main.rs",
                1,
                &trailers_only("abc1234def5678901234567890abcdef12345678"),
                &message("Human commit".into()),
                &pr,
            )
            .unwrap();
            assert_eq!(got.source, "pull request");
            assert!(got.mission.is_none());
            assert_eq!(got.subject, "Human commit");
        });
    }

    #[test]
    fn a_missing_mission_still_reports_the_trailers() {
        with_db(|| {
            let dir = checkout();
            let got = why_with(
                &dir.path().to_string_lossy(),
                "src/main.rs",
                1,
                &trailers_only("abc1234def5678901234567890abcdef12345678"),
                &message("Work\n\nMission: M-999\nAgent: hermes\n".into()),
                &no_pr,
            )
            .unwrap();
            assert_eq!(got.source, "trailers");
            assert_eq!(got.trailers.mission.as_deref(), Some("M-999"));
            assert!(got.mission.is_none());
        });
    }

    #[test]
    fn an_uncommitted_line_is_an_explicit_error() {
        with_db(|| {
            let dir = checkout();
            let err = why_with(
                &dir.path().to_string_lossy(),
                "src/main.rs",
                1,
                &trailers_only("0000000000000000000000000000000000000000"),
                &message("x".into()),
                &no_pr,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("not committed yet"), "{err}");
        });
    }

    #[test]
    fn a_path_outside_the_repository_is_rejected() {
        for bad in ["", "../x.rs", "/etc/passwd", "C:/Windows/x.rs", "a/../../b"] {
            assert!(clean_file(bad).is_err(), "{bad:?} should be refused");
        }
        assert_eq!(clean_file("src\\main.rs").unwrap(), "src/main.rs");
        assert_eq!(clean_file(" src/main.rs ").unwrap(), "src/main.rs");
    }

    #[test]
    fn an_unknown_repository_says_what_to_pass() {
        with_db(|| {
            let err = checkout_for("no-such-project").unwrap_err().to_string();
            assert!(err.contains("project slug"), "{err}");
        });
    }

    #[test]
    fn a_failed_pull_request_lookup_is_a_note_not_an_error() {
        with_db(|| {
            let dir = checkout();
            let pr = |_: &str, _: &Path, _: &str| Err(anyhow!("gh is not installed"));
            let got = why_with(
                &dir.path().to_string_lossy(),
                "src/main.rs",
                1,
                &trailers_only("abc1234def5678901234567890abcdef12345678"),
                &message("Human commit".into()),
                &pr,
            )
            .unwrap();
            assert_eq!(got.source, "none");
            assert!(got.mission.is_none());
            assert!(
                got.note.unwrap_or_default().contains("gh is not installed"),
                "the reason the lookup was skipped should be shown"
            );
        });
    }

    #[test]
    fn a_malformed_blame_record_is_not_a_commit() {
        assert!(parse_blame_sha("").is_err());
        assert!(parse_blame_sha("not-a-sha 1 1 1").is_err());
        let zero = "0000000000000000000000000000000000000000 1 1 1";
        assert!(parse_blame_sha(zero)
            .unwrap_err()
            .to_string()
            .contains("not committed"));
    }

    #[test]
    fn the_merged_pull_request_matches_its_merge_commit_or_head() {
        let list = json!([
            { "url": "https://github.com/o/r/pull/1", "mergeCommit": { "oid": "aaa" }, "headRefOid": "bbb" },
            { "url": "https://github.com/o/r/pull/2", "mergeCommit": null, "headRefOid": "ccc" },
        ]);
        assert_eq!(
            find_pr_url(&list, "aaa").as_deref(),
            Some("https://github.com/o/r/pull/1")
        );
        assert_eq!(
            find_pr_url(&list, "ccc").as_deref(),
            Some("https://github.com/o/r/pull/2")
        );
        assert_eq!(find_pr_url(&list, "ddd"), None);
    }

    #[test]
    fn a_remote_matches_with_or_without_the_git_suffix() {
        assert!(remote_matches("git@github.com:o/r.git", "o/r"));
        assert!(remote_matches("https://github.com/O/R", "o/r"));
        assert!(!remote_matches("git@github.com:o/other.git", "o/r"));
        assert!(!remote_matches("https://gitlab.com/o/r.git", "o/r"));
    }
}
