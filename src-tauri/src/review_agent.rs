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
    format!(
        "You are the reviewer for a mission. You did not write this code. Judge only what is below.\n\n\
Goal:\n{goal}\n\nAcceptance criteria:\n{acceptance}\n\nThe pull request diff:\n```diff\n{diff}\n```\n\n\
Reply with a single JSON object and nothing else:\n\
{{\"verdict\":\"approve\" or \"reject\",\"note\":\"one or two sentences a developer can act on\"}}\n\
Approve only if the diff satisfies every acceptance criterion. If tests are missing or the diff \
touches unrelated files, reject.",
        goal = m.goal,
        acceptance = m.acceptance,
        diff = diff
    )
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

/// The folder a mission's repository lives in, from Hermes projects.
pub fn repo_dir(repo: &str) -> PathBuf {
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
                return PathBuf::from(path);
            }
        }
    }
    crate::settings::home()
}

/// Run the reviewer and record its verdict. Returns the updated mission.
pub fn review(v: &Vault, id: &str, reviewer: &str) -> Result<Mission> {
    let _ = v;
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
    // A mission stores the project slug. `gh` can resolve the repo from the working folder, so use
    // that when the value is not `owner/name`.
    let diff = if m.repo.contains('/') {
        crate::github::pr_diff(&m.repo, number).unwrap_or_default()
    } else {
        let mut cmd = Command::new("gh");
        cmd.args(["pr", "diff", &number.to_string()])
            .current_dir(&repo_dir(&m.repo))
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
    let diff: String = diff.chars().take(MAX_DIFF).collect();

    let dir = repo_dir(&m.repo);
    let prompt_text = prompt(&m, &diff);
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
        (
            "hermes",
            vec![
                "chat".into(),
                "-Q".into(),
                "--oneshot".into(),
                "--query-file".into(),
                qpath.display().to_string(),
                "--in".into(),
                dir.display().to_string(),
            ],
        )
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
    let (verdict, note) = parse_verdict(&text).ok_or_else(|| {
        anyhow::anyhow!(
            "the reviewer did not return a verdict. It said: {}",
            text.chars().take(300).collect::<String>()
        )
    })?;
    missions::review(id, reviewer, &verdict, &note)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn pr_number_comes_from_the_url() {
        assert_eq!(missions::pr_number(&mission()), Some(42));
    }
}
