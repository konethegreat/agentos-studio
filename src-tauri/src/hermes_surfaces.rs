//! Hermes features Studio shows without rebuilding them: built-in memory and the journey graph,
//! insights, the supply-chain audit, cron jobs, profiles and `hermes send`.
//!
//! Rule: read Hermes files directly where they are plain data (MEMORY.md, USER.md,
//! `cron/jobs.json`, `profiles/`), and change anything only through the Hermes CLI, which
//! validates and locks. Studio never writes these files.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

use crate::procs;

fn strip_ansi(s: &str) -> String {
    let re = regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("ansi regex");
    re.replace_all(s, "").replace("\r\n", "\n")
}

/// Run `hermes <args>`; stdout on success, the error text otherwise. Hermes prints a warning line
/// per unreadable plugin on stderr; that is not a failure.
fn hermes(args: &[&str]) -> Result<String> {
    let out = procs::run("hermes", args).ok_or_else(|| anyhow!("hermes is not installed"))?;
    let stdout = strip_ansi(&String::from_utf8_lossy(&out.stdout));
    if out.status.success() {
        return Ok(stdout);
    }
    let stderr = strip_ansi(&String::from_utf8_lossy(&out.stderr));
    let msg = stderr
        .lines()
        .chain(stdout.lines())
        .rev()
        .find(|l| !l.starts_with("Skipping unreadable plugin") && !l.trim().is_empty())
        .unwrap_or("hermes failed")
        .to_string();
    bail!("hermes {}: {msg}", args.first().copied().unwrap_or(""))
}

// ------------------------------------------------------------------------------------ memory

#[derive(Serialize, Debug, Default)]
pub struct HermesMemory {
    pub home: String,
    pub memory_path: String,
    pub memory: String,
    pub user_path: String,
    pub user: String,
    /// `hermes memory status`, as printed.
    pub status: String,
    /// `hermes journey --json`: `{nodes, edges, clusters, memory, stats}`.
    pub journey: Value,
}

fn read_or_empty(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

pub fn memory_view() -> HermesMemory {
    let home = procs::hermes_home();
    let dir = home.join("memories");
    let (m, u) = (dir.join("MEMORY.md"), dir.join("USER.md"));
    HermesMemory {
        home: home.display().to_string(),
        memory: read_or_empty(&m),
        memory_path: m.display().to_string(),
        user: read_or_empty(&u),
        user_path: u.display().to_string(),
        status: hermes(&["memory", "status"]).unwrap_or_else(|e| e.to_string()),
        journey: hermes(&["journey", "--json"])
            .ok()
            .and_then(|t| serde_json::from_str(t.trim()).ok())
            .unwrap_or(Value::Null),
    }
}

/// Journey node ids are skill names or `memory:<source>:<n>:<hash>`; nothing else reaches the CLI.
pub fn valid_node(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

/// `hermes journey delete <node> -y`: archives a learned skill or removes a memory entry.
pub fn journey_delete(node: &str) -> Result<String> {
    if !valid_node(node) {
        bail!("Invalid journey node id");
    }
    hermes(&["journey", "delete", node, "-y"]).map(|s| s.trim().to_string())
}

// ------------------------------------------------------------------- insights and security

pub fn insights(days: u32) -> Result<String> {
    let d = days.clamp(1, 365).to_string();
    hermes(&["insights", "--days", &d])
}

/// `hermes security audit --json`: OSV findings for the Hermes venv, plugins and pinned MCP servers.
pub fn security_audit() -> Result<Value> {
    let text = hermes(&["security", "audit", "--json"]).or_else(|e| {
        // `--fail-on` is unset, but keep the JSON if a non-zero exit still printed it.
        let msg = e.to_string();
        msg.find('{').map(|i| msg[i..].to_string()).ok_or(e)
    })?;
    serde_json::from_str(text.trim()).map_err(|e| anyhow!("unreadable audit output: {e}"))
}

// -------------------------------------------------------------------------------------- cron

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct CronJob {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub schedule_display: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub deliver: Option<String>,
}

pub fn cron_jobs_in(home: &Path) -> Vec<CronJob> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        jobs: Vec<CronJob>,
    }
    fs::read_to_string(home.join("cron").join("jobs.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<File>(&t).ok())
        .map(|f| f.jobs)
        .unwrap_or_default()
}

pub fn cron_jobs() -> Vec<CronJob> {
    cron_jobs_in(&procs::hermes_home())
}

fn valid_job_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Pause, resume, run now or remove one job through `hermes cron`.
pub fn cron_action(id: &str, action: &str) -> Result<String> {
    if !valid_job_id(id) {
        bail!("Invalid job id");
    }
    let verb = match action {
        "pause" | "resume" | "run" | "remove" => action,
        other => bail!("Unknown cron action {other}"),
    };
    hermes(&["cron", verb, id]).map(|s| s.trim().to_string())
}

#[derive(Deserialize, Debug, Default)]
pub struct CronSpec {
    pub schedule: String,
    pub prompt: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub paused: bool,
}

/// Arguments for `hermes cron create`. Output goes to `local` (the run is kept in Hermes and shown
/// here) unless Hermes has a delivery target configured by the user later. Pure, unit tested.
pub fn cron_create_args(spec: &CronSpec) -> Result<Vec<String>> {
    let schedule = spec.schedule.trim();
    let prompt = spec.prompt.trim();
    if schedule.is_empty() || prompt.is_empty() {
        bail!("A schedule and a prompt are both required");
    }
    let mut a: Vec<String> = ["cron", "create", "--deliver", "local"]
        .map(String::from)
        .to_vec();
    if let Some(n) = spec
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        a.extend(["--name".into(), n.into()]);
    }
    if let Some(w) = spec
        .workdir
        .as_deref()
        .map(str::trim)
        .filter(|w| !w.is_empty())
    {
        if !Path::new(w).is_dir() {
            bail!("Working folder {w} does not exist");
        }
        a.extend(["--workdir".into(), w.into()]);
    }
    if spec.paused {
        a.push("--paused".into());
    }
    a.push(schedule.into());
    a.push(prompt.into());
    Ok(a)
}

pub fn cron_create(spec: &CronSpec) -> Result<String> {
    let args = cron_create_args(spec)?;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    hermes(&refs).map(|s| s.trim().to_string())
}

// ---------------------------------------------------------------------------------- profiles

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    pub path: String,
    pub model: String,
    pub provider: String,
    /// `implementer`, `reviewer`, both or none, as assigned in Studio for Hermes missions.
    pub roles: Vec<String>,
}

fn model_of(config: &Path) -> (String, String) {
    let text = fs::read_to_string(config).unwrap_or_default();
    let mut model = String::new();
    let mut provider = String::new();
    let mut in_model = false;
    for line in text.lines() {
        if !line.starts_with(' ') && !line.trim().is_empty() && !line.trim_start().starts_with('#')
        {
            in_model = line.trim_end() == "model:";
            continue;
        }
        if in_model {
            let t = line.trim();
            for (key, slot) in [("default:", &mut model), ("provider:", &mut provider)] {
                if let Some(v) = t.strip_prefix(key) {
                    if slot.is_empty() {
                        *slot = v.trim().trim_matches('"').to_string();
                    }
                }
            }
        }
    }
    (model, provider)
}

pub const ROLE_KEYS: [(&str, &str); 2] = [
    ("implementer", "hermes.profile.implementer"),
    ("reviewer", "hermes.profile.reviewer"),
];

/// The Hermes profile Studio assigned to a mission role, if any.
pub fn profile_for_role(role: &str) -> Option<String> {
    let key = ROLE_KEYS.iter().find(|(r, _)| *r == role)?.1;
    let c = crate::store::open().ok()?;
    crate::store::kv_get(&c, key)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
}

pub fn profiles_in(home: &Path) -> Vec<Profile> {
    let mut out = Vec::new();
    let (m, p) = model_of(&home.join("config.yaml"));
    out.push(Profile {
        name: "default".into(),
        path: home.display().to_string(),
        model: m,
        provider: p,
        roles: vec![],
    });
    if let Ok(rd) = fs::read_dir(home.join("profiles")) {
        let mut dirs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("config.yaml").is_file())
            .collect();
        dirs.sort();
        for d in dirs {
            let (m, p) = model_of(&d.join("config.yaml"));
            out.push(Profile {
                name: d.file_name().unwrap_or_default().to_string_lossy().into(),
                path: d.display().to_string(),
                model: m,
                provider: p,
                roles: vec![],
            });
        }
    }
    out
}

pub fn profiles() -> Vec<Profile> {
    let mut list = profiles_in(&procs::hermes_home());
    for (role, _) in ROLE_KEYS {
        if let Some(name) = profile_for_role(role) {
            if let Some(p) = list.iter_mut().find(|p| p.name == name) {
                p.roles.push(role.into());
            }
        }
    }
    list
}

pub fn set_role_profile(role: &str, profile: &str) -> Result<()> {
    let key = ROLE_KEYS
        .iter()
        .find(|(r, _)| *r == role)
        .ok_or_else(|| anyhow!("Unknown role {role}"))?
        .1;
    if !profile.is_empty() && !profiles().iter().any(|p| p.name == profile) {
        bail!("No Hermes profile named {profile}");
    }
    let c = crate::store::open()?;
    crate::store::kv_set(&c, key, profile)
}

// ------------------------------------------------------------------------------------- send

pub const NOTIFY_KEY: &str = "hermes.notify_target";

fn valid_target(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 200
        && !t.starts_with('-')
        && t.chars().all(|c| !c.is_control() && c != ' ')
}

/// `hermes send -t <target> -q <message>` through the messaging platforms Hermes already has.
pub fn send(target: &str, message: &str) -> Result<String> {
    let target = target.trim();
    if !valid_target(target) {
        bail!("Invalid delivery target");
    }
    if message.trim().is_empty() {
        bail!("Empty message");
    }
    // `--` ends the options, so a message can never be read as a flag.
    hermes(&["send", "-t", target, "-q", "--", message]).map(|s| s.trim().to_string())
}

pub fn notify_target() -> Option<String> {
    let c = crate::store::open().ok()?;
    crate::store::kv_get(&c, NOTIFY_KEY)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
}

/// Best effort: tell the user's messaging target that a mission finished. Never fails the caller.
pub fn notify_mission(title: &str, outcome: &str, pr_url: Option<&str>) {
    let Some(target) = notify_target() else {
        return;
    };
    let mut msg = format!("AgentOS Studio: mission \"{title}\" {outcome}.");
    if let Some(u) = pr_url.filter(|u| !u.is_empty()) {
        msg.push_str(&format!(" {u}"));
    }
    let _ = send(&target, &msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_jobs_and_profiles_read_from_hermes_files() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("cron")).unwrap();
        fs::write(
            d.path().join("cron/jobs.json"),
            r#"{"jobs":[{"id":"dc6144885266","name":"probe","prompt":"Reply OK","schedule_display":"every 120m","enabled":false,"state":"paused","unknown":1}],"updated_at":"x"}"#,
        )
        .unwrap();
        let jobs = cron_jobs_in(d.path());
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state.as_deref(), Some("paused"));
        fs::write(
            d.path().join("config.yaml"),
            "model:\n  # Default model\n  default: \"gemini-4-argon-medium\"\n  provider: \"antigravity-oauth\"\nagent:\n  default: x\n",
        )
        .unwrap();
        fs::create_dir_all(d.path().join("profiles/reviewer")).unwrap();
        fs::write(
            d.path().join("profiles/reviewer/config.yaml"),
            "model:\n  default: m2\n",
        )
        .unwrap();
        let p = profiles_in(d.path());
        assert_eq!(p[0].model, "gemini-4-argon-medium");
        assert_eq!(p[0].provider, "antigravity-oauth");
        assert_eq!(
            (p[1].name.as_str(), p[1].model.as_str()),
            ("reviewer", "m2")
        );
    }

    #[test]
    fn cron_create_builds_safe_arguments() {
        let a = cron_create_args(&CronSpec {
            schedule: "every 2h".into(),
            prompt: "check CI".into(),
            name: Some("ci".into()),
            paused: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            a.join(" "),
            "cron create --deliver local --name ci --paused every 2h check CI"
        );
        assert!(cron_create_args(&CronSpec::default()).is_err());
        assert!(cron_action("../x", "pause").is_err());
        assert!(cron_action("abc123", "delete-all").is_err());
    }

    #[test]
    fn ids_and_targets_are_validated_before_the_cli() {
        assert!(valid_node("memory:profile:7:62719cdcd928"));
        assert!(valid_node("agentos-studio-dev"));
        assert!(!valid_node("x; rm -rf /"));
        assert!(valid_target("whatsapp"));
        assert!(valid_target("telegram:-1001234567890:17585"));
        assert!(!valid_target("--help"));
        assert!(!valid_target("a b"));
    }
}
