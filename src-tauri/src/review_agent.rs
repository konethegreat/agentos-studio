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

/// The most diff, in characters, the reviewer is shown. Every reviewer reads its prompt from a file
/// or stdin, never from the command line, and all of them run on models with large context, so the
/// bound only keeps a huge pull request (a vendored folder, a lockfile rewrite) from flooding the
/// turn. Above it the diff is fitted per file by `fit_diff`, never cut at the tail.
pub const MAX_DIFF: usize = 60_000;

/// What every file keeps when a diff is fitted, however little budget is left: its header and its
/// first lines, so no changed file disappears from the prompt.
const FILE_FLOOR: usize = 600;

/// One file's section of a unified diff, from its `diff --git` line to the next one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub text: String,
    pub added: usize,
    pub deleted: usize,
}

/// Split a unified diff (`gh pr diff`, `git diff`) at its `diff --git` lines and count each
/// file's added and deleted lines. Text before the first header is kept as its own section, so
/// nothing is lost. Pure.
pub fn split_diff(diff: &str) -> Vec<FileDiff> {
    let mut sections: Vec<String> = Vec::new();
    for line in diff.split_inclusive('\n') {
        if line.starts_with("diff --git ") || sections.is_empty() {
            sections.push(String::new());
        }
        if let Some(cur) = sections.last_mut() {
            cur.push_str(line);
        }
    }
    sections
        .into_iter()
        .filter(|t| !t.trim().is_empty())
        .map(|text| {
            let (mut added, mut deleted, mut in_hunk) = (0, 0, false);
            for l in text.lines() {
                if l.starts_with("@@") {
                    in_hunk = true;
                } else if in_hunk && l.starts_with('+') {
                    added += 1;
                } else if in_hunk && l.starts_with('-') {
                    deleted += 1;
                }
            }
            FileDiff {
                path: diff_path(&text),
                text,
                added,
                deleted,
            }
        })
        .collect()
}

/// The file a diff section is about: the `+++ b/` path, else the `--- a/` path for a deletion,
/// else the `b/` side of the `diff --git` line. Pure.
fn diff_path(section: &str) -> String {
    let clean = |p: &str| {
        p.trim_end_matches('\t')
            .trim()
            .trim_matches('"')
            .to_string()
    };
    let mut old_side = None;
    for l in section.lines() {
        if l.starts_with("@@") {
            break;
        }
        if let Some(p) = l.strip_prefix("+++ ") {
            if p.trim() != "/dev/null" {
                return clean(p.strip_prefix("b/").unwrap_or(p));
            }
        }
        if let Some(p) = l.strip_prefix("--- ") {
            if p.trim() != "/dev/null" {
                old_side = Some(clean(p.strip_prefix("a/").unwrap_or(p)));
            }
        }
    }
    if let Some(p) = old_side {
        return p;
    }
    let first = section.lines().next().unwrap_or("");
    match first.strip_prefix("diff --git ") {
        Some(rest) => match rest.rfind(" b/") {
            Some(i) => clean(&rest[i + 3..]),
            None => clean(rest),
        },
        None => "(diff preamble)".to_string(),
    }
}

/// How early a file is shown when a diff must be fitted: code, configuration and tests first (0),
/// documentation next (1), generated lockfiles last (2). Pure.
pub fn diff_rank(path: &str) -> u8 {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    const LOCKFILES: [&str; 11] = [
        "cargo.lock",
        "package-lock.json",
        "npm-shrinkwrap.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "bun.lock",
        "poetry.lock",
        "uv.lock",
        "go.sum",
        "composer.lock",
        "gemfile.lock",
    ];
    if LOCKFILES.contains(&name) {
        return 2;
    }
    const DOCS: [&str; 5] = [".md", ".markdown", ".mdx", ".rst", ".adoc"];
    if DOCS.iter().any(|ext| name.ends_with(ext)) {
        return 1;
    }
    0
}

/// Fit a diff into `budget` characters for the reviewer. A diff within the budget is returned
/// unchanged with an empty note. A larger one is split per file and ordered by `diff_rank` (stable
/// within a rank). Every file keeps at least `FILE_FLOOR` characters, the rest of the budget goes
/// to the higher ranks first, and inside a rank small files are shown whole before large ones are
/// cut, each at a line boundary with a marker. The note lists every changed file with its added
/// and deleted lines and which were clipped, so a reviewer never mistakes a clipped file for a
/// missing change. Markers and the note are outside the budget. Pure.
pub fn fit_diff(diff: &str, budget: usize) -> (String, String) {
    if diff.chars().count() <= budget {
        return (diff.to_string(), String::new());
    }
    let mut files = split_diff(diff);
    files.sort_by_key(|f| diff_rank(&f.path));
    let sizes: Vec<usize> = files.iter().map(|f| f.text.chars().count()).collect();
    let floor = FILE_FLOOR.min(budget / files.len().max(1));
    let mut alloc: Vec<usize> = sizes.iter().map(|s| (*s).min(floor)).collect();
    let mut left = budget.saturating_sub(alloc.iter().sum());
    for rank in 0..=2u8 {
        // Max-min fair share inside the rank: the smallest needs are met in full first.
        let mut idx: Vec<usize> = (0..files.len())
            .filter(|i| diff_rank(&files[*i].path) == rank)
            .collect();
        idx.sort_by_key(|i| sizes[*i] - alloc[*i]);
        let mut remaining = idx.len();
        for i in idx {
            let share = left / remaining.max(1);
            let extra = (sizes[i] - alloc[i]).min(share);
            alloc[i] += extra;
            left -= extra;
            remaining -= 1;
        }
    }
    let mut shown = String::new();
    let mut listing = Vec::new();
    let mut clipped_any = false;
    for (i, f) in files.iter().enumerate() {
        if alloc[i] >= sizes[i] {
            shown.push_str(&f.text);
            listing.push(format!("- {}: +{} -{}", f.path, f.added, f.deleted));
            continue;
        }
        clipped_any = true;
        let total = f.text.lines().count();
        let cut = clip_at_line(&f.text, alloc[i]);
        let kept = cut.lines().count();
        shown.push_str(cut);
        if !cut.is_empty() && !cut.ends_with('\n') {
            shown.push('\n');
        }
        shown.push_str(&format!(
            "... clipped for this prompt: {kept} of {total} lines of {} shown, the rest is in the pull request\n",
            f.path
        ));
        listing.push(format!(
            "- {}: +{} -{} (clipped: {kept} of {total} diff lines shown)",
            f.path, f.added, f.deleted
        ));
    }
    if !clipped_any {
        return (shown, String::new());
    }
    let note = format!(
        "About the diff: it is larger than the {budget} characters a review prompt carries, so it \
was fitted per file. Code, configuration and tests come first, documentation after, and the \
largest files were clipped, never dropped. Every file the pull request changes, with its added \
and deleted lines:\n{}\n\
A clipped file is in the pull request in full; only this prompt leaves lines out. Do not reject \
only because a file is clipped or because the diff looks incomplete. Judge each criterion on what \
is shown and on the line counts above, and if a criterion can only be checked in clipped lines, \
name that file in your note.",
        listing.join("\n")
    );
    (shown, note)
}

/// The longest prefix of `text` within `max` characters that ends at a line break, or the first
/// line cut at `max` characters when even that line is longer. Pure.
fn clip_at_line(text: &str, max: usize) -> &str {
    let end = text
        .char_indices()
        .nth(max)
        .map(|(b, _)| b)
        .unwrap_or(text.len());
    let head = &text[..end];
    match head.rfind('\n') {
        Some(nl) => &text[..=nl],
        None => head,
    }
}

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
    // A diff over the budget is fitted per file, and the note then lists every changed file.
    let (diff, diff_note) = fit_diff(diff, MAX_DIFF);
    let diff_note = if diff_note.is_empty() {
        diff_note
    } else {
        format!("{diff_note}\n\n")
    };
    format!(
        "You are the reviewer for a mission. You did not write this code. Judge only what is below.\n\n\
Goal:\n{goal}\n\nAcceptance criteria:\n{acceptance}\n\n{evidence}The pull request diff:\n```diff\n{diff}\n```\n\n\
{diff_note}Reply with a single JSON object and nothing else:\n\
{{\"verdict\":\"approve\" or \"reject\",\"note\":\"one or two sentences a developer can act on\"}}\n\
Approve only if the diff satisfies every acceptance criterion. If tests are missing or the diff \
touches unrelated files, reject.",
        goal = m.goal,
        acceptance = m.acceptance,
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
        // The prompt goes on stdin: a diff of tens of kilobytes would pass the Windows command
        // line limit (32,767 characters) as an argument.
        ("claude", vec!["-p".into()])
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
    let out = if bin == "claude" {
        use std::io::Write;
        use std::process::Stdio;
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        // Written from a thread so a child that fills its stdout before reading all of stdin
        // cannot deadlock against this write.
        let writer = child.stdin.take().map(|mut stdin| {
            let text = prompt_text.clone();
            std::thread::spawn(move || {
                let _ = stdin.write_all(text.as_bytes());
            })
        });
        let out = child.wait_with_output()?;
        if let Some(w) = writer {
            let _ = w.join();
        }
        out
    } else {
        cmd.output()?
    };
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

    /// One file's section of a `gh pr diff`: a new file when `old` is empty and `context` too.
    fn file_diff(path: &str, old: &[&str], new: &[&str], context: &[String]) -> String {
        let created = old.is_empty() && context.is_empty();
        let mut s = format!("diff --git a/{path} b/{path}\n");
        if created {
            s.push_str("new file mode 100644\nindex 0000000..3f1c2ab\n--- /dev/null\n");
        } else {
            s.push_str(&format!("index 9a4c17b..64d41c5 100644\n--- a/{path}\n"));
        }
        s.push_str(&format!(
            "+++ b/{path}\n@@ -{},{} +1,{} @@\n",
            if created { 0 } else { 1 },
            old.len() + context.len(),
            new.len() + context.len()
        ));
        for c in context {
            s.push_str(&format!(" {c}\n"));
        }
        for l in old {
            s.push_str(&format!("-{l}\n"));
        }
        for l in new {
            s.push_str(&format!("+{l}\n"));
        }
        s
    }

    /// The shape of konethegreat/agentos-sandbox-1 PR #23 in `gh pr diff` order: a new 7.4 KB ADR,
    /// two small docs edits with context lines, then package.json and test.js.
    fn pr23_shaped() -> (String, String, String) {
        let adr_lines: Vec<String> = (0..90)
            .map(|i| format!("ADR line {i:02}: move the sandbox test suite to node:test, record decision text {i:02}."))
            .collect();
        let adr_refs: Vec<&str> = adr_lines.iter().map(String::as_str).collect();
        let adr = file_diff(
            "docs/adr/0001-move-the-sandbox-test-suite-to-node-test.md",
            &[],
            &adr_refs,
            &[],
        );
        let report_ctx: Vec<String> = (0..30)
            .map(|i| format!("2026-11-{i:02}: an earlier report entry that the diff shows as context, line {i:02}."))
            .collect();
        let report = file_diff(
            "docs/agentos/REPORT.md",
            &[],
            &["2026-11-12: test.js moved to node:test; npm test passes and fails on a - b."],
            &report_ctx,
        );
        let tasks_ctx: Vec<String> = (0..22)
            .map(|i| {
                format!("- [ ] Phase 2 task {i:02}: a later task listed as context in the diff.")
            })
            .collect();
        let tasks = file_diff(
            "docs/agentos/TASKS.md",
            &["- [ ] Phase 2 task 1: rewrite test.js with node:test"],
            &["- [x] Phase 2 task 1: rewrite test.js with node:test"],
            &tasks_ctx,
        );
        let package = file_diff(
            "package.json",
            &["    \"test\": \"node test.js\""],
            &["    \"test\": \"node --test test.js\""],
            &["  \"scripts\": {".to_string()],
        );
        let test_js = file_diff(
            "test.js",
            &[
                "const assert = require(\"assert\");",
                "assert.strictEqual(add(2, 3), 5);",
                "console.log(\"ok\");",
            ],
            &[
                "const { test } = require(\"node:test\");",
                "const assert = require(\"node:assert/strict\");",
                "test(\"add sums two numbers\", () => {",
                "  assert.strictEqual(add(2, 3), 5);",
                "});",
            ],
            &["const { add } = require(\"./lib\");".to_string()],
        );
        assert!(
            (7_000..8_000).contains(&adr.chars().count()),
            "{}",
            adr.len()
        );
        let diff = format!("{adr}{report}{tasks}{package}{test_js}");
        (diff, package, test_js)
    }

    #[test]
    fn split_diff_finds_every_file_with_its_line_counts() {
        let deleted = "diff --git a/old.txt b/old.txt\ndeleted file mode 100644\nindex 1111111..0000000\n--- a/old.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-one\n-two\n";
        // A Markdown list item removed from a file starts with `---` inside the hunk: it is a
        // deleted line, not a header.
        let md = "diff --git a/notes.md b/notes.md\nindex 2222222..3333333 100644\n--- a/notes.md\n+++ b/notes.md\n@@ -1,3 +1,3 @@\n # Notes\n--- a list item\n+- a new item\n+++ a bold line\n";
        let (diff, ..) = pr23_shaped();
        let all = format!("{diff}{deleted}{md}");
        let files = split_diff(&all);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "docs/adr/0001-move-the-sandbox-test-suite-to-node-test.md",
                "docs/agentos/REPORT.md",
                "docs/agentos/TASKS.md",
                "package.json",
                "test.js",
                "old.txt",
                "notes.md",
            ]
        );
        let counts: Vec<(usize, usize)> = files.iter().map(|f| (f.added, f.deleted)).collect();
        assert_eq!(
            counts,
            [(90, 0), (1, 0), (1, 1), (1, 1), (5, 3), (0, 2), (2, 1)]
        );
        // Splitting loses nothing.
        assert_eq!(
            files.iter().map(|f| f.text.as_str()).collect::<String>(),
            all
        );
    }

    #[test]
    fn code_ranks_before_docs_and_lockfiles_come_last() {
        assert_eq!(diff_rank("package.json"), 0);
        assert_eq!(diff_rank("test.js"), 0);
        assert_eq!(diff_rank("src-tauri/src/review_agent.rs"), 0);
        assert_eq!(diff_rank(".github/workflows/ci.yml"), 0);
        assert_eq!(diff_rank("requirements.txt"), 0);
        assert_eq!(diff_rank("docs/adr/0001-x.md"), 1);
        assert_eq!(diff_rank("README.MD"), 1);
        assert_eq!(diff_rank("site/page.mdx"), 1);
        assert_eq!(diff_rank("package-lock.json"), 2);
        assert_eq!(diff_rank("src-tauri/Cargo.lock"), 2);
    }

    #[test]
    fn a_diff_within_the_budget_is_unchanged_and_carries_no_note() {
        let (diff, ..) = pr23_shaped();
        let (shown, note) = fit_diff(&diff, MAX_DIFF);
        assert_eq!(shown, diff);
        assert!(note.is_empty());
        let p = prompt(&mission(), &diff);
        assert!(p.contains(&diff));
        assert!(!p.contains("About the diff"));
    }

    #[test]
    fn a_pr23_shaped_diff_keeps_package_json_and_test_js_whole() {
        let (diff, package, test_js) = pr23_shaped();
        let size = diff.chars().count();
        assert!(
            size > 12_000,
            "the fixture must exceed the old cap, it is {size}"
        );
        // The old tail cut lost both code files.
        let old: String = diff.chars().take(12_000).collect();
        assert!(!old.contains(&test_js));

        let (shown, note) = fit_diff(&diff, 12_000);
        assert!(
            shown.contains(&package),
            "package.json is not whole:\n{shown}"
        );
        assert!(shown.contains(&test_js), "test.js is not whole:\n{shown}");
        // Code is shown before the docs.
        let at = |needle: &str| shown.find(needle).unwrap();
        assert!(at("diff --git a/package.json") < at("diff --git a/docs/adr/"));
        assert!(at("diff --git a/test.js") < at("diff --git a/docs/agentos/REPORT.md"));
        // Every file is still there, the large ADR is the one clipped, and the bound holds apart
        // from the marker line.
        for f in split_diff(&diff) {
            assert!(shown.contains(&format!("diff --git a/{0} b/{0}", f.path)));
        }
        assert!(shown.contains("... clipped for this prompt: "));
        assert!(shown.chars().count() < 12_000 + 300);

        assert!(note.contains(
            "- docs/adr/0001-move-the-sandbox-test-suite-to-node-test.md: +90 -0 (clipped: "
        ));
        assert!(note.contains("- package.json: +1 -1\n"));
        assert!(note.contains("- test.js: +5 -3\n"));
        assert!(note.contains("- docs/agentos/TASKS.md: +1 -1"));
        assert!(note.contains("- docs/agentos/REPORT.md: +1 -0"));
        assert!(note.contains("Do not reject only because a file is clipped"));
    }

    #[test]
    fn the_prompt_carries_the_note_when_the_diff_is_fitted() {
        let (small, package, test_js) = pr23_shaped();
        // A 900-line design doc in front pushes the diff past MAX_DIFF.
        let big_doc: String = (0..900)
            .map(|i| format!("+A long design note line {i:03} that a docs-heavy pull request adds to the repo.\n"))
            .collect();
        let diff = format!(
            "diff --git a/docs/DESIGN.md b/docs/DESIGN.md\nnew file mode 100644\n--- /dev/null\n+++ b/docs/DESIGN.md\n@@ -0,0 +1,900 @@\n{big_doc}{small}"
        );
        assert!(diff.chars().count() > MAX_DIFF);
        let p = prompt(&mission(), &diff);
        assert!(p.contains(&package) && p.contains(&test_js));
        assert!(p.contains("About the diff"));
        assert!(p.contains("- docs/DESIGN.md: +900 -0 (clipped: "));
        // The note sits after the diff block and before the verdict instructions.
        let fence_end = p.rfind("```").unwrap();
        assert!(p.find("About the diff").unwrap() > fence_end);
        assert!(p.find("About the diff").unwrap() < p.find("Reply with a single JSON").unwrap());
    }

    #[test]
    fn a_tiny_budget_still_names_every_file_and_cuts_on_char_boundaries() {
        let files: String = (0..40)
            .map(|i| {
                let lines: Vec<String> = (0..50).map(|j| format!("é{i}{j} ünïcödé line")).collect();
                let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                file_diff(&format!("src/f{i:02}.rs"), &[], &refs, &[])
            })
            .collect();
        let (shown, note) = fit_diff(&files, 2_000);
        for i in 0..40 {
            assert!(note.contains(&format!("- src/f{i:02}.rs: +50 -0 (clipped: ")));
            assert!(shown.contains(&format!("of src/f{i:02}.rs shown")));
        }
        // Cuts count characters, not bytes, and end at a line break when one fits.
        assert_eq!(clip_at_line("éé\nüü\nöö", 4), "éé\n");
        assert_eq!(clip_at_line("éé\nüü\nöö", 6), "éé\nüü\n");
        assert_eq!(clip_at_line("ééééé", 3), "ééé");
        assert_eq!(clip_at_line("ab\n", 50), "ab\n");
    }
}
