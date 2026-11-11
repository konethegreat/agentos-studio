//! The reviewer: a second agent, on another provider where possible, that judges the work.
//!
//! It is given the mission's acceptance criteria and the actual pull request diff, and must answer
//! with a JSON object. Anything else is treated as a rejection, because silence is not approval.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Result};
use serde_json::Value;

use crate::missions::{self, Mission};
use crate::procs;
use crate::projects;
use crate::vault::Vault;

const MAX_DIFF: usize = 12_000;

pub fn prompt(m: &Mission, diff: &str) -> String {
    prompt_full(m, diff, "", "", "")
}

/// The reviewer's prompt with the evidence Phase D gives it: the CI checks, the `hermes verify`
/// result and the repository's conventions from memory. Empty parts are left out. Pure.
pub fn prompt_full(
    m: &Mission,
    diff: &str,
    checks: &str,
    verify: &str,
    conventions: &str,
) -> String {
    let mut evidence = String::new();
    if !checks.trim().is_empty() {
        evidence.push_str(&format!(
            "CI checks on the pull request:\n{}\n\n",
            checks.trim()
        ));
    }
    if !verify.trim().is_empty() {
        evidence.push_str(&format!(
            "Test run in the repository (hermes verify):\n{}\n\n",
            verify.trim()
        ));
    }
    if !conventions.trim().is_empty() {
        evidence.push_str(&format!(
            "Conventions from earlier reviews in this repository:\n{}\n\n",
            conventions.trim()
        ));
    }
    format!(
        "You are the reviewer for a mission. You did not write this code. Judge only what is below.\n\n\
Goal:\n{goal}\n\nAcceptance criteria:\n{acceptance}\n\n{evidence}The pull request diff:\n```diff\n{diff}\n```\n\n\
Reply with a single JSON object and nothing else:\n\
{{\"verdict\":\"approve\" or \"reject\",\"note\":\"one or two sentences a developer can act on\"}}\n\
Approve only if the diff satisfies every acceptance criterion. If tests are missing or the diff \
touches unrelated files, reject.",
        goal = m.goal,
        acceptance = m.acceptance,
        diff = diff
    )
}

/// A short line from a recorded `hermes verify --json` result for the prompt. Pure.
pub fn verify_line(json: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(json.trim()) else {
        return String::new();
    };
    if v.get("error").and_then(Value::as_str) == Some("no-recipe") {
        return String::new();
    }
    if v.get("error").and_then(Value::as_str) == Some("not-run") {
        let why = v
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("no worktree");
        return format!("not run: {why}");
    }
    let ok = v.get("ok").and_then(Value::as_bool);
    let phases: Vec<String> = v
        .get("phases")
        .and_then(Value::as_array)
        .map(|ps| {
            ps.iter()
                .map(|p| {
                    format!(
                        "{} ({}): {}",
                        p.get("phase").and_then(Value::as_str).unwrap_or("?"),
                        p.get("command").and_then(Value::as_str).unwrap_or(""),
                        if p.get("ok").and_then(Value::as_bool) == Some(true) {
                            "passed"
                        } else {
                            "failed"
                        }
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    match (ok, phases.is_empty()) {
        (Some(true), true) => "passed".into(),
        (Some(false), true) => "failed".into(),
        (_, false) => phases.join("; "),
        _ => String::new(),
    }
}

/// Pull the verdict out of whatever the model said, tolerating prose around the object.
pub fn parse_verdict(text: &str) -> Option<(String, String)> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let v: Value = serde_json::from_str(&text[start..=end]).ok()?;
    let verdict = v.get("verdict")?.as_str()?.trim().to_lowercase();
    let note = v.get("note")?.as_str()?.trim().to_string();
    if verdict != "approve" && verdict != "reject" {
        return None;
    }
    if note.is_empty() {
        return None;
    }
    Some((verdict, note))
}

/// The repository's folder from Hermes projects, with no fallback. Anything that lets an agent
/// work in the folder (kanban workers, `hermes verify`) must use this: the home folder is never a
/// valid workspace.
pub fn project_dir(repo: &str) -> Option<PathBuf> {
    if let Ok(list) = projects::list() {
        if let Some(p) = list
            .iter()
            .find(|p| p.slug.eq_ignore_ascii_case(repo) || p.name.eq_ignore_ascii_case(repo))
        {
            if let Some(path) = p
                .primary_path
                .clone()
                .or_else(|| p.folders.first().map(|f| f.path.clone()))
            {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

/// Run the reviewer and record its verdict. Returns the updated mission.
pub fn review(v: &Vault, id: &str, reviewer: &str) -> Result<Mission> {
    let m = missions::get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    if m.status != "review" {
        bail!(
            "{} is {}; only a mission in review can be judged",
            id,
            m.status
        );
    }
    let number = missions::pr_number(&m)
        .ok_or_else(|| anyhow::anyhow!("{} has no pull request to review", m.id))?;
    // The reviewer runs in the project folder, never in the home folder.
    let dir = project_dir(&m.repo).filter(|d| d.is_dir()).ok_or_else(|| {
        anyhow::anyhow!(
            "repository '{}' is not a known project with a folder, so the reviewer has nowhere to run",
            m.repo
        )
    })?;
    // A mission stores the project slug. `gh` can resolve the repo from the working folder, so use
    // that when the value is not `owner/name`.
    let diff = if m.repo.contains('/') {
        crate::github::pr_diff(&m.repo, number).unwrap_or_default()
    } else {
        let mut cmd = Command::new("gh");
        cmd.args(["pr", "diff", &number.to_string()])
            .current_dir(&dir)
            .env("PATH", procs::search_path());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(procs::CREATE_NO_WINDOW);
        }
        cmd.output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    };
    if diff.trim().is_empty() {
        bail!("could not read the diff of {}#{number}", m.repo);
    }
    // The constitution gate runs before any reviewer turn: a pull request that breaks a law goes
    // back to its implementer with the findings, through the same reject path as a reviewer.
    let findings = crate::constitution_gate::check_diff(&diff);
    if !findings.is_empty() {
        let note = crate::constitution_gate::note(&findings);
        let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
            mission_id: Some(id),
            kind: "note",
            from_agent: "agentos-studio",
            summary: &format!(
                "constitution gate: {} finding(s) on {}#{number}; sent back to {}",
                findings.len(),
                m.repo,
                m.agent
            ),
            ..Default::default()
        });
        return missions::review(id, crate::constitution_gate::REVIEWER, "reject", &note);
    }
    let diff: String = diff.chars().take(MAX_DIFF).collect();
    // The evidence the reviewer judges with, besides the diff. Each part is best effort: a missing
    // part is left out of the prompt, never invented.
    let checks = crate::github::name_with_owner(&m.repo)
        .and_then(|r| crate::github::pr_check_runs(&r, number))
        .map(|runs| crate::github::checks_summary(&runs))
        .unwrap_or_default();
    let verify = m.verify.as_deref().map(verify_line).unwrap_or_default();
    let conventions = crate::learning::conventions_for(v, &m.repo);

    let prompt_text = prompt_full(&m, &diff, &checks, &verify, &conventions);
    if reviewer.eq_ignore_ascii_case("antigravity") {
        let (text, _, cost) = crate::ask::run_turn(
            "antigravity",
            &prompt_text,
            &dir,
            std::time::Duration::from_secs(20 * 60),
        )?;
        if cost > 0.0 {
            missions::set_spent(id, m.spent_usd + cost)?;
        }
        let (verdict, note) = parse_verdict(&text).ok_or_else(|| {
            anyhow::anyhow!(
                "the reviewer did not return a verdict. It said: {}",
                text.chars().take(300).collect::<String>()
            )
        })?;
        return missions::review(id, reviewer, &verdict, &note);
    }
    let qfile = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("agentos-studio")
        .join("review");
    std::fs::create_dir_all(&qfile)?;
    let qpath = qfile.join(format!("{id}.md"));
    std::fs::write(&qpath, &prompt_text)?;

    let (bin, args): (&str, Vec<String>) = if reviewer.eq_ignore_ascii_case("claude-code") {
        ("claude", vec!["-p".into(), prompt_text.clone()])
    } else {
        // The reviewer profile chosen in MCP and Providers > Hermes, so the review runs with its
        // own model and memory rather than the implementer's.
        let mut a: Vec<String> = crate::hermes_surfaces::profile_for_role("reviewer")
            .map(|p| vec!["-p".into(), p])
            .unwrap_or_default();
        a.extend(["chat", "-Q", "--oneshot", "--query-file"].map(String::from));
        a.push(qpath.display().to_string());
        a.extend(["--in".into(), dir.display().to_string()]);
        ("hermes", a)
    };
    let exe = procs::find(bin)
        .ok_or_else(|| anyhow::anyhow!("{bin} is not installed, so it cannot review"))?;
    let mut cmd = Command::new(exe);
    cmd.args(&args)
        .current_dir(&dir)
        .env("PATH", procs::search_path())
        .env("NO_COLOR", "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(procs::CREATE_NO_WINDOW);
    }
    let out = cmd.output()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() && text.trim().is_empty() {
        bail!(
            "the reviewer failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // A Hermes review prints its session id on stderr; its metered cost counts toward the budget.
    if bin == "hermes" {
        if let Some(sid) = session_id_in(&String::from_utf8_lossy(&out.stderr)) {
            let cost = crate::usage::metered_cost(&[sid]);
            if cost > 0.0 {
                missions::set_spent(id, m.spent_usd + cost)?;
            }
        }
    }
    let (verdict, note) = parse_verdict(&text).ok_or_else(|| {
        anyhow::anyhow!(
            "the reviewer did not return a verdict. It said: {}",
            text.chars().take(300).collect::<String>()
        )
    })?;
    missions::review(id, reviewer, &verdict, &note)
}

/// The gatekeeper: recompute every merge condition, then squash merge and delete the branch.
/// Refuses with the full blocker list. `allowlisted` is the board's Allow merge switch.
pub fn merge(id: &str, allowlisted: bool) -> Result<String> {
    let m = missions::get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    let number =
        missions::pr_number(&m).ok_or_else(|| anyhow::anyhow!("{} has no pull request", m.id))?;
    // Missions store the project slug; gh needs owner/name.
    let repo = crate::github::name_with_owner(&m.repo)?;
    let green = crate::github::pr_checks_green(&repo, number).unwrap_or(false);
    let mut blockers = missions::merge_blockers(&m, green, allowlisted);
    // The gate again, on the diff as it is now: commits pushed after the review are judged too.
    match crate::constitution_gate::check_pr(&repo, number) {
        Ok(f) if f.is_empty() => {}
        Ok(f) => blockers.push(format!(
            "constitution gate: {}",
            crate::constitution_gate::note(&f).trim()
        )),
        Err(e) => blockers.push(format!("constitution gate could not read the diff: {e}")),
    }
    if !blockers.is_empty() {
        bail!("not merging: {}", blockers.join("; "));
    }
    // The squash body keeps the pull request's description and names the mission behind the line.
    // A pull request whose title or body cannot be read is not merged: the trailers are the record
    // "Why is this here?" resolves, and a merge without them would lose the mission.
    let (title, body) = crate::github::pr_title_and_body(&repo, number)?;
    let mut message = body;
    if !message.is_empty() {
        message.push_str("\n\n");
    }
    message.push_str(&crate::provenance::trailers(&m));
    let subject = crate::github::squash_subject(&title, number);
    crate::github::pr_merge_with(
        &repo,
        number,
        "squash",
        true,
        true,
        subject.as_deref(),
        Some(&message),
    )
}

/// The provider family of an agent's answers, for picking a reviewer on another provider.
fn family(agent: &str) -> String {
    crate::council::family(agent, &crate::agents::hermes_provider())
}

/// The automatic reviewer for work done by `implementer`: the first available headless agent on a
/// different provider family (Antigravity, then Claude Code, then Hermes), else any other
/// available agent. The bool says whether the provider differs. Never the implementer. Pure over
/// `family_of` and `available`.
pub fn pick_reviewer(
    implementer: &str,
    family_of: impl Fn(&str) -> String,
    available: impl Fn(&str) -> bool,
) -> Option<(String, bool)> {
    let imp = implementer.trim().to_lowercase();
    let mine = family_of(&imp);
    let candidates: Vec<&str> = ["antigravity", "claude-code", "hermes"]
        .into_iter()
        .filter(|a| *a != imp && available(a))
        .collect();
    candidates
        .iter()
        .find(|a| family_of(a) != mine)
        .map(|a| (a.to_string(), true))
        .or_else(|| candidates.first().map(|a| (a.to_string(), false)))
}

/// kv key for the automatic review of the mission's current round.
pub fn auto_key(id: &str) -> String {
    format!("review.auto.{id}")
}

const AUTO_STALE: i64 = 45 * 60;
const AUTO_RETRY_AFTER: i64 = 10 * 60;
/// Failed automatic reviews of one round before the mission is blocked.
pub const AUTO_TRIES: u32 = 2;

/// Should the scheduler start an automatic review now? `state` is kv `review.auto.<id>`:
/// absent (not started this round), `running:<epoch>`, `failed:<epoch>:<tries>:<reason>` or
/// `done`. A running review older than 45 minutes is stale; a failed one is retried once after
/// 10 minutes. Pure.
pub fn auto_review_due(m: &Mission, state: Option<&str>, now: i64, eligible: bool) -> bool {
    if !eligible || m.status != "review" || m.pr_url.is_none() {
        return false;
    }
    let Some(s) = state else {
        return true;
    };
    let mut parts = s.splitn(4, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("running"), Some(t), _) => t.parse::<i64>().map_or(true, |t| now - t > AUTO_STALE),
        (Some("failed"), Some(t), Some(n)) => {
            let tries = n.parse::<u32>().unwrap_or(AUTO_TRIES);
            tries < AUTO_TRIES
                && t.parse::<i64>()
                    .map_or(true, |t| now - t > AUTO_RETRY_AFTER)
        }
        _ => false,
    }
}

/// How many automatic reviews of this round failed, from the kv state. Pure.
pub fn auto_failures(state: Option<&str>) -> u32 {
    state
        .and_then(|s| s.strip_prefix("failed:"))
        .and_then(|s| s.split(':').nth(1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn kv_get(key: &str) -> Option<String> {
    crate::store::open()
        .ok()
        .and_then(|c| crate::store::kv_get(&c, key).ok().flatten())
}

fn kv_set(key: &str, value: &str) {
    if let Ok(c) = crate::store::open() {
        let _ = crate::store::kv_set(&c, key, value);
    }
}

/// Forget the round's automatic review (a retried mission starts a new round).
pub fn clear_auto(id: &str) {
    if let Ok(c) = crate::store::open() {
        let _ = crate::store::kv_delete(&c, &auto_key(id));
    }
}

/// The automatic review state of a mission's current round.
pub fn auto_state(id: &str) -> Option<String> {
    kv_get(&auto_key(id))
}

/// Run one automatic review: pick the reviewer, judge, record the outcome in kv. After the second
/// failure in a round the mission is blocked with the reason, which is an escalation for a goal.
pub fn run_auto(v: &Vault, id: &str) -> Result<Mission> {
    let m = missions::get(id)?.ok_or_else(|| anyhow::anyhow!("no mission '{id}'"))?;
    let tries = auto_failures(auto_state(id).as_deref());
    let picked = pick_reviewer(&m.agent, family, crate::bus::headless_available);
    let Some((reviewer, other_provider)) = picked else {
        let reason = "no other agent is installed to review this work";
        kv_set(
            &auto_key(id),
            &format!("failed:{}:{AUTO_TRIES}:{reason}", crate::store::now()),
        );
        return missions::block(id, &format!("Automatic review: {reason}"), false);
    };
    kv_set(&auto_key(id), &format!("running:{}", crate::store::now()));
    let started = std::time::Instant::now();
    let summary = format!(
        "automatic review by {reviewer}{}",
        if other_provider {
            " (another provider)"
        } else {
            " (same provider family; no other was available)"
        }
    );
    let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
        mission_id: Some(id),
        kind: "note",
        from_agent: "agentos-studio",
        to_agent: &reviewer,
        summary: &summary,
        ..Default::default()
    });
    match review(v, id, &reviewer) {
        Ok(judged) => {
            kv_set(&auto_key(id), "done");
            let _ = crate::agent_log::record(&crate::agent_log::NewEvent {
                mission_id: Some(id),
                kind: "note",
                from_agent: &reviewer,
                to_agent: "agentos-studio",
                summary: &format!(
                    "{reviewer} {}d: {}",
                    judged.review_verdict.as_deref().unwrap_or("judge"),
                    judged.review_note.as_deref().unwrap_or("")
                ),
                duration_ms: Some(started.elapsed().as_millis() as i64),
                ..Default::default()
            });
            Ok(judged)
        }
        Err(e) => {
            let n = tries + 1;
            let reason: String = format!("{e:#}")
                .replace(['\n', '\r'], " ")
                .chars()
                .take(300)
                .collect();
            kv_set(
                &auto_key(id),
                &format!("failed:{}:{n}:{reason}", crate::store::now()),
            );
            if n >= AUTO_TRIES {
                let _ = missions::block(
                    id,
                    &format!("Automatic review failed {n} times: {reason}"),
                    false,
                );
            }
            Err(e)
        }
    }
}

/// Start an automatic review on its own thread (a review takes minutes). Skipped under unit
/// tests, where the thread would escape the thread-local store.
pub fn auto_in_background(vault_root: PathBuf, id: String) {
    if cfg!(test) {
        return;
    }
    kv_set(&auto_key(&id), &format!("running:{}", crate::store::now()));
    std::thread::spawn(move || {
        let v = Vault::new(&vault_root);
        if let Err(e) = run_auto(&v, &id) {
            eprintln!("review: automatic review of {id} failed: {e:#}");
        }
    });
}

/// `session_id: <id>` as Hermes prints it on stderr after a one-shot turn. Pure.
pub fn session_id_in(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .find_map(|l| l.trim().strip_prefix("session_id:"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_session_id_hermes_prints() {
        let e = "Skipping unreadable plugin directory x\nsession_id: local-58238a4e\n";
        assert_eq!(session_id_in(e).as_deref(), Some("local-58238a4e"));
        assert_eq!(session_id_in("nothing"), None);
    }

    fn mission() -> Mission {
        Mission {
            id: "M-001".into(),
            repo: "agentos-studio".into(),
            goal: "Add the scheduler".into(),
            acceptance: "Runs every minute".into(),
            role: "implementer".into(),
            agent: "hermes".into(),
            status: "review".into(),
            provider: "claude-subscription".into(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: Some("TASK-1".into()),
            reviewer: None,
            review_verdict: None,
            review_note: None,
            pr_url: Some("https://github.com/konethegreat/agentos-studio/pull/42".into()),
            created_at: 0,
            updated_at: 0,
            verify: None,
        }
    }

    #[test]
    fn prompt_names_the_criteria_and_the_diff() {
        let p = prompt(&mission(), "+ fn tick() {}");
        assert!(p.contains("Runs every minute"));
        assert!(p.contains("+ fn tick()"));
        assert!(p.contains("\"verdict\""));
    }

    #[test]
    fn verdict_parses_through_prose_and_fences() {
        let (v, n) = parse_verdict(
            "```json\n{\"verdict\":\"approve\",\"note\":\"covers the scheduler\"}\n```",
        )
        .unwrap();
        assert_eq!(v, "approve");
        assert_eq!(n, "covers the scheduler");
        assert!(parse_verdict("{\"verdict\":\"maybe\",\"note\":\"x\"}").is_none());
        assert!(parse_verdict("{\"verdict\":\"approve\",\"note\":\"  \"}").is_none());
        assert!(parse_verdict("no json here").is_none());
    }

    #[test]
    fn the_prompt_carries_checks_verify_and_conventions_when_present() {
        let p = prompt_full(
            &mission(),
            "+ fn tick() {}",
            "- build-and-test: SUCCESS",
            "test (npm test): passed",
            "- Tests are plain assert scripts",
        );
        assert!(p.contains("CI checks on the pull request:\n- build-and-test: SUCCESS"));
        assert!(p.contains("hermes verify):\ntest (npm test): passed"));
        assert!(p.contains("Conventions from earlier reviews"));
        let bare = prompt(&mission(), "+ x");
        assert!(!bare.contains("CI checks") && !bare.contains("Conventions"));
        assert_eq!(
            verify_line(
                r#"{"ok":true,"phases":[{"phase":"test","command":"npm test","ok":true}]}"#
            ),
            "test (npm test): passed"
        );
        assert_eq!(verify_line(r#"{"ok":false,"error":"no-recipe"}"#), "");
        assert_eq!(verify_line("not json"), "");
    }

    fn fam(a: &str) -> String {
        match a {
            "antigravity" => "google".into(),
            _ => "anthropic".into(),
        }
    }

    #[test]
    fn the_reviewer_is_on_another_provider_and_never_the_implementer() {
        let all = |_: &str| true;
        assert_eq!(
            pick_reviewer("claude-code", fam, all),
            Some(("antigravity".into(), true))
        );
        assert_eq!(
            pick_reviewer("hermes", fam, all),
            Some(("antigravity".into(), true))
        );
        assert_eq!(
            pick_reviewer("antigravity", fam, all),
            Some(("claude-code".into(), true))
        );
        let no_agy = |a: &str| a != "antigravity";
        assert_eq!(
            pick_reviewer("claude-code", fam, no_agy),
            Some(("hermes".into(), false)),
            "same family only as a last resort, and said so"
        );
        assert_eq!(pick_reviewer("hermes", fam, |a: &str| a == "hermes"), None);
    }

    #[test]
    fn an_automatic_review_runs_once_per_round_with_one_retry() {
        let mut m = mission();
        assert!(auto_review_due(&m, None, 1_000, true));
        assert!(!auto_review_due(&m, None, 1_000, false), "not eligible");
        assert!(!auto_review_due(&m, Some("done"), 1_000, true));
        assert!(!auto_review_due(&m, Some("running:900"), 1_000, true));
        assert!(
            auto_review_due(&m, Some("running:900"), 900 + 46 * 60, true),
            "stale"
        );
        assert!(
            !auto_review_due(&m, Some("failed:900:1:boom"), 1_000, true),
            "too soon"
        );
        assert!(auto_review_due(
            &m,
            Some("failed:900:1:boom"),
            900 + 11 * 60,
            true
        ));
        assert!(
            !auto_review_due(&m, Some("failed:900:2:boom"), 99_999, true),
            "gave up"
        );
        assert_eq!(auto_failures(Some("failed:900:1:a: b")), 1);
        assert_eq!(auto_failures(None), 0);
        m.pr_url = None;
        assert!(!auto_review_due(&m, None, 1_000, true), "nothing to review");
        m.pr_url = Some("https://github.com/o/r/pull/1".into());
        m.status = "running".into();
        assert!(!auto_review_due(&m, None, 1_000, true));
    }

    #[test]
    fn pr_number_comes_from_the_url() {
        assert_eq!(missions::pr_number(&mission()), Some(42));
    }
}
