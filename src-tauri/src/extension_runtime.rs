//! Extension runtime (Phase G item 11): the only place Studio executes code from a capability
//! pack.
//!
//! Hook packs and command packs run their entrypoint as a scoped child process; an MCP bundle
//! registers its server in the managed MCP registry and is started, by Studio or by an agent,
//! only through `agentos-studio --extension-mcp <pack id>`, which applies the same scope. Every
//! run passes the same checks in the same order, and every outcome lands in the audit log:
//!
//! 1. `marketplace::verify`: the active copy still matches the hash the lockfile pins, so a
//!    tampered pack never runs.
//! 2. The kind: `hook-pack` and `command-pack` run, `mcp-bundle` registers. Nothing else executes.
//! 3. The trust tier: only `official` and `verified` (signed by a key the owner trusts). A
//!    `community` pack is untrusted for execution and is refused.
//! 4. The grant: the capability the kind needs (`hooks.exec`, `shell.exec` or `mcp.tools`) must be
//!    both declared in `capability.json` and granted in `capabilities.lock.json`.
//! 5. The permission action `capability.run` (default `ask`) through `permissions::guard_in`.
//!
//! The child gets `env_clear()` plus `permissions::unattended_env` minus the home and app data
//! variables (CLIs find their logins through those), plus `AGENTOS_PACK_ID`; `TEMP` and `TMP`
//! point at the run's sandbox. Its working directory is the mission worktree the owner chose or
//! a fresh sandbox folder, it has no stdin, its output is capped and redacted, and a run that
//! outlasts its timeout is killed. This is process scoping, not an OS sandbox: a granted hook
//! can still read files its user account can read, which is why execution needs a signed pack
//! and an explicit grant.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::marketplace::{self, LockEntry, Store};

pub const RUN_ACTION: &str = "capability.run";
pub const PACK_ENV: &str = "AGENTOS_PACK_ID";
pub const SERVE_FLAG: &str = "--extension-mcp";
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
pub const MAX_TIMEOUT_SECS: u64 = 300;
const OUTPUT_CAP: usize = 16 * 1024;
/// Variables `unattended_env` keeps that an extension does not get: they lead a child to the
/// owner's logins (Claude, gh, Hermes, git credentials), which is ambient authority.
const HOME_VARS: [&str; 6] = [
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "HOME",
    "APPDATA",
    "LOCALAPPDATA",
];

/// The capabilities a pack kind needs, declared and granted, before it may execute. An MCP
/// bundle starts a server process, so it needs `shell.exec` as well as `mcp.tools`. Pure.
pub fn required_capabilities(kind: &str) -> &'static [&'static str] {
    match kind {
        "hook-pack" => &["hooks.exec"],
        "command-pack" => &["shell.exec"],
        "mcp-bundle" => &["mcp.tools", "shell.exec"],
        _ => &[],
    }
}

/// Tiers whose code may execute. A community pack is untrusted for execution. Pure.
pub fn trusted_tier(tier: &str) -> bool {
    matches!(tier, "official" | "verified")
}

/// Why this pack may not execute, or None when the lock entry passes every static check. Pure.
pub fn refusal(e: &LockEntry, want_kind: &[&str]) -> Option<String> {
    if !want_kind.contains(&e.kind.as_str()) {
        return Some(format!(
            "{} is a {}; this runs only {}",
            e.id,
            e.kind,
            want_kind.join(" or ")
        ));
    }
    if !trusted_tier(&e.tier) {
        return Some(format!(
            "{} is a {} pack: only official or verified packs may execute code",
            e.id, e.tier
        ));
    }
    for cap in required_capabilities(&e.kind) {
        if !e.requested.iter().any(|c| c == cap) {
            return Some(format!(
                "{} does not declare {cap} in its capability.json",
                e.id
            ));
        }
        if !e.granted.iter().any(|c| c == cap) {
            return Some(format!(
                "{cap} was not granted to {} in capabilities.lock.json",
                e.id
            ));
        }
    }
    None
}

/// The interpreter and leading arguments for one entrypoint, by extension. Pure apart from
/// finding the interpreter on PATH.
fn interpreter(entry: &Path) -> Result<(PathBuf, Vec<OsString>)> {
    let ext = entry
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (tool, lead): (&str, Vec<&str>) = match ext.as_str() {
        "cmd" | "bat" if cfg!(windows) => ("cmd", vec!["/D", "/C"]),
        "ps1" => (
            "powershell",
            vec![
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ],
        ),
        "js" | "mjs" | "cjs" => ("node", vec![]),
        "py" => ("python", vec!["-I"]),
        "sh" => ("bash", vec!["--noprofile", "--norc"]),
        other => {
            bail!("entrypoint type '.{other}' cannot be run; use .cmd, .ps1, .js, .mjs, .py or .sh")
        }
    };
    let exe = crate::procs::find(tool).ok_or_else(|| anyhow!("{tool} is not installed"))?;
    let mut args: Vec<OsString> = lead.into_iter().map(OsString::from).collect();
    args.push(entry.as_os_str().to_owned());
    Ok((exe, args))
}

/// The environment one extension child starts with. Pure.
pub fn scoped_env<I>(base: I, pack_id: &str, sandbox: &Path) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut out: Vec<(OsString, OsString)> = crate::permissions::unattended_env(base, None, None)
        .into_iter()
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            !HOME_VARS.iter().any(|h| h.eq_ignore_ascii_case(&k))
                && !k.eq_ignore_ascii_case("TEMP")
                && !k.eq_ignore_ascii_case("TMP")
        })
        .collect();
    out.push(("TEMP".into(), sandbox.as_os_str().to_owned()));
    out.push(("TMP".into(), sandbox.as_os_str().to_owned()));
    out.push((PACK_ENV.into(), pack_id.into()));
    out
}

/// The folder of the active, verified copy of a pack.
fn active_dir(store: &Store, e: &LockEntry) -> PathBuf {
    store.pack_dir(&e.kind, &e.id).join(&e.hash)
}

/// Resolve an entrypoint inside the pack. It must be listed in the manifest and stay inside the
/// pack folder after symlinks resolve.
fn resolve_entry(dir: &Path, m: &marketplace::Manifest, wanted: Option<&str>) -> Result<PathBuf> {
    let rel = match wanted.map(str::trim).filter(|w| !w.is_empty()) {
        Some(w) => m
            .entrypoints
            .iter()
            .find(|e| e.as_str() == w)
            .ok_or_else(|| anyhow!("{w} is not an entrypoint of {}", m.id))?,
        None => m
            .entrypoints
            .first()
            .ok_or_else(|| anyhow!("{} has no entrypoint", m.id))?,
    };
    if !marketplace::safe_rel(rel) {
        bail!("entrypoint '{rel}' leaves the pack");
    }
    let root = plain(dir.canonicalize().context("pack folder")?);
    let p = plain(
        dir.join(rel)
            .canonicalize()
            .with_context(|| format!("entrypoint {rel} is missing"))?,
    );
    if !p.starts_with(&root) || !p.is_file() {
        bail!("entrypoint '{rel}' leaves the pack");
    }
    Ok(p)
}

fn plain(p: PathBuf) -> PathBuf {
    let s = p.display().to_string();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

fn read_manifest(dir: &Path) -> Result<marketplace::Manifest> {
    marketplace::parse_manifest(
        &std::fs::read_to_string(dir.join(marketplace::MANIFEST)).context("capability.json")?,
    )
}

/// A worktree an extension may run in: an absolute, existing folder that is a git checkout and
/// is not a system folder.
pub fn check_worktree(p: &Path) -> Result<PathBuf> {
    if !p.is_absolute() || !p.is_dir() {
        bail!("{} is not an existing absolute folder", p.display());
    }
    let low = p.display().to_string().to_lowercase().replace('\\', "/");
    if low.contains("/windows/") || low.ends_with("/windows") || low.contains("system32") {
        bail!("{} is a system folder", p.display());
    }
    if !p.join(".git").exists() {
        bail!(
            "{} is not a git worktree; extensions run in a mission worktree or a sandbox",
            p.display()
        );
    }
    Ok(p.to_path_buf())
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct RunRequest {
    pub id: String,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// A mission worktree; None runs in a fresh sandbox folder that is removed afterwards.
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct RunRecord {
    pub pack: String,
    pub kind: String,
    pub entrypoint: String,
    pub cwd: String,
    pub sandbox: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    /// Names (never values) of the variables the child received.
    pub env_keys: Vec<String>,
    pub outcome: String,
}

fn capped(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out: String = s.chars().take(OUTPUT_CAP).collect();
    if s.chars().count() > OUTPUT_CAP {
        out.push_str("\n[output truncated]");
    }
    crate::constitution_gate::redact(&out)
}

/// Read a pipe to the end on its own thread; the result arrives on the channel.
fn reader<R: Read + Send + 'static>(mut pipe: R) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = pipe.read_to_end(&mut b);
        let _ = tx.send(b);
    });
    rx
}

/// Kill the child and everything it started: a hook that spawned a helper must not outlive its
/// timeout.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        if let Some(tk) = crate::procs::find("taskkill") {
            let _ = crate::procs::command(&tk)
                .args(["/PID", &child.id().to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    #[cfg(unix)]
    {
        // The child leads its own process group (see `run_in`), so the group dies with it.
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn refuse(c: &Connection, id: &str, why: &str) -> anyhow::Error {
    let _ = crate::permissions::audit_in(c, "capability.refuse", id, why, "refused");
    anyhow!("{why}")
}

/// The checks every execution passes, in order. Returns the verified entry and manifest.
fn admit(
    c: &Connection,
    store: &Store,
    id: &str,
    kinds: &[&str],
    summary: &str,
    approved: bool,
) -> Result<(LockEntry, marketplace::Manifest, PathBuf)> {
    let e = marketplace::verify(id, store).map_err(|err| refuse(c, id, &err.to_string()))?;
    if let Some(why) = refusal(&e, kinds) {
        return Err(refuse(c, id, &why));
    }
    let dir = active_dir(store, &e);
    let m = read_manifest(&dir).map_err(|err| refuse(c, id, &format!("{err:#}")))?;
    if m.id != e.id || m.kind != e.kind {
        return Err(refuse(
            c,
            id,
            "capability.json does not match the lockfile entry",
        ));
    }
    crate::permissions::guard_in(c, RUN_ACTION, id, summary, approved)?;
    Ok((e, m, dir))
}

/// Run one hook or command entrypoint under the scope. `base_env` is the environment the scope
/// filters (the app passes its own; tests pass a fixture).
pub fn run_in(
    c: &Connection,
    store: &Store,
    req: &RunRequest,
    base_env: Vec<(OsString, OsString)>,
    sandbox_root: &Path,
    approved: bool,
) -> Result<RunRecord> {
    let summary = format!(
        "Run {} {} {}",
        req.id,
        req.entrypoint.as_deref().unwrap_or("(first entrypoint)"),
        req.args.join(" ")
    );
    let (e, m, dir) = admit(
        c,
        store,
        &req.id,
        &["hook-pack", "command-pack"],
        summary.trim(),
        approved,
    )?;
    let entry = resolve_entry(&dir, &m, req.entrypoint.as_deref())
        .map_err(|err| refuse(c, &req.id, &format!("{err:#}")))?;
    let run_dir = sandbox_root.join(format!("run-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&run_dir)?;
    let (cwd, sandbox) = match req
        .worktree
        .as_deref()
        .map(str::trim)
        .filter(|w| !w.is_empty())
    {
        Some(w) => (
            check_worktree(Path::new(w)).map_err(|err| refuse(c, &req.id, &format!("{err:#}")))?,
            false,
        ),
        None => (run_dir.clone(), true),
    };
    let (exe, mut args) =
        interpreter(&entry).map_err(|err| refuse(c, &req.id, &format!("{err:#}")))?;
    args.extend(req.args.iter().map(OsString::from));
    let env = scoped_env(base_env, &e.id, &run_dir);
    let timeout = Duration::from_secs(
        req.timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS),
    );
    let mut cmd = Command::new(&exe);
    cmd.args(&args)
        .current_dir(&cwd)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(crate::procs::CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let started = Instant::now();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    let out_rx = reader(child.stdout.take().expect("piped stdout"));
    let err_rx = reader(child.stderr.take().expect("piped stderr"));
    let mut timed_out = false;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            kill_tree(&mut child);
            break None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    // A grandchild that outlived the kill can hold a pipe open; never wait on it for long.
    let grace = Duration::from_secs(2);
    let stdout = capped(&out_rx.recv_timeout(grace).unwrap_or_default());
    let stderr = capped(&err_rx.recv_timeout(grace).unwrap_or_default());
    let exit_code = status.and_then(|s| s.code());
    let outcome = if timed_out {
        "timeout"
    } else if exit_code == Some(0) {
        "ok"
    } else {
        "failed"
    };
    let rec = RunRecord {
        pack: e.id.clone(),
        kind: e.kind.clone(),
        entrypoint: entry
            .strip_prefix(plain(dir.canonicalize().unwrap_or(dir.clone())))
            .map(|p| p.display().to_string().replace('\\', "/"))
            .unwrap_or_else(|_| entry.display().to_string()),
        cwd: cwd.display().to_string(),
        sandbox,
        exit_code,
        timed_out,
        duration_ms: started.elapsed().as_millis() as u64,
        stdout,
        stderr,
        env_keys: env
            .iter()
            .map(|(k, _)| k.to_string_lossy().to_string())
            .collect(),
        outcome: outcome.into(),
    };
    let _ = std::fs::remove_dir_all(&run_dir);
    crate::permissions::audit_in(
        c,
        RUN_ACTION,
        &e.id,
        &format!(
            "{} {} in {}{} exit {} in {} ms ({}, {})",
            e.kind,
            rec.entrypoint,
            if sandbox { "sandbox " } else { "worktree " },
            rec.cwd,
            exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "none".into()),
            rec.duration_ms,
            e.tier,
            e.hash
        ),
        outcome,
    )?;
    Ok(rec)
}

/// Where sandboxes live: `%APPDATA%\agentos-studio\extension-sandbox`.
pub fn sandbox_root() -> PathBuf {
    crate::store::studio_dir().join("extension-sandbox")
}

/// Run against the live store, lockfile and app environment.
pub fn run(req: &RunRequest, approved: bool) -> Result<RunRecord> {
    let c = crate::store::open()?;
    let mut base: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    base.retain(|(k, _)| !k.eq_ignore_ascii_case("PATH"));
    base.push(("PATH".into(), crate::procs::search_path()));
    run_in(&c, &Store::live(), req, base, &sandbox_root(), approved)
}

// ---------------------------------------------------------------------------------------------
// MCP bundles
// ---------------------------------------------------------------------------------------------

/// The one server an MCP bundle declares through `mcp_tools` (`server:tool`). Pure.
pub fn bundle_server(m: &marketplace::Manifest) -> Result<String> {
    let mut servers: Vec<&str> = m
        .mcp_tools
        .iter()
        .filter_map(|t| t.split_once(':').map(|(s, _)| s))
        .collect();
    servers.sort();
    servers.dedup();
    match servers.as_slice() {
        [one] => Ok(one.to_string()),
        [] => bail!("{} names no MCP tools, so its server is unknown", m.id),
        many => bail!(
            "{} names tools on {} servers; a bundle ships one server",
            m.id,
            many.len()
        ),
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct ToolRule {
    pub tool: String,
    pub policy: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct McpRegistration {
    pub pack: String,
    pub server: String,
    pub spec: crate::mcp_manager::ServerSpec,
    /// Every rule written to the tool policy, `*` included.
    pub rules: Vec<ToolRule>,
    pub agents: Vec<crate::mcp_manager::AgentOutcome>,
}

/// kv key recording the hash approved for a bundle server; `--extension-mcp` refuses any other.
pub fn approved_key(id: &str) -> String {
    format!("extension.mcp.{id}")
}

/// Register an MCP bundle: admit it, write its tool policy (`ask` for every declared tool and
/// for `*`, `deny` for tools the ceiling drops), record the approved hash and return the server
/// spec that starts it through the scope. Writes no agent config; `register_mcp` does that.
pub fn register_mcp_in(
    c: &Connection,
    store: &Store,
    id: &str,
    hub_exe: &Path,
    ctx: &marketplace::Ctx,
    approved: bool,
) -> Result<McpRegistration> {
    let (e, m, dir) = admit(
        c,
        store,
        id,
        &["mcp-bundle"],
        &format!("Register the MCP server bundled in {id}"),
        approved,
    )?;
    let server = bundle_server(&m).map_err(|err| refuse(c, id, &format!("{err:#}")))?;
    resolve_entry(&dir, &m, None).map_err(|err| refuse(c, id, &format!("{err:#}")))?;
    let denied: Vec<String> = marketplace::ceiling(&m, &e.tier, ctx)
        .into_iter()
        .filter(|v| v.capability == "mcp.tools")
        .flat_map(|v| v.denied_tools)
        .collect();
    let mut rules = Vec::new();
    for t in &m.mcp_tools {
        let (_, tool) = t.split_once(':').unwrap_or(("", t));
        let policy = if denied.contains(t) { "deny" } else { "ask" };
        crate::policy::set_default_in(c, &server, tool, policy)?;
        rules.push(ToolRule {
            tool: tool.into(),
            policy: crate::policy::decide_in(c, &server, tool),
        });
    }
    crate::policy::set_default_in(c, &server, "*", "ask")?;
    rules.push(ToolRule {
        tool: "*".into(),
        policy: crate::policy::decide_in(c, &server, "*"),
    });
    crate::store::kv_set(c, &approved_key(id), &e.hash)?;
    let spec = crate::mcp_manager::ServerSpec {
        name: server.clone(),
        transport: "stdio".into(),
        command_or_url: hub_exe.display().to_string(),
        args: vec![SERVE_FLAG.into(), id.into()],
        env_vars: Default::default(),
        auth: None,
    };
    spec.validate()?;
    crate::permissions::audit_in(
        c,
        "capability.mcp",
        id,
        &format!(
            "server {server} registered ({}); {}",
            e.hash,
            rules
                .iter()
                .map(|r| format!("mcp.{server}.{} = {}", r.tool, r.policy))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "ok",
    )?;
    Ok(McpRegistration {
        pack: id.into(),
        server,
        spec,
        rules,
        agents: Vec::new(),
    })
}

/// The executable agents start a bundle server through: the staged hub for this version when it
/// exists, otherwise this executable.
pub fn hub_exe() -> PathBuf {
    let staged = crate::mcp_manager::hub_binary_for(
        &crate::mcp_manager::hub_root(),
        env!("CARGO_PKG_VERSION"),
    );
    if staged.is_file() {
        staged
    } else {
        std::env::current_exe().unwrap_or(staged)
    }
}

/// Register against the live store and, when `targets` names agents, write their MCP configs.
pub fn register_mcp(id: &str, targets: &[String], approved: bool) -> Result<McpRegistration> {
    let c = crate::store::open()?;
    let mut reg = marketplace::with_live_ctx(|ctx| {
        register_mcp_in(&c, &Store::live(), id, &hub_exe(), ctx, approved)
    })?;
    if !targets.is_empty() {
        reg.agents = crate::mcp_manager::add_mcp_server(&reg.spec, targets)?;
    }
    Ok(reg)
}

/// `agentos-studio --extension-mcp <pack id>`: start a registered bundle's server in the scope,
/// with this process's stdio passed through. Refuses a pack that fails any check, a hash other
/// than the one approved at registration, and a `capability.run` policy of `deny`.
pub fn serve_from_args(args: &[String]) -> i32 {
    let Some(id) = args
        .iter()
        .position(|a| a == SERVE_FLAG)
        .and_then(|i| args.get(i + 1))
    else {
        eprintln!("usage: agentos-studio {SERVE_FLAG} <pack id>");
        return 2;
    };
    match serve(id) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{SERVE_FLAG} {id}: {e:#}");
            1
        }
    }
}

fn serve(id: &str) -> Result<i32> {
    let c = crate::store::open()?;
    let store = Store::live();
    let approved = crate::store::kv_get(&c, &approved_key(id))?
        .ok_or_else(|| refuse(&c, id, "this bundle was never registered in Studio"))?;
    if crate::permissions::policy_in(&c, RUN_ACTION)? == "deny" {
        return Err(refuse(&c, id, "capability.run is set to deny"));
    }
    // Approval was given in Studio at registration, for exactly this hash.
    let (e, m, dir) = admit(&c, &store, id, &["mcp-bundle"], "serve", true)?;
    if e.hash != approved {
        return Err(refuse(
            &c,
            id,
            "the installed copy differs from the one approved at registration; register it again",
        ));
    }
    let entry = resolve_entry(&dir, &m, None)?;
    let run_dir = sandbox_root().join(format!("mcp-{}", e.id));
    std::fs::create_dir_all(&run_dir)?;
    let (exe, args) = interpreter(&entry)?;
    let mut base: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    base.retain(|(k, _)| !k.eq_ignore_ascii_case("PATH"));
    base.push(("PATH".into(), crate::procs::search_path()));
    let env = scoped_env(base, &e.id, &run_dir);
    let _ = crate::permissions::audit_in(
        &c,
        RUN_ACTION,
        id,
        &format!("mcp-bundle server started in sandbox {}", run_dir.display()),
        "ok",
    );
    drop(c);
    let mut cmd = Command::new(&exe);
    cmd.args(&args)
        .current_dir(&run_dir)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(crate::procs::CREATE_NO_WINDOW);
    }
    let status = cmd.status()?;
    Ok(status.code().unwrap_or(1))
}

// ---------------------------------------------------------------------------------------------
// The audit view
// ---------------------------------------------------------------------------------------------

/// Extension runs, registrations and refusals, newest first.
pub fn list_runs_in(c: &Connection, limit: usize) -> Result<Vec<crate::permissions::AuditEntry>> {
    Ok(
        crate::permissions::list_audit_in(c, limit.saturating_mul(4).max(50), Some("capability."))?
            .into_iter()
            .filter(|a| {
                matches!(
                    a.action.as_str(),
                    "capability.run" | "capability.mcp" | "capability.refuse"
                )
            })
            .take(limit)
            .collect(),
    )
}

pub fn list_runs(limit: usize) -> Result<Vec<crate::permissions::AuditEntry>> {
    list_runs_in(&crate::store::open()?, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn manifest(id: &str, kind: &str, caps: &[&str], entry: &str, tools: &[&str]) -> String {
        let mut v = serde_json::json!({
            "schema_version": 1,
            "id": id,
            "name": "Fixture",
            "version": "1.0.0",
            "kind": kind,
            "description": "A fixture pack",
            "license": "MIT",
            "publisher": {"name": "K-ONE IT SOLUTIONS", "key_id": "k-one-official-1", "homepage": "https://example.com"},
            "entrypoints": [entry],
            "capabilities": caps,
            "requires": {"app_version": ">=0.15.0", "os": ["windows"], "runtimes": []}
        });
        if !tools.is_empty() {
            v["mcp_tools"] = serde_json::json!(tools);
        }
        v.to_string()
    }

    /// Install a pack into a temp store the way `activate` leaves it: content-addressed folder,
    /// `current` pointer and a lockfile entry.
    fn install(
        store: &Store,
        kind: &str,
        id: &str,
        files: &[(&str, &str)],
        tier: &str,
        requested: &[&str],
        granted: &[&str],
    ) -> String {
        let tmp = store.root.join("tmp-fixture");
        for (rel, text) in files {
            let p = tmp.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        let hash = marketplace::content_hash(&tmp, false).unwrap();
        let dest = store.pack_dir(kind, id).join(&hash);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::rename(&tmp, &dest).unwrap();
        fs::write(store.pack_dir(kind, id).join("current"), &hash).unwrap();
        let mut lock = marketplace::read_lock(store).unwrap();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        lock.schema_version = 1;
        lock.packs.insert(
            id.into(),
            LockEntry {
                id: id.into(),
                kind: kind.into(),
                name: "Fixture".into(),
                version: "1.0.0".into(),
                hash: hash.clone(),
                publisher: "K-ONE IT SOLUTIONS".into(),
                publisher_key_id: "k-one-official-1".into(),
                tier: tier.into(),
                source: "fixture".into(),
                installed_at: 0,
                requested: s(requested),
                granted: s(granted),
                history: vec![],
            },
        );
        fs::write(&store.lock, serde_json::to_string_pretty(&lock).unwrap()).unwrap();
        hash
    }

    struct Env {
        _t: tempfile::TempDir,
        store: Store,
        sandbox: PathBuf,
        c: Connection,
    }

    fn env() -> Env {
        let t = tempfile::tempdir().unwrap();
        crate::store::set_test_db(Some(t.path().join("studio.db")));
        let c = crate::store::open().unwrap();
        Env {
            store: Store {
                root: t.path().join("capabilities"),
                lock: t.path().join("capabilities.lock.json"),
            },
            sandbox: t.path().join("sandbox"),
            c,
            _t: t,
        }
    }

    #[cfg(windows)]
    const PRINT_ENV: (&str, &str) = ("hooks/print-env.cmd", "@set\r\n");
    #[cfg(not(windows))]
    const PRINT_ENV: (&str, &str) = ("hooks/print-env.sh", "env\n");

    fn base_env() -> Vec<(OsString, OsString)> {
        let mut base: Vec<(OsString, OsString)> = [
            ("OPENROUTER_API_KEY", "sk-or-v1-fixture-secret-value"),
            ("GITHUB_TOKEN", "fixture-github-token-value"),
            ("NOTION_API_KEY", "fixture-notion-value"),
            ("USERPROFILE", "C:\\Users\\fixture"),
            ("APPDATA", "C:\\Users\\fixture\\AppData\\Roaming"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        for k in ["PATH", "SYSTEMROOT", "WINDIR", "COMSPEC"] {
            if let Some(v) = std::env::var_os(k) {
                base.push((k.into(), v));
            }
        }
        base
    }

    #[test]
    fn hook_without_capability_refused() {
        let e = env();
        let id = "com.example.hook-no-grant";
        let m = manifest(id, "hook-pack", &["hooks.exec"], PRINT_ENV.0, &[]);
        // Declared but not granted.
        install(
            &e.store,
            "hook-pack",
            id,
            &[("capability.json", &m), PRINT_ENV],
            "official",
            &["hooks.exec"],
            &[],
        );
        let req = RunRequest {
            id: id.into(),
            ..Default::default()
        };
        let err = run_in(&e.c, &e.store, &req, base_env(), &e.sandbox, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("was not granted"), "{err}");
        // Not declared at all.
        let id2 = "com.example.hook-undeclared";
        let m2 = manifest(id2, "hook-pack", &["fs.read"], PRINT_ENV.0, &[]);
        install(
            &e.store,
            "hook-pack",
            id2,
            &[("capability.json", &m2), PRINT_ENV],
            "official",
            &["fs.read"],
            &["fs.read"],
        );
        let err = run_in(
            &e.c,
            &e.store,
            &RunRequest {
                id: id2.into(),
                ..Default::default()
            },
            base_env(),
            &e.sandbox,
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("does not declare hooks.exec"), "{err}");
        // A community pack is untrusted for execution even with the grant forged into the lock.
        let id3 = "com.example.hook-community";
        let m3 = manifest(id3, "hook-pack", &["hooks.exec"], PRINT_ENV.0, &[]);
        install(
            &e.store,
            "hook-pack",
            id3,
            &[("capability.json", &m3), PRINT_ENV],
            "community",
            &["hooks.exec"],
            &["hooks.exec"],
        );
        let err = run_in(
            &e.c,
            &e.store,
            &RunRequest {
                id: id3.into(),
                ..Default::default()
            },
            base_env(),
            &e.sandbox,
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("only official or verified"), "{err}");
        // A tampered pack never runs.
        let id4 = "com.example.hook-tampered";
        let m4 = manifest(id4, "hook-pack", &["hooks.exec"], PRINT_ENV.0, &[]);
        let h = install(
            &e.store,
            "hook-pack",
            id4,
            &[("capability.json", &m4), PRINT_ENV],
            "official",
            &["hooks.exec"],
            &["hooks.exec"],
        );
        fs::write(
            e.store
                .pack_dir("hook-pack", id4)
                .join(&h)
                .join(PRINT_ENV.0),
            "echo changed",
        )
        .unwrap();
        let err = run_in(
            &e.c,
            &e.store,
            &RunRequest {
                id: id4.into(),
                ..Default::default()
            },
            base_env(),
            &e.sandbox,
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("hash mismatch"), "{err}");
        // Every refusal is audited, and nothing ran.
        let runs = list_runs_in(&e.c, 20).unwrap();
        assert_eq!(
            runs.iter()
                .filter(|r| r.action == "capability.refuse")
                .count(),
            4
        );
        assert!(!runs.iter().any(|r| r.action == "capability.run"));
        crate::store::set_test_db(None);
    }

    #[test]
    fn hook_runs_in_scoped_env() {
        let e = env();
        let id = "com.example.print-env";
        let m = manifest(id, "hook-pack", &["hooks.exec"], PRINT_ENV.0, &[]);
        install(
            &e.store,
            "hook-pack",
            id,
            &[("capability.json", &m), PRINT_ENV],
            "official",
            &["hooks.exec"],
            &["hooks.exec"],
        );
        let req = RunRequest {
            id: id.into(),
            timeout_secs: Some(60),
            ..Default::default()
        };
        // `capability.run` defaults to ask: without approval it asks and runs nothing.
        let ask = run_in(&e.c, &e.store, &req, base_env(), &e.sandbox, false)
            .unwrap_err()
            .to_string();
        assert!(
            ask.starts_with(crate::permissions::APPROVAL_PREFIX),
            "{ask}"
        );
        let rec = run_in(&e.c, &e.store, &req, base_env(), &e.sandbox, true).unwrap();
        assert_eq!(rec.outcome, "ok", "{rec:?}");
        assert!(rec.sandbox);
        for secret in [
            "OPENROUTER_API_KEY",
            "GITHUB_TOKEN",
            "NOTION_API_KEY",
            "USERPROFILE",
            "APPDATA",
        ] {
            assert!(
                !rec.stdout.to_uppercase().contains(&format!("{secret}=")),
                "{secret} reached the child: {}",
                rec.stdout
            );
            assert!(!rec.env_keys.iter().any(|k| k.eq_ignore_ascii_case(secret)));
        }
        assert!(!rec.stdout.contains("fixture-secret-value"));
        assert!(
            rec.stdout.contains(&format!("{PACK_ENV}={id}")),
            "{}",
            rec.stdout
        );
        let allowed = |k: &str| {
            crate::permissions::UNATTENDED_ENV_ALLOW
                .iter()
                .any(|a| a.eq_ignore_ascii_case(k))
                || k == PACK_ENV
                || k == crate::permissions::UNATTENDED
        };
        for k in &rec.env_keys {
            assert!(allowed(k), "{k} is not on the allowlist");
        }
        // What the child itself saw: every variable it printed is allowlisted.
        for line in rec.stdout.lines() {
            if let Some((k, _)) = line.split_once('=') {
                if !k.is_empty()
                    && !k.starts_with(' ')
                    && k != "PROMPT"
                    && k != "PWD"
                    && k != "SHLVL"
                    && k != "_"
                {
                    assert!(allowed(k), "the child saw {k}");
                }
            }
        }
        // It ran in the sandbox, which is gone afterwards, and the run is audited.
        assert!(!Path::new(&rec.cwd).exists());
        let runs = list_runs_in(&e.c, 10).unwrap();
        assert!(runs
            .iter()
            .any(|r| r.action == "capability.run" && r.target == id && r.outcome == "ok"));
        crate::store::set_test_db(None);
    }

    #[test]
    fn hook_timeout_kills_the_child() {
        let e = env();
        let id = "com.example.slow-hook";
        #[cfg(windows)]
        let slow = ("hooks/slow.cmd", "@ping -n 30 127.0.0.1 > nul\r\n");
        #[cfg(not(windows))]
        let slow = ("hooks/slow.sh", "sleep 30\n");
        let m = manifest(id, "hook-pack", &["hooks.exec"], slow.0, &[]);
        install(
            &e.store,
            "hook-pack",
            id,
            &[("capability.json", &m), slow],
            "verified",
            &["hooks.exec"],
            &["hooks.exec"],
        );
        let rec = run_in(
            &e.c,
            &e.store,
            &RunRequest {
                id: id.into(),
                timeout_secs: Some(1),
                ..Default::default()
            },
            base_env(),
            &e.sandbox,
            true,
        )
        .unwrap();
        assert!(rec.timed_out && rec.outcome == "timeout", "{rec:?}");
        assert!(rec.duration_ms < 15_000);
        crate::store::set_test_db(None);
    }

    #[test]
    fn mcp_bundle_registers_with_ask_default() {
        let e = env();
        let id = "com.example.files-mcp";
        let m = manifest(
            id,
            "mcp-bundle",
            &["mcp.tools", "shell.exec"],
            "server/server.mjs",
            &[
                "files-demo:list_files",
                "files-demo:read_file",
                "files-demo:delete_file",
            ],
        );
        install(
            &e.store,
            "mcp-bundle",
            id,
            &[
                ("capability.json", &m),
                ("server/server.mjs", "// fixture\n"),
            ],
            "official",
            &["mcp.tools", "shell.exec"],
            &["mcp.tools", "shell.exec"],
        );
        let policy = |s: &str, t: &str| {
            if s == "files-demo" && t == "delete_file" {
                "deny".to_string()
            } else {
                "allow".to_string()
            }
        };
        let audit = |_: &str, _: &str, _: &str, _: &str| {};
        let ctx = marketplace::Ctx {
            trusted_keys: Default::default(),
            owner_ceiling: Default::default(),
            tool_policy: &policy,
            audit: &audit,
        };
        let hub = Path::new("C:/fixture/agentos-studio.exe");
        assert!(register_mcp_in(&e.c, &e.store, id, hub, &ctx, false)
            .unwrap_err()
            .to_string()
            .starts_with(crate::permissions::APPROVAL_PREFIX));
        let reg = register_mcp_in(&e.c, &e.store, id, hub, &ctx, true).unwrap();
        assert_eq!(reg.server, "files-demo");
        assert_eq!(reg.spec.args, vec![SERVE_FLAG.to_string(), id.to_string()]);
        assert!(reg.spec.env_vars.is_empty(), "no credentials ride along");
        assert_eq!(
            crate::policy::decide_in(&e.c, "files-demo", "list_files"),
            "ask"
        );
        assert_eq!(
            crate::policy::decide_in(&e.c, "files-demo", "read_file"),
            "ask"
        );
        assert_eq!(
            crate::policy::decide_in(&e.c, "files-demo", "delete_file"),
            "deny"
        );
        assert_eq!(
            crate::policy::decide_in(&e.c, "files-demo", "some_new_tool"),
            "ask",
            "mcp.files-demo.* = ask covers tools the bundle adds later"
        );
        assert!(reg.rules.iter().any(|r| r.tool == "*" && r.policy == "ask"));
        // An owner's later choice is kept when the bundle is registered again.
        crate::policy::set_in(&e.c, "files-demo", "read_file", "allow").unwrap();
        register_mcp_in(&e.c, &e.store, id, hub, &ctx, true).unwrap();
        assert_eq!(
            crate::policy::decide_in(&e.c, "files-demo", "read_file"),
            "allow"
        );
        let approved = crate::store::kv_get(&e.c, &approved_key(id)).unwrap();
        assert_eq!(
            approved,
            Some(marketplace::verify(id, &e.store).unwrap().hash)
        );
        // A bundle without the mcp.tools grant is refused.
        let id2 = "com.example.files-mcp-nogrant";
        let m2 = manifest(
            id2,
            "mcp-bundle",
            &["mcp.tools", "shell.exec"],
            "server/server.mjs",
            &["other:x"],
        );
        install(
            &e.store,
            "mcp-bundle",
            id2,
            &[("capability.json", &m2), ("server/server.mjs", "//\n")],
            "official",
            &["mcp.tools", "shell.exec"],
            &["shell.exec"],
        );
        assert!(register_mcp_in(&e.c, &e.store, id2, hub, &ctx, true)
            .unwrap_err()
            .to_string()
            .contains("mcp.tools was not granted"));
        crate::store::set_test_db(None);
    }

    /// The official commit-lint example, installed through the real pipeline (propose, then
    /// activate with the grants ticked) and run in a git worktree.
    #[test]
    fn commit_lint_example_runs_through_the_real_pipeline() {
        let (Some(git), Some(_node)) = (crate::procs::find("git"), crate::procs::find("node"))
        else {
            return;
        };
        let e = env();
        let policy = |_: &str, _: &str| "allow".to_string();
        let audit = |_: &str, _: &str, _: &str, _: &str| {};
        let ctx = marketplace::Ctx {
            trusted_keys: Default::default(),
            owner_ceiling: Default::default(),
            tool_policy: &policy,
            audit: &audit,
        };
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("examples/capabilities/hooks/commit-lint");
        let p = marketplace::propose(&src.display().to_string(), None, &e.store, &ctx).unwrap();
        let grants: Vec<String> = ["fs.read", "hooks.exec", "shell.exec"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        marketplace::activate(&p.token, &grants, false, &e.store, &ctx).unwrap();
        let repo = e._t.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let git_ok = |args: &[&str]| {
            crate::procs::command(&git)
                .arg("-C")
                .arg(&repo)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.com",
                ])
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        assert!(git_ok(&["init", "-q"]));
        fs::write(repo.join("a.txt"), "a").unwrap();
        assert!(git_ok(&["add", "a.txt"]));
        assert!(git_ok(&["commit", "-q", "-m", "feat(x): add a"]));
        let req = RunRequest {
            id: p.manifest.id.clone(),
            worktree: Some(repo.display().to_string()),
            timeout_secs: Some(60),
            ..Default::default()
        };
        let rec = run_in(&e.c, &e.store, &req, base_env(), &e.sandbox, true).unwrap();
        assert_eq!(rec.outcome, "ok", "{rec:?}");
        assert!(!rec.sandbox);
        assert!(rec.stdout.contains("every rule passes"), "{}", rec.stdout);
        assert!(
            rec.stdout.contains("solutions.k-one.commit-lint"),
            "{}",
            rec.stdout
        );
        // A failing message exits 1 and is recorded as failed.
        let bad = RunRequest {
            args: vec!["Added a thing".into()],
            ..req
        };
        let rec = run_in(&e.c, &e.store, &bad, base_env(), &e.sandbox, true).unwrap();
        assert_eq!(
            (rec.outcome.as_str(), rec.exit_code),
            ("failed", Some(1)),
            "{rec:?}"
        );
        crate::store::set_test_db(None);
    }

    #[test]
    fn the_extension_example_packs_parse_and_verify() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("examples")
            .join("capabilities");
        for (dir, kind) in [
            ("hooks/commit-lint", "hook-pack"),
            ("mcp/text-stats", "mcp-bundle"),
        ] {
            let d = root.join(dir);
            let t = tempfile::tempdir().unwrap();
            let store = Store {
                root: t.path().join("c"),
                lock: t.path().join("l.json"),
            };
            let policy = |_: &str, _: &str| "allow".to_string();
            let audit = |_: &str, _: &str, _: &str, _: &str| {};
            let ctx = marketplace::Ctx {
                trusted_keys: Default::default(),
                owner_ceiling: Default::default(),
                tool_policy: &policy,
                audit: &audit,
            };
            let p = marketplace::propose(&d.display().to_string(), None, &store, &ctx).unwrap();
            assert_eq!(p.manifest.kind, kind);
            assert_eq!(p.tier, "official", "{dir}: {:?}", p.refusal);
            assert!(!p.blocked, "{dir}: {:?} {:?}", p.refusal, p.findings);
            for c in required_capabilities(kind) {
                assert!(
                    p.manifest.capabilities.iter().any(|x| x == c),
                    "{dir} lacks {c}"
                );
            }
            if kind == "mcp-bundle" {
                assert!(bundle_server(&p.manifest).is_ok());
            }
        }
    }
}
