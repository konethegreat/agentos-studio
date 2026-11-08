//! Skills from success: a procedure that worked three times becomes a skill draft.
//!
//! Approved missions are grouped by role and by a normalised verb plus object read from the goal.
//! A group of three or more produces one proposal: a `SKILL.md` draft with the frontmatter Hermes
//! requires, steps taken from the memory ledger's tool sequence, pitfalls, and the missions as
//! evidence. Nothing is installed until the owner approves, and approval runs one Hermes turn that
//! creates the skill; the turn is injected so the proposal side is unit tested offline.
//!
//! Dismissed proposals are recorded as `skills.dismissed.<hash>` in `studio.db`, so the same group
//! is not proposed again.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::missions::{self, Mission};
use crate::procs;
use crate::store;
use crate::vault::{now, Vault};

/// Three approved missions with the same role and task class make a proposal.
pub const THRESHOLD: usize = 3;
/// Steps listed in the draft.
const MAX_STEPS: usize = 6;
/// The ledger rows read per session when building the tool sequence.
const LEDGER_ROWS: usize = 50;
/// The missions shown as evidence in the draft.
const MAX_EVIDENCE: usize = 3;
const MAX_NAME_CHARS: usize = 48;

const VERBS: &[&str] = &[
    "add",
    "build",
    "create",
    "delete",
    "document",
    "expose",
    "fix",
    "hook",
    "implement",
    "improve",
    "make",
    "migrate",
    "move",
    "refactor",
    "remove",
    "rename",
    "resolve",
    "review",
    "split",
    "test",
    "update",
    "upgrade",
    "wire",
    "write",
];

const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "for", "in", "my", "of", "on", "our", "that", "the", "this", "to", "with",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Evidence {
    pub mission: String,
    pub goal: String,
    pub agent: String,
    pub pr_url: Option<String>,
    /// Tools the mission used, in ledger order, from the memory reads and writes it made.
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Proposal {
    /// Stable id for this role and task class, the suffix of `skills.dismissed.<hash>`.
    pub hash: String,
    pub name: String,
    pub role: String,
    pub task_class: String,
    pub description: String,
    /// The full `SKILL.md` content.
    pub draft: String,
    pub evidence: Vec<Evidence>,
}

/// The normalised verb plus object a title names. Pure.
///
/// The first known verb wins ("Add the learning loop" and "add a Learning Loop" both become
/// `add learning loop`); a title with no known verb falls back to its first word. Articles and
/// joining words are dropped, and the object keeps at most four words.
pub fn task_class(title: &str) -> Option<String> {
    let words: Vec<String> = title
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    if words.is_empty() {
        return None;
    }
    let verb = words
        .iter()
        .position(|w| VERBS.contains(&w.as_str()))
        .unwrap_or(0);
    let object: Vec<&str> = words[verb + 1..]
        .iter()
        .map(String::as_str)
        .filter(|w| !STOP_WORDS.contains(w))
        .take(4)
        .collect();
    if object.is_empty() {
        return None;
    }
    Some(format!("{} {}", words[verb], object.join(" ")))
}

fn approved(m: &Mission) -> bool {
    m.status == "done" && m.review_verdict.as_deref() == Some("approve")
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

pub fn hash_of(role: &str, class: &str) -> String {
    format!("{:016x}", fnv(format!("{role}\n{class}").as_bytes()))
}

fn skill_name(class: &str) -> String {
    let mut name = class
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if name.chars().count() > MAX_NAME_CHARS {
        name = name.chars().take(MAX_NAME_CHARS).collect();
        name = name.trim_end_matches('-').to_string();
    }
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        name = format!("task-{name}");
    }
    name
}

/// `Use when ...` under 60 characters, as the skill frontmatter requires. Pure.
pub fn description(class: &str) -> String {
    const PREFIX: &str = "Use when the task is: ";
    let mut object = class.trim().to_string();
    while PREFIX.len() + object.chars().count() > 59 {
        match object.rfind(' ') {
            Some(i) => object.truncate(i),
            None => {
                object.clear();
            }
        }
    }
    if object.is_empty() {
        "Use when this class of task comes up".to_string()
    } else {
        format!("{PREFIX}{object}")
    }
}

fn draft_steps(evidence: &[Evidence]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut tools = Vec::new();
    for e in evidence {
        for t in &e.tools {
            if seen.insert(t.clone()) {
                tools.push(t.clone());
            }
        }
    }
    if tools.is_empty() {
        return vec![
            "Read the repository and the mission's acceptance criteria.".into(),
            "Make the smallest change that satisfies the criteria.".into(),
            "Run the repository's checks locally.".into(),
            "Open the pull request with the verification results.".into(),
        ];
    }
    tools
        .into_iter()
        .take(MAX_STEPS)
        .map(|t| format!("Run `{t}`."))
        .collect()
}

/// The `SKILL.md` a proposal carries. Pure, so the frontmatter is tested without a store.
pub fn render_draft(p: &Proposal) -> String {
    let mut out = format!(
        "---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\n\
This procedure worked in {n} approved missions with the {role} role. Use it when the task \
class is \"{class}\".\n\n## Steps\n\n",
        name = p.name,
        description = p.description,
        n = p.evidence.len(),
        role = p.role,
        class = p.task_class
    );
    for (i, step) in draft_steps(&p.evidence).iter().enumerate() {
        out.push_str(&format!("{}. {step}\n", i + 1));
    }
    out.push_str(
        "\n## Pitfalls\n\n\
- Do not open the pull request before the repository checks pass.\n\
- Keep the change to the mission's goal; leave unrelated refactors out.\n\
- If a reviewer rejects the work, fix the named reasons before submitting again.\n\
\n## Evidence\n\n",
    );
    for e in p.evidence.iter().take(MAX_EVIDENCE) {
        out.push_str(&format!(
            "- {}: {} ({}{})\n",
            e.mission,
            e.goal,
            e.agent,
            match &e.pr_url {
                Some(url) => format!(", {url}"),
                None => String::new(),
            }
        ));
    }
    out
}

/// Group approved missions and keep the groups at or above the threshold. Pure; the tool
/// sequences and the dismissed set are inputs, so tests build them without a store.
pub fn build(
    missions: &[Mission],
    tools: &BTreeMap<String, Vec<String>>,
    dismissed: &HashSet<String>,
) -> Vec<Proposal> {
    let mut groups: BTreeMap<(String, String), Vec<Evidence>> = BTreeMap::new();
    for m in missions.iter().filter(|m| approved(m)) {
        let Some(class) = task_class(&m.goal) else {
            continue;
        };
        groups
            .entry((m.role.clone(), class))
            .or_default()
            .push(Evidence {
                mission: m.id.clone(),
                goal: m.goal.clone(),
                agent: m.agent.clone(),
                pr_url: m.pr_url.clone(),
                tools: tools.get(&m.id).cloned().unwrap_or_default(),
            });
    }

    let mut out: Vec<Proposal> = Vec::new();
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    for ((role, class), mut evidence) in groups {
        if evidence.len() < THRESHOLD {
            continue;
        }
        let hash = hash_of(&role, &class);
        if dismissed.contains(&hash) {
            continue;
        }
        evidence.sort_by(|a, b| a.mission.cmp(&b.mission));
        let base = skill_name(&class);
        let count = names.entry(base.clone()).or_insert(0);
        *count += 1;
        let name = if *count == 1 {
            base
        } else {
            format!("{base}-{role}")
        };
        let description = description(&class);
        let mut p = Proposal {
            hash,
            name,
            role,
            task_class: class,
            description,
            draft: String::new(),
            evidence,
        };
        p.draft = render_draft(&p);
        out.push(p);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn dismissed_in(c: &Connection) -> Result<HashSet<String>> {
    let mut st = c.prepare("SELECT key FROM kv WHERE key LIKE 'skills.dismissed.%'")?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows
        .flatten()
        .filter_map(|k| k.strip_prefix("skills.dismissed.").map(str::to_string))
        .collect())
}

/// The Hermes sessions a mission used, from the exchange log and its worker log.
fn mission_sessions(v: &Vault, m: &Mission) -> Vec<String> {
    let mut out = BTreeSet::new();
    for e in crate::agent_log::for_mission(&m.id).unwrap_or_default() {
        if let Some((_, sid)) = e.detail.rsplit_once("Hermes session: ") {
            let sid = sid.trim();
            if !sid.is_empty() {
                out.insert(sid.to_string());
            }
        }
    }
    if let Some(task) = m.task_id.as_deref() {
        if let Ok(log) = fs::read_to_string(v.worker_logs().join(format!("{task}.log"))) {
            for line in log.lines() {
                if let Some(sid) = line
                    .strip_prefix("[stderr] ")
                    .and_then(|l| l.trim().strip_prefix("session_id:"))
                {
                    let sid = sid.trim();
                    if !sid.is_empty() {
                        out.insert(sid.to_string());
                    }
                }
            }
        }
    }
    out.into_iter().collect()
}

/// The tools a mission used, in ledger order, without adjacent repeats.
fn tool_sequence(v: &Vault, m: &Mission) -> Vec<String> {
    let mut out = Vec::new();
    for sid in mission_sessions(v, m) {
        for e in crate::memory_ledger::list(&sid, LEDGER_ROWS).unwrap_or_default() {
            if out.last() != Some(&e.tool) {
                out.push(e.tool);
            }
        }
    }
    out
}

pub fn proposals(v: &Vault) -> Result<Vec<Proposal>> {
    let c = store::open()?;
    let missions = missions::list(500)?;
    let dismissed = dismissed_in(&c)?;
    let mut tools = BTreeMap::new();
    for m in missions.iter().filter(|m| approved(m)) {
        tools.insert(m.id.clone(), tool_sequence(v, m));
    }
    Ok(build(&missions, &tools, &dismissed))
}

/// Dismiss a proposal. The hash is the stable id from `build`, so the same group never returns.
pub fn dismiss(hash: &str) -> Result<()> {
    let hash = hash.trim().to_lowercase();
    if hash.len() != 16 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("a proposal id is 16 hexadecimal characters");
    }
    let c = store::open()?;
    store::kv_set(&c, &format!("skills.dismissed.{hash}"), &now())?;
    Ok(())
}

fn valid_profile(profile: &str) -> bool {
    !profile.is_empty()
        && profile.len() <= 64
        && profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The skills folder a profile reads: the Hermes home itself for the default profile, otherwise
/// the profile folder under `profiles/`.
pub fn skill_root(profile: Option<&str>) -> Result<PathBuf> {
    let home = procs::hermes_home();
    match profile
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "default")
    {
        None => Ok(home.join("skills")),
        Some(p) if valid_profile(p) => Ok(home.join("profiles").join(p).join("skills")),
        Some(p) => bail!("invalid profile name '{p}'"),
    }
}

/// The prompt for the one turn that creates the skill. Pure.
pub fn skill_prompt(name: &str, draft: &Path) -> String {
    format!(
        "Create one new Hermes skill and nothing else.\n\n\
Skill name: {name}\n\
Draft SKILL.md: {draft}\n\n\
Read the draft and create the skill with your skill creation tool so that Hermes lists it under \
its skills directory with exactly this name. Do not install anything from a registry or a URL, do \
not modify or remove any other skill, and do not change memory. Reply with the path of the \
SKILL.md you created.",
        draft = draft.display()
    )
}

type Turn<'a> = dyn Fn(Option<&str>, &Path, &Path) -> Result<String> + 'a;
type Confirm<'a> = dyn Fn(Option<&str>, &str) -> Option<String> + 'a;

/// Create the skill through one Hermes turn, then confirm the file exists where the profile reads
/// its skills. The approval permission is checked by the caller.
pub fn approve(proposal: &Proposal, profile: Option<&str>) -> Result<String> {
    approve_with(proposal, profile, &run_skill_turn, &confirm_skill)
}

fn approve_with(
    proposal: &Proposal,
    profile: Option<&str>,
    turn: &Turn<'_>,
    confirm: &Confirm<'_>,
) -> Result<String> {
    let profile = profile.map(str::trim).filter(|p| !p.is_empty());
    if let Some(p) = profile.filter(|p| *p != "default") {
        if !valid_profile(p) {
            bail!("invalid profile name '{p}'");
        }
    }
    let dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("agentos-studio")
        .join("skill-proposals");
    fs::create_dir_all(&dir)?;
    let draft = dir.join(format!("{}.md", proposal.name));
    fs::write(&draft, &proposal.draft)?;
    let prompt = dir.join(format!("{}.prompt.md", proposal.name));
    fs::write(&prompt, skill_prompt(&proposal.name, &draft))?;

    turn(profile, &dir, &prompt)?;
    confirm(profile, &proposal.name).ok_or_else(|| {
        anyhow!(
            "the turn did not create '{}' under the {} profile's skills",
            proposal.name,
            profile.unwrap_or("default")
        )
    })
}

fn confirm_skill(profile: Option<&str>, name: &str) -> Option<String> {
    let root = skill_root(profile).ok()?;
    crate::skills::scan(&root)
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .map(|s| s.path)
}

/// One short headless turn on the named profile, otherwise the default. Quiet mode keeps it out of
/// the owner's chat list.
fn run_skill_turn(profile: Option<&str>, dir: &Path, qpath: &Path) -> Result<String> {
    let mut args: Vec<String> = profile
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "default")
        .map(|p| vec!["-p".into(), p.into()])
        .unwrap_or_default();
    args.extend(["chat", "-Q", "--oneshot", "--query-file"].map(String::from));
    args.push(qpath.display().to_string());
    args.extend(["--in".into(), dir.display().to_string()]);
    let exe = procs::find("hermes")
        .ok_or_else(|| anyhow!("hermes is not installed, so a skill cannot be created"))?;
    let out = procs::command(&exe)
        .args(&args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .output()?;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() && text.trim().is_empty() {
        bail!("the skill turn failed: {}", stderr.trim());
    }
    if text.trim().is_empty() {
        bail!("the skill turn returned nothing: {}", stderr.trim());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission(
        id: &str,
        goal: &str,
        role: &str,
        agent: &str,
        status: &str,
        verdict: Option<&str>,
    ) -> Mission {
        Mission {
            id: id.into(),
            repo: "agentos-studio".into(),
            goal: goal.into(),
            acceptance: "The change is on main".into(),
            role: role.into(),
            agent: agent.into(),
            status: status.into(),
            provider: "claude-subscription".into(),
            model: String::new(),
            budget_usd: 0.0,
            spent_usd: 0.0,
            task_id: None,
            reviewer: Some("qoder".into()),
            review_verdict: verdict.map(str::to_string),
            pr_url: Some(format!(
                "https://github.com/konethegreat/agentos-studio/pull/{id}"
            )),
            review_note: None,
            created_at: 0,
            updated_at: 0,
            verify: None,
        }
    }

    fn approved_trio(role: &str) -> Vec<Mission> {
        vec![
            mission(
                "M-001",
                "Add the learning loop",
                role,
                "hermes",
                "done",
                Some("approve"),
            ),
            mission(
                "M-002",
                "add a Learning Loop",
                role,
                "qoder",
                "done",
                Some("approve"),
            ),
            mission(
                "M-003",
                "Add learning loop",
                role,
                "hermes",
                "done",
                Some("approve"),
            ),
        ]
    }

    #[test]
    fn the_task_class_is_a_normalised_verb_and_object() {
        assert_eq!(
            task_class("Add the learning loop").as_deref(),
            Some("add learning loop")
        );
        assert_eq!(
            task_class("add a Learning Loop to Brain").as_deref(),
            Some("add learning loop brain")
        );
        assert_eq!(
            task_class("Fix the merge gate, and the slug").as_deref(),
            Some("fix merge gate slug")
        );
        assert_eq!(
            task_class("Update README").as_deref(),
            Some("update readme")
        );
        assert_eq!(task_class("ship it").as_deref(), Some("ship it"));
        assert_eq!(task_class("   "), None);
        assert_eq!(task_class("Add"), None);
    }

    #[test]
    fn three_approved_missions_of_one_class_make_a_proposal() {
        let missions = approved_trio("implementer");
        let mut tools = BTreeMap::new();
        tools.insert(
            "M-001".to_string(),
            vec!["recall".to_string(), "write".to_string()],
        );
        tools.insert("M-002".to_string(), vec!["recall".to_string()]);
        let out = build(&missions, &tools, &HashSet::new());
        assert_eq!(out.len(), 1, "{out:?}");
        let p = &out[0];
        assert_eq!(p.role, "implementer");
        assert_eq!(p.task_class, "add learning loop");
        assert_eq!(p.name, "add-learning-loop");
        assert_eq!(p.evidence.len(), 3);
        assert_eq!(p.evidence[0].mission, "M-001");
        assert_eq!(p.evidence[0].tools, vec!["recall", "write"]);
        assert_eq!(p.description, "Use when the task is: add learning loop");
    }

    #[test]
    fn two_missions_are_below_the_threshold() {
        let mut missions = approved_trio("implementer");
        missions.truncate(2);
        let out = build(&missions, &BTreeMap::new(), &HashSet::new());
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn rejected_and_unfinished_missions_do_not_count() {
        let missions = vec![
            mission(
                "M-001",
                "Add the loop",
                "implementer",
                "hermes",
                "done",
                Some("approve"),
            ),
            mission(
                "M-002",
                "Add the loop",
                "implementer",
                "hermes",
                "done",
                Some("reject"),
            ),
            mission(
                "M-003",
                "Add the loop",
                "implementer",
                "hermes",
                "running",
                None,
            ),
            mission(
                "M-004",
                "Add the loop",
                "implementer",
                "hermes",
                "done",
                Some("approve"),
            ),
        ];
        let out = build(&missions, &BTreeMap::new(), &HashSet::new());
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn roles_group_separately_and_names_do_not_collide() {
        let mut missions = approved_trio("implementer");
        missions.extend(approved_trio("reviewer"));
        let out = build(&missions, &BTreeMap::new(), &HashSet::new());
        assert_eq!(out.len(), 2, "{out:?}");
        let mut names: Vec<&str> = out.iter().map(|p| p.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["add-learning-loop", "add-learning-loop-reviewer"]
        );
    }

    #[test]
    fn the_draft_has_the_frontmatter_steps_pitfalls_and_evidence() {
        let out = build(
            &approved_trio("implementer"),
            &BTreeMap::new(),
            &HashSet::new(),
        );
        let p = &out[0];
        assert!(p.draft.starts_with("---\nname: add-learning-loop\n"));
        assert!(p
            .draft
            .contains("\ndescription: Use when the task is: add learning loop\n"));
        assert!(p.description.chars().count() < 60);
        assert!(p.description.starts_with("Use when"));
        assert!(p.draft.contains("\n## Steps\n\n1. Read the repository"));
        assert!(p
            .draft
            .contains("\n## Pitfalls\n\n- Do not open the pull request"));
        assert!(p
            .draft
            .contains("## Evidence\n\n- M-001: Add the learning loop (hermes"));
        assert!(p.draft.contains("pull/M-001)"));
    }

    #[test]
    fn a_long_class_still_has_a_short_description() {
        let d = description("add a very long object name that keeps going past sixty characters");
        assert!(d.starts_with("Use when"));
        assert!(d.chars().count() < 60, "{d} is {} chars", d.chars().count());
        assert_eq!(description("x"), "Use when the task is: x");
    }

    #[test]
    fn a_dismissed_proposal_is_not_proposed_again() {
        let missions = approved_trio("implementer");
        let hash = hash_of("implementer", "add learning loop");
        let dismissed: HashSet<String> = [hash.clone()].into_iter().collect();
        let out = build(&missions, &BTreeMap::new(), &dismissed);
        assert!(out.is_empty(), "{out:?}");
        // The hash is stable for the same role and class.
        assert_eq!(hash, hash_of("implementer", "add learning loop"));
        assert_ne!(hash, hash_of("reviewer", "add learning loop"));
    }

    #[test]
    fn the_hash_and_name_are_safe_for_kv_and_the_filesystem() {
        let h = hash_of("implementer", "add learning loop");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(skill_name("add learning loop"), "add-learning-loop");
        assert_eq!(skill_name("1 2 3"), "task-1-2-3");
    }

    #[test]
    fn the_approval_prompt_names_the_skill_and_its_draft() {
        let prompt = skill_prompt(
            "add-learning-loop",
            Path::new("C:/cache/add-learning-loop.md"),
        );
        assert!(prompt.contains("add-learning-loop"));
        assert!(prompt.contains("C:/cache/add-learning-loop.md"));
        assert!(prompt.contains("skill creation tool"));
        assert!(prompt.contains("nothing else"));
    }

    #[test]
    fn approve_writes_the_draft_and_requires_the_skill_to_exist() {
        let out = build(
            &approved_trio("implementer"),
            &BTreeMap::new(),
            &HashSet::new(),
        );
        let p = &out[0];
        let asked = std::cell::RefCell::new(String::new());
        let turn = |_: Option<&str>, _: &Path, qpath: &Path| {
            let prompt = fs::read_to_string(qpath).unwrap();
            *asked.borrow_mut() = prompt;
            Ok("created".to_string())
        };
        let confirm = |_: Option<&str>, name: &str| Some(format!("skills/dev/{name}/SKILL.md"));
        let path = approve_with(p, None, &turn, &confirm).unwrap();
        assert_eq!(path, "skills/dev/add-learning-loop/SKILL.md");
        assert!(asked.borrow().contains("add-learning-loop"));
        let draft = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("agentos-studio")
            .join("skill-proposals")
            .join("add-learning-loop.md");
        assert!(draft.is_file());
        let _ = fs::remove_file(draft);

        // A turn that reports success but leaves no skill is an error, not a silent success.
        let missing = |_: Option<&str>, _: &str| None;
        assert!(approve_with(p, None, &turn, &missing).is_err());
    }

    #[test]
    fn a_bad_hash_is_refused_before_the_store() {
        let _g = crate::store::test_env_lock();
        let d = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(d.path().join("studio.db")));
        assert!(dismiss("../../etc").is_err());
        assert!(dismiss("0011").is_err());
        dismiss("0123456789abcdef").unwrap();
        let c = store::open().unwrap();
        assert!(store::kv_get(&c, "skills.dismissed.0123456789abcdef")
            .unwrap()
            .is_some());
        crate::store::set_test_db(None);
    }
}
