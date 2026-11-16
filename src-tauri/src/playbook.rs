//! Playbooks (Beyond B3.1): what the fleet learned in a repository, delivered to every worker.
//!
//! The learning loop writes a convention or gotcha note for what each reviewed mission taught
//! (280 notes in the first three days). Before playbooks, a worker received only the newest
//! conventions that fit 1,500 characters, never a gotcha, and a Hermes kanban worker (most of the
//! fleet's missions) received nothing at all. A playbook ranks both kinds per repository, condenses
//! each to one line and fits a larger budget; every worker gets it in its task, and a generated
//! `01_Projects/<repo>/Playbook.md` shows the owner the same thing in Obsidian, rewritten nightly.

use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use crate::store;
use crate::vault::Vault;

/// The block a worker receives, in characters.
pub const WORKER_CAP: usize = 6000;
/// Share of the block that conventions get first; gotchas get the rest and any space left over.
pub const CONVENTION_SHARE_PCT: usize = 60;
/// One lesson, condensed, in characters.
pub const LINE_CHARS: usize = 220;
const LAST_DAY_KEY: &str = "playbook.last_day";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Lesson {
    /// `convention` or `gotcha`.
    pub kind: String,
    pub title: String,
    pub path: String,
    pub mtime: i64,
    pub in_degree: i64,
    pub text: String,
}

/// A note body as one line: no headings, no "See also" line. Pure.
pub fn condense(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| !l.to_lowercase().starts_with("see also:"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A lesson cut to `max` characters: whole sentences when they fit, else at a word. Pure.
pub fn summary(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    if let Some(i) = head.rfind(". ").filter(|i| *i >= max / 3) {
        return head[..=i].trim().to_string();
    }
    let cut = head.rfind(' ').unwrap_or(head.len());
    format!("{}...", head[..cut].trim_end_matches([',', ';', ':']))
}

fn ranked<'a>(lessons: &'a [Lesson], kind: &str) -> Vec<&'a Lesson> {
    let mut v: Vec<&Lesson> = lessons.iter().filter(|l| l.kind == kind).collect();
    // Notes other notes link to first, then the newest.
    v.sort_by_key(|l| std::cmp::Reverse((l.in_degree, l.mtime, l.path.clone())));
    v
}

fn line(l: &Lesson) -> String {
    format!("- {}: {}\n", l.title.trim(), summary(&l.text, LINE_CHARS))
}

/// The lessons that fit `cap`: conventions up to their share, then gotchas, then any conventions
/// that still fit. Pure.
pub fn select(lessons: &[Lesson], cap: usize) -> (Vec<&Lesson>, Vec<&Lesson>) {
    let conv = ranked(lessons, "convention");
    let gotchas = ranked(lessons, "gotcha");
    let mut used = 0;
    let mut take = |pool: &[&Lesson], budget: usize, out: &mut Vec<String>| -> Vec<usize> {
        let mut picked = Vec::new();
        for (i, l) in pool.iter().enumerate() {
            if out.contains(&l.path) {
                continue;
            }
            let n = line(l).chars().count();
            if used + n > budget {
                continue;
            }
            used += n;
            out.push(l.path.clone());
            picked.push(i);
        }
        picked
    };
    let mut seen = Vec::new();
    let mut c_idx = take(&conv, cap * CONVENTION_SHARE_PCT / 100, &mut seen);
    let g_idx = take(&gotchas, cap, &mut seen);
    c_idx.extend(take(&conv, cap, &mut seen));
    c_idx.sort_unstable();
    (
        c_idx.into_iter().map(|i| conv[i]).collect(),
        g_idx.into_iter().map(|i| gotchas[i]).collect(),
    )
}

/// The block every worker in this repository receives. Empty when nothing was learned. Pure.
pub fn worker_block(repo: &str, conv: &[&Lesson], gotchas: &[&Lesson], total: usize) -> String {
    if conv.is_empty() && gotchas.is_empty() {
        return String::new();
    }
    let mut b = format!(
        "What the fleet has learned in {repo} ({} of {total} lessons from reviewed missions). Follow the conventions; check the gotchas before you finish.\n",
        conv.len() + gotchas.len()
    );
    if !conv.is_empty() {
        b.push_str("Conventions:\n");
        for l in conv {
            b.push_str(&line(l));
        }
    }
    if !gotchas.is_empty() {
        b.push_str("Gotchas:\n");
        for l in gotchas {
            b.push_str(&line(l));
        }
    }
    b.trim_end().to_string()
}

/// The generated Obsidian note. `date` is shown, not compared. Pure.
pub fn note(repo: &str, conv: &[&Lesson], gotchas: &[&Lesson], total: usize, date: &str) -> String {
    let link = |l: &Lesson| {
        let target = l.path.trim_end_matches(".md");
        format!(
            "- [[{target}|{}]]: {}\n",
            l.title.trim(),
            summary(&l.text, LINE_CHARS)
        )
    };
    let mut n = format!(
        "---\n{}\ntype: playbook\nrepo: {repo}\n---\n# {repo} playbook\n\nGenerated by AgentOS Studio on {date} from {total} lesson notes. The {} below reach every worker that starts a mission in {repo}.\n",
        crate::goal_views::GENERATED,
        conv.len() + gotchas.len()
    );
    if !conv.is_empty() {
        n.push_str("\n## Conventions\n\n");
        for l in conv {
            n.push_str(&link(l));
        }
    }
    if !gotchas.is_empty() {
        n.push_str("\n## Gotchas\n\n");
        for l in gotchas {
            n.push_str(&link(l));
        }
    }
    n
}

/// Every active convention and gotcha note for a repository.
pub fn lessons(v: &Vault, repo: &str) -> Vec<Lesson> {
    crate::memory_index::notes(v)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| (r.kind == "convention" || r.kind == "gotcha") && r.status == "active")
        .filter(|r| {
            r.repo
                .as_deref()
                .is_some_and(|x| x.eq_ignore_ascii_case(repo.trim()))
        })
        .filter_map(|r| {
            let raw = std::fs::read_to_string(v.root.join(&r.path)).ok()?;
            let text = condense(&crate::vault::Doc::parse(&raw).body);
            (!text.is_empty()).then_some(Lesson {
                kind: r.kind,
                title: r.title,
                path: r.path,
                mtime: r.mtime,
                in_degree: r.in_degree,
                text,
            })
        })
        .collect()
}

/// The block for a worker starting in this repository.
pub fn for_worker(v: &Vault, repo: &str) -> String {
    let all = lessons(v, repo);
    let (c, g) = select(&all, WORKER_CAP);
    worker_block(repo, &c, &g, all.len())
}

/// The same block, from the configured vault (for callers with no `AppState`).
pub fn for_worker_here(repo: &str) -> String {
    let root = crate::settings::vault_path();
    if !root.is_dir() {
        return String::new();
    }
    for_worker(&Vault::new(root), repo)
}

pub fn note_path(v: &Vault, repo: &str) -> PathBuf {
    let safe: String = repo
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    v.root.join("01_Projects").join(safe).join("Playbook.md")
}

/// Write one repository's playbook note. Returns whether it changed.
pub fn write(v: &Vault, repo: &str) -> Result<bool> {
    let all = lessons(v, repo);
    if all.is_empty() {
        return Ok(false);
    }
    let (c, g) = select(&all, WORKER_CAP);
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let text = note(repo, &c, &g, all.len(), &date);
    let path = note_path(v, repo);
    let without_date = |s: &str| s.replace(&date, "");
    if let Ok(old) = std::fs::read_to_string(&path) {
        let old_dated = old
            .lines()
            .find(|l| l.starts_with("Generated by AgentOS Studio on "))
            .and_then(|l| l.split_whitespace().nth(5))
            .unwrap_or("")
            .to_string();
        if without_date(&old.replace(&old_dated, &date)) == without_date(&text) {
            return Ok(false);
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, text)?;
    Ok(true)
}

/// Every repository with lessons. Returns the repositories whose playbook changed.
pub fn write_all(v: &Vault) -> Result<Vec<String>> {
    let mut repos: Vec<String> = crate::memory_index::notes(v)?
        .into_iter()
        .filter(|r| (r.kind == "convention" || r.kind == "gotcha") && r.status == "active")
        .filter_map(|r| r.repo)
        .collect();
    repos.sort_by_key(|r| r.to_lowercase());
    repos.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    let mut changed = Vec::new();
    for r in repos {
        if write(v, &r)? {
            changed.push(r);
        }
    }
    Ok(changed)
}

/// Is tonight's distillation due? Once a day, from 02:00 local. Pure.
pub fn due(last_day: Option<&str>, today: &str, hour: u32) -> bool {
    hour >= 2 && last_day != Some(today)
}

/// The nightly pass, called from the scheduler: rewrite the playbooks once a day.
pub fn nightly(v: &Vault) -> Option<Vec<String>> {
    let now = chrono::Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let c = store::open().ok()?;
    let last = store::kv_get(&c, LAST_DAY_KEY).ok().flatten();
    if !due(last.as_deref(), &today, chrono::Timelike::hour(&now)) {
        return None;
    }
    let _ = store::kv_set(&c, LAST_DAY_KEY, &today);
    drop(c);
    write_all(v).ok()
}

#[derive(Debug, Clone, Serialize)]
pub struct Learned {
    pub repo: String,
    pub count: usize,
    pub titles: Vec<String>,
}

/// What the fleet learned in the last `hours`, per repository, newest first (Today's card).
pub fn learned_since(v: &Vault, hours: i64) -> Vec<Learned> {
    let since = store::now() - hours * 3600;
    let mut by: std::collections::BTreeMap<String, Vec<(i64, String)>> = Default::default();
    for r in crate::memory_index::notes(v).unwrap_or_default() {
        if (r.kind == "convention" || r.kind == "gotcha") && r.mtime >= since {
            if let Some(repo) = r.repo {
                by.entry(repo).or_default().push((r.mtime, r.title));
            }
        }
    }
    let mut out: Vec<Learned> = by
        .into_iter()
        .map(|(repo, mut t)| {
            t.sort_by_key(|x| std::cmp::Reverse(x.0));
            Learned {
                repo,
                count: t.len(),
                titles: t.into_iter().take(4).map(|x| x.1).collect(),
            }
        })
        .collect();
    out.sort_by_key(|l| std::cmp::Reverse(l.count));
    out
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PeriodStats {
    pub missions: usize,
    pub approved_first: usize,
    pub first_attempt_rate: f64,
    pub total_retries: u32,
    pub avg_retries: f64,
}

/// Proof of learning per repository: before vs after its playbook (Beyond B3.4).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RepoProof {
    pub repo: String,
    pub playbook_at: Option<i64>,
    pub lessons: usize,
    pub before: PeriodStats,
    pub after: PeriodStats,
}

fn summarize_period(rows: &[(&crate::missions::Mission, u32)]) -> PeriodStats {
    let n = rows.len();
    if n == 0 {
        return PeriodStats {
            missions: 0,
            approved_first: 0,
            first_attempt_rate: 0.0,
            total_retries: 0,
            avg_retries: 0.0,
        };
    }
    let approved_first = rows
        .iter()
        .filter(|(m, retries)| m.review_verdict.as_deref() == Some("approve") && *retries == 0)
        .count();
    let total_retries: u32 = rows.iter().map(|(_, r)| *r).sum();
    PeriodStats {
        missions: n,
        approved_first,
        first_attempt_rate: approved_first as f64 / n as f64,
        total_retries,
        avg_retries: total_retries as f64 / n as f64,
    }
}

/// Pure proof-of-learning aggregation per repository.
pub fn compute_proof(
    missions: &[crate::missions::Mission],
    retries_map: &std::collections::HashMap<String, u32>,
    playbook_at: &std::collections::HashMap<String, i64>,
    lesson_counts: &std::collections::HashMap<String, usize>,
) -> Vec<RepoProof> {
    let mut by_repo: std::collections::BTreeMap<String, Vec<&crate::missions::Mission>> =
        Default::default();
    for m in missions {
        if m.review_verdict.is_some() || m.status == "done" || m.status == "failed" {
            by_repo.entry(m.repo.clone()).or_default().push(m);
        }
    }
    let mut out = Vec::new();
    for (repo, ms) in by_repo {
        let key = repo.to_lowercase();
        let cut = playbook_at.get(&key).copied();
        let mut before = Vec::new();
        let mut after = Vec::new();
        for m in ms {
            let r = retries_map.get(&m.id).copied().unwrap_or(0);
            if let Some(t) = cut {
                if m.created_at >= t {
                    after.push((m, r));
                } else {
                    before.push((m, r));
                }
            } else {
                before.push((m, r));
            }
        }
        out.push(RepoProof {
            repo: repo.clone(),
            playbook_at: cut,
            lessons: lesson_counts.get(&key).copied().unwrap_or(0),
            before: summarize_period(&before),
            after: summarize_period(&after),
        });
    }
    out.sort_by(|a, b| {
        (b.before.missions + b.after.missions)
            .cmp(&(a.before.missions + a.after.missions))
            .then_with(|| a.repo.cmp(&b.repo))
    });
    out
}

/// Live proof-of-learning across all repositories from `studio.db` and the vault.
pub fn proof_of_learning(v: &Vault) -> Vec<RepoProof> {
    let missions = crate::missions::list(500).unwrap_or_default();
    let mut retries_map = std::collections::HashMap::new();
    for m in &missions {
        let total = crate::stuck::retries(&m.id) + crate::goal_loop::rejects(&m.id);
        retries_map.insert(m.id.clone(), total);
    }
    let mut playbook_at: std::collections::HashMap<String, i64> = Default::default();
    let mut lesson_counts: std::collections::HashMap<String, usize> = Default::default();
    for r in crate::memory_index::notes(v).unwrap_or_default() {
        if (r.kind == "convention" || r.kind == "gotcha") && r.status == "active" {
            if let Some(repo) = r.repo {
                let k = repo.to_lowercase();
                *lesson_counts.entry(k.clone()).or_default() += 1;
                let pb = note_path(v, &repo);
                if pb.exists() {
                    let entry = playbook_at.entry(k).or_insert(r.mtime);
                    if r.mtime < *entry {
                        *entry = r.mtime;
                    }
                }
            }
        }
    }
    compute_proof(&missions, &retries_map, &playbook_at, &lesson_counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_splits_before_and_after_playbook_and_counts_first_attempt_rate() {
        let mut m1 = crate::missions::Mission {
            id: "M-1".into(),
            repo: "r1".into(),
            status: "done".into(),
            review_verdict: Some("approve".into()),
            created_at: 100,
            ..Default::default()
        };
        let m2 = crate::missions::Mission {
            id: "M-2".into(),
            repo: "r1".into(),
            status: "done".into(),
            review_verdict: Some("approve".into()),
            created_at: 200,
            ..Default::default()
        };
        m1.review_verdict = Some("approve".into());
        let mut retries = std::collections::HashMap::new();
        retries.insert("M-1".into(), 1);
        retries.insert("M-2".into(), 0);
        let mut pb = std::collections::HashMap::new();
        pb.insert("r1".into(), 150);
        let mut lc = std::collections::HashMap::new();
        lc.insert("r1".into(), 4);
        let out = compute_proof(&[m1, m2], &retries, &pb, &lc);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].before.missions, 1);
        assert_eq!(out[0].before.approved_first, 0);
        assert_eq!(out[0].after.missions, 1);
        assert_eq!(out[0].after.approved_first, 1);
        assert!((out[0].after.first_attempt_rate - 1.0).abs() < 1e-6);
    }

    fn lesson(kind: &str, title: &str, mtime: i64, links: i64, text: &str) -> Lesson {
        Lesson {
            kind: kind.into(),
            title: title.into(),
            path: format!("03_Memory/{}.md", title.replace(' ', "_")),
            mtime,
            in_degree: links,
            text: text.into(),
        }
    }

    #[test]
    fn a_note_body_becomes_one_line_and_a_long_one_is_cut_cleanly() {
        let body = "# Adding a check-text rule\n\nSee also: [[SYSTEM_CONTEXT]]\n\n## 2026-11-12 (convention)\n\nAppend a rule to HIDDEN_RULES.\nNever put the character literally.";
        assert_eq!(
            condense(body),
            "Append a rule to HIDDEN_RULES. Never put the character literally."
        );
        let long =
            "First sentence is here. Second sentence is long enough to pass the limit for sure.";
        assert_eq!(summary(long, 40), "First sentence is here.");
        assert_eq!(
            summary("one two three four five six", 14),
            "one two three..."
        );
        assert_eq!(summary("short", 40), "short");
    }

    #[test]
    fn gotchas_reach_the_worker_and_the_block_fits_its_budget() {
        let mut all = Vec::new();
        for i in 0..40 {
            all.push(lesson(
                "convention",
                &format!("conv {i}"),
                1000 + i,
                0,
                &"c ".repeat(80),
            ));
            all.push(lesson(
                "gotcha",
                &format!("gotcha {i}"),
                1000 + i,
                0,
                &"g ".repeat(80),
            ));
        }
        all.push(lesson(
            "convention",
            "linked old",
            1,
            5,
            "Linked by five notes.",
        ));
        let (c, g) = select(&all, 3000);
        assert!(!g.is_empty(), "gotchas are delivered");
        assert!(!c.is_empty());
        assert_eq!(c[0].title, "linked old", "linked notes rank first");
        assert_eq!(c[1].title, "conv 39", "then the newest");
        let block = worker_block("repo-a", &c, &g, all.len());
        assert!(
            block.chars().count() <= 3000 + 200,
            "{}",
            block.chars().count()
        );
        assert!(block.contains("Conventions:") && block.contains("Gotchas:"));
        assert!(block.contains("of 81 lessons"));
        assert_eq!(worker_block("repo-a", &[], &[], 0), "");
    }

    #[test]
    fn spare_room_goes_back_to_conventions() {
        let all: Vec<Lesson> = (0..10)
            .map(|i| lesson("convention", &format!("c{i}"), i, 0, "Short rule."))
            .collect();
        let (c, g) = select(&all, 6000);
        assert_eq!(
            c.len(),
            10,
            "no gotchas, so conventions take the whole block"
        );
        assert!(g.is_empty());
    }

    #[test]
    fn the_note_is_generated_and_links_every_lesson() {
        let a = lesson("convention", "Use npm ci", 5, 0, "Use npm ci in CI.");
        let b = lesson("gotcha", "Flaky port", 6, 0, "Port 1420 can be taken.");
        let n = note("repo-a", &[&a], &[&b], 2, "2026-11-16");
        assert!(
            n.starts_with("---\ngenerated_by: agentos-studio\ntype: playbook\nrepo: repo-a\n---")
        );
        assert!(n.contains("[[03_Memory/Use_npm_ci|Use npm ci]]: Use npm ci in CI."));
        assert!(n.contains("## Gotchas"));
    }

    /// Read only: prints the block a worker in a real repository would receive.
    /// `PLAYBOOK_REPO=konethegreat cargo test --lib playbook::tests::real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_vault_block_for_a_repository() {
        let repo = std::env::var("PLAYBOOK_REPO").unwrap_or_else(|_| "konethegreat".into());
        let block = for_worker_here(&repo);
        println!("{} chars\n{block}", block.chars().count());
        assert!(!block.is_empty());
    }

    #[test]
    fn the_distillation_runs_once_a_night() {
        assert!(due(None, "2026-11-16", 3));
        assert!(!due(None, "2026-11-16", 1), "not before 02:00");
        assert!(!due(Some("2026-11-16"), "2026-11-16", 5));
        assert!(due(Some("2026-11-15"), "2026-11-16", 2));
    }

    #[test]
    fn a_repository_s_lessons_become_its_playbook_note() {
        let d = tempfile::tempdir().unwrap();
        let _g = crate::store::test_env_lock();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        crate::memory_index::set_test_index_dir(Some(d.path().join("index")));
        let v = Vault::new(d.path().join("vault"));
        v.scaffold().unwrap();
        let mem = v.root.join("03_Memory");
        std::fs::write(
            mem.join("Use_npm_ci.md"),
            "---\ntype: convention\nrepo: repo-a\nagent: agentos-learning\n---\n# Use npm ci\n\nUse npm ci, never npm install, in CI.\n",
        )
        .unwrap();
        std::fs::write(
            mem.join("Port_taken.md"),
            "---\ntype: gotcha\nrepo: repo-a\nagent: agentos-learning\n---\n# Port taken\n\nThe dev server port 1420 can already be taken.\n",
        )
        .unwrap();
        std::fs::write(
            mem.join("Other.md"),
            "---\ntype: convention\nrepo: repo-b\n---\n# Other\n\nNot for repo-a.\n",
        )
        .unwrap();
        let block = for_worker(&v, "repo-a");
        assert!(block.contains("Use npm ci, never npm install"), "{block}");
        assert!(block.contains("Port taken"), "{block}");
        assert!(!block.contains("Not for repo-a"));
        assert!(write(&v, "repo-a").unwrap());
        assert!(
            !write(&v, "repo-a").unwrap(),
            "unchanged lessons, unchanged note"
        );
        let text = std::fs::read_to_string(note_path(&v, "repo-a")).unwrap();
        assert!(text.contains("generated_by: agentos-studio"));
        let changed = write_all(&v).unwrap();
        assert_eq!(changed, vec!["repo-b".to_string()]);
        crate::memory_index::set_test_index_dir(None);
        crate::store::set_test_db(None);
    }
}
