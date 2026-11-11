//! Tauri commands: the only surface the React UI talks to.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

use crate::boot::{self, BootReport};
use crate::bus::{self, BusState, WorkerInfo};
use crate::chat_stream::{self, ApplyResult, ChatRequest, ChatStarted, ChatState};
use crate::cli_runner::{self, CliState};
use crate::constitution;
use crate::hermes_engine::{
    self, ChatMessage, HermesState, HermesStatus, ProviderConfig, ProviderTest, SessionSummary,
};
use crate::mcp_manager::{self, AgentOutcome, CatalogEntry, McpServer, ProbeResult, ServerSpec};
use crate::projects::{Project, ProjectSession};
use crate::settings::Settings;
use crate::skills::Skill;
use crate::vault::{self, Board, NoteInfo, SearchHit, Vault};
use crate::{bootstrapper, broadcast, fleet};

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub bus: BusState,
    pub hermes: Arc<HermesState>,
    pub chat: ChatState,
    pub cli: CliState,
    pub pty: crate::pty::PtyState,
}

type CmdResult<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

fn current_vault(state: &AppState) -> Vault {
    let s = state.settings.lock().map(|s| s.clone()).unwrap_or_default();
    Vault::new(s.vault_path())
}

fn current_settings(state: &AppState) -> Settings {
    state.settings.lock().map(|s| s.clone()).unwrap_or_default()
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

#[derive(Serialize)]
pub struct SettingsView {
    pub vault: String,
    pub scan_roots: Vec<String>,
    pub studio_binary: String,
    pub version: String,
    pub home: String,
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsView {
    let s = current_settings(&state);
    SettingsView {
        vault: s.vault_path().display().to_string(),
        scan_roots: s.roots().iter().map(|p| p.display().to_string()).collect(),
        studio_binary: std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        version: env!("CARGO_PKG_VERSION").into(),
        home: crate::settings::home().display().to_string(),
    }
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    vault: String,
    scan_roots: Vec<String>,
) -> CmdResult<SettingsView> {
    {
        let mut s = state.settings.lock().map_err(|e| e.to_string())?;
        s.vault = Some(vault.trim().to_string()).filter(|v| !v.is_empty());
        s.scan_roots = scan_roots
            .into_iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        s.save().map_err(err)?;
    }
    let v = current_vault(&state);
    let _ = v.ensure_dirs();
    bus::start_watcher(app, &state.bus, &v.root).map_err(err)?;
    Ok(get_settings(state))
}

#[tauri::command]
pub async fn check_full_stack_status(
    state: State<'_, AppState>,
) -> CmdResult<bootstrapper::StackStatus> {
    let v = current_vault(&state);
    blocking(move || Ok(bootstrapper::check_full_stack_status(&v))).await
}

/// Claude Desktop, Cursor, VS Code, Windsurf and Codex: installed, and connected to the hub?
#[tauri::command]
pub async fn clients_list() -> CmdResult<Vec<crate::clients::Client>> {
    blocking(|| {
        let exe = crate::mcp_manager::hub_binary_for(
            &crate::mcp_manager::hub_root(),
            env!("CARGO_PKG_VERSION"),
        );
        Ok(crate::clients::list(&exe.display().to_string()))
    })
    .await
}

/// Give one app the read-only hub (no identity token, so it can read but never write as an agent).
#[tauri::command]
pub async fn client_connect(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let v = current_vault(&state);
    blocking(move || {
        let exe = crate::mcp_manager::stage_hub_binary()?;
        crate::clients::connect(&id, &exe.display().to_string(), &v.root)
    })
    .await
}

#[tauri::command]
pub async fn client_disconnect(id: String) -> CmdResult<()> {
    blocking(move || crate::clients::disconnect(&id)).await
}

/// What "Set up everything" will do on this machine (docs/ONE-CLICK-SETUP.md).
#[tauri::command]
pub async fn setup_plan(state: State<'_, AppState>) -> CmdResult<crate::setup_flow::Plan> {
    let v = current_vault(&state);
    blocking(move || Ok(bootstrapper::setup_plan(&v))).await
}

#[tauri::command]
pub fn install_or_repair_component(
    app: AppHandle,
    state: State<'_, AppState>,
    component: String,
) -> String {
    bootstrapper::install_or_repair_component(app, current_vault(&state), component)
}

#[tauri::command]
pub fn scaffold_vault(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> CmdResult<Vec<String>> {
    let v = Vault::new(PathBuf::from(&path));
    let created = v.scaffold().map_err(err)?;
    {
        let mut s = state.settings.lock().map_err(|e| e.to_string())?;
        s.vault = Some(path);
        s.save().map_err(err)?;
    }
    bus::start_watcher(app, &state.bus, &v.root).map_err(err)?;
    Ok(created)
}

#[tauri::command]
pub fn list_board(state: State<'_, AppState>) -> Board {
    current_vault(&state).board()
}

#[tauri::command]
pub fn read_task(state: State<'_, AppState>, task_id: String) -> CmdResult<Value> {
    let v = current_vault(&state);
    let (task, raw) = v.get_task(&task_id).map_err(err)?;
    let log = std::fs::read_to_string(v.worker_logs().join(format!("{}.log", task.id)))
        .unwrap_or_default();
    Ok(json!({"task": task, "markdown": raw, "log": log}))
}

#[tauri::command]
pub async fn quick_mission_dispatch(
    app: AppHandle,
    state: State<'_, AppState>,
    prompt: String,
    project_path: Option<String>,
    target_agent: String,
    queue_only: Option<bool>,
) -> CmdResult<Value> {
    let v = current_vault(&state);
    let workers = state.bus.workers.clone();
    bus::quick_mission_dispatch(
        app,
        workers,
        v,
        &prompt,
        project_path,
        &target_agent,
        queue_only.unwrap_or(false),
    )
    .map_err(err)
}

#[tauri::command]
pub async fn run_task_now(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    agent: String,
) -> CmdResult<WorkerInfo> {
    let v = current_vault(&state);
    let workers = state.bus.workers.clone();
    bus::run_task_now(app, workers, v, &task_id, &agent).map_err(err)
}

#[tauri::command]
pub fn move_task(state: State<'_, AppState>, task_id: String, to: String) -> CmdResult<String> {
    let target = vault::State::parse(&to).map_err(err)?;
    current_vault(&state)
        .move_task(&task_id, target, "human")
        .map_err(err)
}

#[tauri::command]
pub fn stop_worker(state: State<'_, AppState>, task_id: String) -> CmdResult<String> {
    bus::stop_worker(&state.bus.workers, &task_id).map_err(err)
}

#[tauri::command]
pub fn list_workers(state: State<'_, AppState>) -> Vec<WorkerInfo> {
    state
        .bus
        .workers
        .lock()
        .map(|w| w.values().cloned().collect())
        .unwrap_or_default()
}

#[tauri::command]
pub async fn scan_fleet(state: State<'_, AppState>) -> CmdResult<Vec<fleet::RepoInfo>> {
    let s = current_settings(&state);
    let raw = constitution::load_raw(&s.vault_path());
    blocking(move || Ok(fleet::scan(&s.roots(), &raw))).await
}

#[tauri::command]
pub async fn sync_fleet_rules(
    state: State<'_, AppState>,
    repo_paths: Vec<String>,
    auto_commit_rules: bool,
) -> CmdResult<Vec<fleet::SyncResult>> {
    let raw = constitution::load_raw(&current_vault(&state).root);
    blocking(move || {
        Ok(fleet::sync_fleet_rules(
            &repo_paths,
            auto_commit_rules,
            &raw,
        ))
    })
    .await
}

#[tauri::command]
pub async fn create_new_project_from_ui(
    state: State<'_, AppState>,
    name: String,
    parent_dir: String,
    stack_template: String,
    create_github_repo: bool,
    private_repo: bool,
) -> CmdResult<Value> {
    let v = current_vault(&state);
    let raw = constitution::load_raw(&v.root);
    blocking(move || {
        fleet::create_project(
            &v,
            &name,
            &parent_dir,
            &stack_template,
            create_github_repo,
            private_repo,
            &raw,
        )
    })
    .await
}

#[tauri::command]
pub fn load_constitution(state: State<'_, AppState>) -> Value {
    let v = current_vault(&state);
    let raw = constitution::load_raw(&v.root);
    let system = std::fs::read_to_string(v.memory().join("SYSTEM_CONTEXT.md")).unwrap_or_default();
    json!({"constitution": raw, "system_context": system, "laws": constitution::laws(&raw), "path": constitution::constitution_path(&v.root).display().to_string()})
}

#[tauri::command]
pub fn toggle_law(constitution_text: String, number: u32, enabled: bool) -> Value {
    let next = constitution::set_law_enabled(&constitution_text, number, enabled);
    json!({"constitution": next, "laws": constitution::laws(&next)})
}

#[tauri::command]
pub async fn save_and_broadcast(
    state: State<'_, AppState>,
    constitution_text: String,
    system_context: String,
    include_repos: bool,
) -> CmdResult<broadcast::BroadcastReport> {
    let v = current_vault(&state);
    let s = current_settings(&state);
    blocking(move || {
        broadcast::save_and_broadcast(&v, &s, &constitution_text, &system_context, include_repos)
    })
    .await
}

#[tauri::command]
pub fn list_memory_notes(state: State<'_, AppState>) -> Vec<NoteInfo> {
    current_vault(&state).list_memory_notes()
}

#[tauri::command]
pub fn read_note(state: State<'_, AppState>, relative_path: String) -> CmdResult<String> {
    current_vault(&state).read_note(&relative_path).map_err(err)
}

#[tauri::command]
pub fn search_memory(state: State<'_, AppState>, query: String) -> CmdResult<Vec<SearchHit>> {
    current_vault(&state)
        .search_shared_memory(&query)
        .map_err(err)
}

// ---------------------------------------------------------------------------------------------
// v0.2: Hermes engine, provider switchboard and sessions
// ---------------------------------------------------------------------------------------------

#[tauri::command]
pub async fn get_hermes_status(
    state: State<'_, AppState>,
    refresh: Option<bool>,
) -> CmdResult<HermesStatus> {
    let h = state.hermes.clone();
    let refresh = refresh.unwrap_or(false);
    blocking(move || Ok(h.status(refresh))).await
}

#[tauri::command]
pub async fn get_provider_config() -> CmdResult<ProviderConfig> {
    blocking(hermes_engine::get_provider_config).await
}

#[derive(Serialize)]
pub struct ProviderSwitch {
    pub config: ProviderConfig,
    pub log: Vec<String>,
}

#[tauri::command]
pub async fn set_active_provider(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: String,
    model: String,
    api_key: Option<String>,
    base_url: Option<String>,
) -> CmdResult<ProviderSwitch> {
    let h = state.hermes.clone();
    let out = blocking(move || {
        let (config, log) = hermes_engine::set_active_provider(
            &provider,
            &model,
            api_key.as_deref(),
            base_url.as_deref(),
        )?;
        Ok(ProviderSwitch { config, log })
    })
    .await?;
    let status = tauri::async_runtime::spawn_blocking(move || h.status(false))
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit(hermes_engine::EV_STATUS, status);
    Ok(out)
}

#[tauri::command]
pub async fn list_available_models(provider: String) -> CmdResult<Vec<String>> {
    blocking(move || hermes_engine::list_available_models(&provider)).await
}

#[tauri::command]
pub async fn test_provider(
    provider: String,
    model: String,
    base_url: Option<String>,
) -> CmdResult<ProviderTest> {
    blocking(move || hermes_engine::test_provider(&provider, &model, base_url.as_deref())).await
}

#[tauri::command]
pub async fn login_provider(provider: String) -> CmdResult<String> {
    blocking(move || hermes_engine::login_provider(&provider)).await
}

#[tauri::command]
pub async fn list_hermes_sessions(
    query: Option<String>,
    limit: Option<usize>,
) -> CmdResult<Vec<SessionSummary>> {
    blocking(move || hermes_engine::list_hermes_sessions(query.as_deref(), limit.unwrap_or(300)))
        .await
}

#[tauri::command]
pub async fn load_session_messages(session_id: String) -> CmdResult<Vec<ChatMessage>> {
    blocking(move || hermes_engine::load_session_messages(&session_id)).await
}

#[tauri::command]
pub async fn rename_session(session_id: String, title: String) -> CmdResult<()> {
    blocking(move || hermes_engine::rename_session(&session_id, &title)).await
}

#[tauri::command]
pub async fn delete_session(session_id: String, approved: Option<bool>) -> CmdResult<()> {
    blocking(move || {
        crate::permissions::run_guarded(
            "session.delete",
            &session_id,
            &format!(
                "Permanently delete Hermes session {session_id} (hermes sessions delete --yes)"
            ),
            approved.unwrap_or(false),
            || hermes_engine::delete_session(&session_id),
        )
    })
    .await
}

#[tauri::command]
pub async fn export_session_to_obsidian(
    state: State<'_, AppState>,
    session_id: String,
) -> CmdResult<String> {
    let v = current_vault(&state);
    blocking(move || hermes_engine::export_session_to_obsidian(&session_id, &v)).await
}

// ---------------------------------------------------------------------------------------------
// v0.2: MCP control plane
// ---------------------------------------------------------------------------------------------

#[tauri::command]
pub async fn get_all_mcp_servers() -> CmdResult<Vec<McpServer>> {
    blocking(|| Ok(mcp_manager::get_all_mcp_servers())).await
}

fn targets_for(sync_to_all_agents: bool, agents: Option<Vec<String>>) -> Vec<String> {
    match agents.filter(|a| !a.is_empty()) {
        Some(a) => a,
        None if sync_to_all_agents => mcp_manager::AGENTS.iter().map(|s| s.to_string()).collect(),
        None => vec!["hermes".to_string()],
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn add_mcp_server(
    name: String,
    transport: String,
    command_or_url: String,
    args: Vec<String>,
    env_vars: BTreeMap<String, String>,
    sync_to_all_agents: bool,
    auth: Option<String>,
    agents: Option<Vec<String>>,
) -> CmdResult<Vec<AgentOutcome>> {
    let spec = ServerSpec {
        name,
        transport,
        command_or_url,
        args,
        env_vars,
        auth,
    };
    let targets = targets_for(sync_to_all_agents, agents);
    blocking(move || mcp_manager::add_mcp_server(&spec, &targets)).await
}

#[tauri::command]
pub async fn remove_mcp_server(name: String) -> CmdResult<Vec<AgentOutcome>> {
    blocking(move || mcp_manager::remove_mcp_server(&name)).await
}

#[tauri::command]
pub async fn test_mcp_server(name: String) -> CmdResult<ProbeResult> {
    blocking(move || mcp_manager::test_mcp_server(&name)).await
}

#[tauri::command]
pub async fn trigger_mcp_oauth(name: String) -> CmdResult<String> {
    blocking(move || mcp_manager::trigger_mcp_oauth(&name)).await
}

#[tauri::command]
pub async fn set_mcp_enabled(name: String, enabled: bool) -> CmdResult<String> {
    blocking(move || mcp_manager::set_mcp_enabled(&name, enabled)).await
}

#[tauri::command]
pub async fn sync_mcp_server(name: String, agents: Vec<String>) -> CmdResult<Vec<AgentOutcome>> {
    blocking(move || mcp_manager::sync_server_to_agents(&name, &agents)).await
}

#[tauri::command]
pub async fn cutover_agentos_hub(state: State<'_, AppState>) -> CmdResult<Vec<AgentOutcome>> {
    let root = current_vault(&state).root.clone();
    blocking(move || mcp_manager::cutover_agentos_hub(&root)).await
}

#[tauri::command]
pub fn mcp_catalog(state: State<'_, AppState>) -> Vec<CatalogEntry> {
    mcp_manager::catalog(&current_vault(&state).root)
}

#[tauri::command]
pub async fn install_catalog_server(
    state: State<'_, AppState>,
    id: String,
    values: BTreeMap<String, String>,
    sync_to_all_agents: bool,
) -> CmdResult<Vec<AgentOutcome>> {
    let root = current_vault(&state).root.clone();
    blocking(move || {
        let cat = mcp_manager::catalog(&root);
        let entry = cat
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| anyhow::anyhow!("No catalog entry '{id}'"))?;
        let spec = mcp_manager::spec_from_catalog(entry, &values)?;
        let targets = targets_for(sync_to_all_agents, None);
        mcp_manager::add_mcp_server(&spec, &targets)
    })
    .await
}

#[tauri::command]
pub async fn gh_token() -> CmdResult<String> {
    blocking(mcp_manager::gh_token).await
}

// ---------------------------------------------------------------------------------------------
// v0.2: chat streaming and the in-app terminal
// ---------------------------------------------------------------------------------------------

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn send_chat_message(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: Option<String>,
    prompt: String,
    working_dir: String,
    agent_backend: String,
    model: Option<String>,
    skills: Option<Vec<String>>,
    turn: Option<u32>,
) -> CmdResult<ChatStarted> {
    chat_stream::send_chat_message(
        app,
        &state.chat,
        ChatRequest {
            session_id,
            prompt,
            working_dir,
            agent_backend,
            model,
            provider: None,
            skills: skills.unwrap_or_default(),
            turn,
        },
    )
    .map_err(err)
}

#[tauri::command]
pub fn stop_active_chat(state: State<'_, AppState>) -> CmdResult<String> {
    chat_stream::stop_active_chat(&state.chat).map_err(err)
}

#[tauri::command]
pub fn active_chat(state: State<'_, AppState>) -> Option<String> {
    chat_stream::active_run(&state.chat)
}

#[tauri::command]
pub async fn apply_code_to_file(
    working_dir: String,
    rel_path: String,
    content: String,
) -> CmdResult<ApplyResult> {
    blocking(move || chat_stream::apply_code_to_file(&working_dir, &rel_path, &content)).await
}

#[tauri::command]
pub async fn execute_cli_command(
    app: AppHandle,
    state: State<'_, AppState>,
    command: String,
    cwd: String,
) -> CmdResult<String> {
    cli_runner::execute_cli_command(app, &state.cli, &command, &cwd).map_err(err)
}

#[tauri::command]
pub fn kill_cli_command(state: State<'_, AppState>, run_id: String) -> CmdResult<()> {
    cli_runner::kill_cli_command(&state.cli, &run_id).map_err(err)
}

// ---------------------------------------------------------------------------------------------
// v0.4: startup checks, logs and diagnostics
// ---------------------------------------------------------------------------------------------

#[tauri::command]
pub fn boot_snapshot() -> Option<BootReport> {
    boot::load_snapshot()
}

#[tauri::command]
pub async fn boot_run(app: AppHandle) -> CmdResult<BootReport> {
    tauri::async_runtime::spawn_blocking(move || boot::run(Some(app)))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn boot_logs_dir() -> String {
    boot::logs_dir().display().to_string()
}

/// Only the two files this app writes are readable here, and only the last 200 KB of each.
#[tauri::command]
pub fn boot_read_log(name: String) -> CmdResult<String> {
    let path = match name.as_str() {
        "boot.log" => boot::logs_dir().join("boot.log"),
        "crash.log" => boot::crash_log(),
        other => return Err(format!("Unknown log '{other}'")),
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let cut = text.len().saturating_sub(200 * 1024);
    Ok(text[cut..].to_string())
}

#[tauri::command]
pub fn boot_open_logs() -> CmdResult<String> {
    let dir = boot::logs_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(dir.display().to_string())
}

// ---------------------------------------------------------------------------------------------
// v0.4: projects, session branches and skills
// ---------------------------------------------------------------------------------------------

/// Failing workflow runs for whichever repository the given folder belongs to.
#[tauri::command]
pub async fn ci_badge(dir: Option<String>) -> CmdResult<u32> {
    blocking(move || -> anyhow::Result<u32> {
        let dir = dir
            .filter(|d| std::path::Path::new(d).is_dir())
            .unwrap_or_else(|| crate::settings::home().display().to_string());
        let view = match crate::github::repo_for_dir(&dir) {
            Ok(v) => v,
            Err(_) => return Ok(0),
        };
        let Some(repo) = view.get("nameWithOwner").and_then(|v| v.as_str()) else {
            return Ok(0);
        };
        let runs = crate::github::runs(repo, None)?;
        let arr = runs.as_array().cloned().unwrap_or_default();
        Ok(arr
            .iter()
            .filter(|r| r.get("conclusion").and_then(|v| v.as_str()) == Some("failure"))
            .count() as u32)
    })
    .await
}

#[tauri::command]
pub fn get_pref(key: String) -> CmdResult<Option<String>> {
    let c = crate::store::open().map_err(|e| e.to_string())?;
    crate::store::kv_get(&c, &key).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_pref(key: String, value: String) -> CmdResult<()> {
    let c = crate::store::open().map_err(|e| e.to_string())?;
    crate::store::kv_set(&c, &key, &value).map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn create_mission(
    repo: String,
    goal: String,
    acceptance: String,
    role: String,
    agent: String,
    provider: String,
    model: String,
    budget_usd: f64,
    agent_pack: Option<String>,
) -> CmdResult<crate::missions::Mission> {
    blocking(move || {
        let pack = agent_pack.filter(|p| !p.trim().is_empty());
        let (mut provider, mut model) = (provider, model);
        if let Some(id) = &pack {
            let (_, spec, _) = crate::agent_packs::load(id)?;
            if !spec.agents.iter().any(|a| a == &agent) {
                anyhow::bail!(
                    "agent pack {id} is written for {}, not {agent}",
                    spec.agents.join(" and ")
                );
            }
            // The pack's model policy decides when no model is pinned by hand; `agent-default`
            // keeps the chosen provider and its active model.
            if model.trim().is_empty() {
                let (_, p, m) =
                    crate::agent_packs::resolve_for_mission(&spec, &repo, &role, &agent);
                if !p.trim().is_empty() {
                    provider = p;
                    model = m;
                }
            }
        }
        // Explicit choice, then the repository's routing.json, then the router if it may pick.
        let m = crate::router::create_mission(
            &repo,
            &goal,
            &acceptance,
            &role,
            &agent,
            &provider,
            &model,
            budget_usd,
        )?;
        if let Some(id) = &pack {
            crate::agent_packs::set_for_mission(&m.id, id)?;
        }
        Ok(m)
    })
    .await
}

/// Installed agent packs with their ceilings on each agent they support.
#[tauri::command]
pub async fn agent_packs_installed() -> CmdResult<Vec<crate::agent_packs::InstalledAgent>> {
    blocking(crate::agent_packs::installed).await
}

#[tauri::command]
pub async fn mission_pins(
    ids: Vec<String>,
) -> CmdResult<std::collections::BTreeMap<String, crate::router::Pin>> {
    blocking(move || Ok(crate::router::pins(&ids))).await
}

#[tauri::command]
pub async fn router_auto() -> CmdResult<bool> {
    blocking(|| Ok(crate::router::auto_enabled())).await
}

#[tauri::command]
pub async fn set_router_auto(on: bool) -> CmdResult<()> {
    blocking(move || crate::router::set_auto(on)).await
}

#[tauri::command]
pub async fn list_missions(limit: Option<usize>) -> CmdResult<Vec<crate::missions::Mission>> {
    blocking(move || crate::missions::list(limit.unwrap_or(100))).await
}

/// Create the bus task that does the work, then mark the mission running.
#[tauri::command]
pub async fn dispatch_mission(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<crate::missions::Mission> {
    let v = current_vault(&state);
    blocking(move || crate::missions::start(&v, &id)).await
}

/// Ask the signed update channel whether a newer Studio exists.
#[tauri::command]
pub async fn studio_update_check(app: tauri::AppHandle) -> CmdResult<Option<String>> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let found = updater.check().await.map_err(|e| e.to_string())?;
    Ok(found.map(|u| u.version.clone()))
}

/// Download, verify the signature, install and restart. The signature is checked by the plugin
/// against the public key baked into the build, so a tampered asset is refused.
#[tauri::command]
pub async fn studio_update_install(app: tauri::AppHandle) -> CmdResult<String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Ok("up to date".into());
    };
    let version = update.version.clone();
    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("{version} installed, restart to use it"))
}

#[tauri::command]
pub async fn session_usage(session_id: String) -> CmdResult<Option<crate::usage::SessionUsage>> {
    blocking(move || crate::usage::usage(&session_id)).await
}

#[tauri::command]
pub async fn provider_credits() -> CmdResult<Vec<crate::usage::Credits>> {
    blocking(|| Ok(crate::usage::credits())).await
}

#[tauri::command]
pub async fn provider_connections() -> CmdResult<Vec<crate::connect::Connection>> {
    blocking(|| Ok(crate::connect::status())).await
}

#[tauri::command]
pub async fn save_provider_key(provider: String, key: String) -> CmdResult<String> {
    blocking(move || crate::connect::save_key(&provider, &key)).await
}

#[tauri::command]
pub fn tool_policies() -> Vec<crate::policy::ToolPolicy> {
    crate::policy::list()
}

#[tauri::command]
pub fn set_tool_policy(server: String, tool: String, policy: String) -> CmdResult<()> {
    crate::policy::set(&server, &tool, &policy).map_err(|e| e.to_string())
}

/// One read for the Safety tool policy UI: the hub tools, the agents and projects an override
/// can name, the global policy and every stored override.
#[tauri::command]
pub fn tool_policy_model() -> crate::permissions::ToolPolicyModel {
    let repos = crate::projects::list()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.archived)
        .map(|p| p.slug)
        .collect();
    crate::permissions::ToolPolicyModel {
        tools: hub_tools(),
        agents: crate::mcp_manager::hub_agents(),
        repos,
        global: crate::policy::list(),
        scoped: crate::permissions::scoped_list(),
    }
}

/// Set or clear (policy = None) one agent or repo tool override.
#[tauri::command]
pub fn set_scoped_tool_policy(
    scope: String,
    key: String,
    server: String,
    tool: String,
    policy: Option<String>,
) -> CmdResult<()> {
    crate::permissions::set_scoped(&scope, &key, &server, &tool, policy.as_deref())
        .map_err(|e| e.to_string())
}

/// The unattended worker profile: what it inherits, and which guarantees are live. Read only.
#[tauri::command]
pub fn unattended_profile() -> crate::permissions::UnattendedProfile {
    crate::permissions::unattended_profile()
}

/// The tools the hub exposes, so the policy UI lists exactly what exists.
#[tauri::command]
pub fn hub_tools() -> Vec<String> {
    crate::mcp_stdio::tool_definitions()
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Shadow mode: what the router would pick, with the evidence behind it. It changes nothing.
/// Whether the network is reachable and what can still run locally.
#[tauri::command]
pub async fn offline_status() -> CmdResult<crate::offline::OfflineStatus> {
    blocking(|| Ok(crate::offline::status())).await
}

/// The documentation that ships with the build, for the in-app help view.
#[tauri::command]
pub async fn help_docs() -> CmdResult<Vec<crate::help::Doc>> {
    blocking(crate::help::all).await
}

/// The committed routing policy for a repository, if it has one.
#[tauri::command]
pub fn routing_policy(repo: String) -> CmdResult<Option<serde_json::Value>> {
    let Some(p) = crate::review_agent::project_dir(&repo).and_then(|d| crate::routing::load(&d))
    else {
        return Ok(None);
    };
    serde_json::to_value(p).map(Some).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn router_suggestions() -> CmdResult<Vec<crate::router::Suggestion>> {
    blocking(crate::router::suggestions).await
}

#[tauri::command]
pub async fn router_verdicts() -> CmdResult<Vec<crate::router::Verdict>> {
    blocking(crate::router::verdicts).await
}

#[tauri::command]
pub fn model_catalog() -> crate::models::Catalog {
    crate::models::catalog()
}

#[tauri::command]
pub async fn notion_publish(
    state: State<'_, AppState>,
    dry_run: Option<bool>,
) -> CmdResult<crate::notion::PublishReport> {
    let v = current_vault(&state);
    blocking(move || Ok(crate::notion::publish(&v, dry_run.unwrap_or(false)))).await
}

#[tauri::command]
pub async fn notion_pull_inbox(state: State<'_, AppState>) -> CmdResult<usize> {
    let v = current_vault(&state);
    blocking(move || crate::notion::pull_inbox(&v)).await
}

#[tauri::command]
pub fn notion_status() -> serde_json::Value {
    serde_json::json!({
        "connected": crate::notion::token().is_some(),
        "has_parent": crate::notion::setting(crate::notion::KV_PARENT).is_some()
            || std::env::var("NOTION_PARENT_PAGE").is_ok(),
        "token_var": "NOTION_API_KEY",
        "missions_board": crate::notion::setting(crate::notion_sync::KV_DB).is_some(),
        "goals_board": crate::notion::setting(crate::notion_goals::KV_DB).is_some(),
        "ledger_out": crate::notion::ledger_counts().0,
        "ledger_in": crate::notion::ledger_counts().1,
    })
}

#[tauri::command]
pub async fn review_with_agent(
    state: State<'_, AppState>,
    id: String,
    reviewer: Option<String>,
) -> CmdResult<crate::missions::Mission> {
    let v = current_vault(&state);
    let who = reviewer.unwrap_or_else(|| "claude-code".into());
    blocking(move || crate::review_agent::review(&v, &id, &who)).await
}

/// Merge a mission's pull request only if every gate passes. Returns the merge output.
#[tauri::command]
pub async fn merge_mission(id: String, allowlisted: bool) -> CmdResult<String> {
    blocking(move || crate::review_agent::merge(&id, allowlisted)).await
}

#[tauri::command]
pub async fn automation_status() -> CmdResult<serde_json::Value> {
    blocking(|| Ok(automation_json())).await
}

fn automation_json() -> serde_json::Value {
    serde_json::json!({
        "enabled": crate::automation::enabled(),
        "max_concurrent": crate::automation::max_concurrent(),
        "ide_wait_minutes": crate::automation::ide_wait_minutes(),
    })
}

#[tauri::command]
pub async fn set_automation(
    enabled: bool,
    max_concurrent: Option<usize>,
    ide_wait_minutes: Option<i64>,
) -> CmdResult<serde_json::Value> {
    blocking(move || {
        crate::automation::set_enabled(enabled);
        if let Some(n) = max_concurrent {
            crate::automation::set_max_concurrent(n);
        }
        if let Some(n) = ide_wait_minutes {
            crate::automation::set_ide_wait_minutes(n);
        }
        Ok(automation_json())
    })
    .await
}

/// The capability fabric: every skill, plugin, agent, command and MCP server on this machine,
/// read only, with provenance, hashes, compatibility and duplicate groups.
#[tauri::command]
pub async fn catalog_index() -> CmdResult<crate::catalog::Index> {
    blocking(|| {
        let vault = crate::settings::vault_path();
        Ok(crate::catalog::index(
            &crate::catalog::default_roots(),
            Some(&vault),
            Some(&crate::catalog::cache_path()),
        ))
    })
    .await
}

/// Stage and check a capability pack (a local folder, or an https or file git URL). Returns the
/// permission sheet; nothing is activated and nothing from the pack runs.
#[tauri::command]
pub async fn capability_propose(
    source: String,
    expected_hash: Option<String>,
) -> CmdResult<crate::marketplace::Proposal> {
    blocking(move || {
        let store = crate::marketplace::Store::live();
        crate::marketplace::with_live_ctx(|ctx| {
            crate::marketplace::propose(&source, expected_hash.as_deref(), &store, ctx)
        })
    })
    .await
}

#[tauri::command]
pub async fn capability_activate(
    token: String,
    granted: Vec<String>,
    consent_new: bool,
) -> CmdResult<crate::marketplace::LockEntry> {
    blocking(move || {
        let store = crate::marketplace::Store::live();
        crate::marketplace::with_live_ctx(|ctx| {
            crate::marketplace::activate(&token, &granted, consent_new, &store, ctx)
        })
    })
    .await
}

#[tauri::command]
pub async fn capability_discard(token: String) -> CmdResult<()> {
    blocking(move || crate::marketplace::discard(&token, &crate::marketplace::Store::live())).await
}

#[tauri::command]
pub async fn capability_installed() -> CmdResult<Vec<crate::marketplace::Installed>> {
    blocking(|| crate::marketplace::list(&crate::marketplace::Store::live())).await
}

#[tauri::command]
pub async fn capability_rollback(id: String) -> CmdResult<crate::marketplace::LockEntry> {
    blocking(move || {
        let store = crate::marketplace::Store::live();
        crate::marketplace::with_live_ctx(|ctx| crate::marketplace::rollback(&id, &store, ctx))
    })
    .await
}

#[tauri::command]
pub async fn capability_uninstall(id: String) -> CmdResult<()> {
    blocking(move || {
        let store = crate::marketplace::Store::live();
        crate::marketplace::with_live_ctx(|ctx| crate::marketplace::uninstall(&id, &store, ctx))
    })
    .await
}

#[tauri::command]
pub async fn capability_export_lock(project: String) -> CmdResult<String> {
    blocking(move || {
        crate::marketplace::export_lock(
            std::path::Path::new(&project),
            &crate::marketplace::Store::live(),
        )
        .map(|p| p.display().to_string())
    })
    .await
}

#[tauri::command]
pub async fn capability_trusted_keys() -> CmdResult<Vec<crate::marketplace::TrustedKey>> {
    blocking(|| Ok(crate::marketplace::trusted_keys_stored())).await
}

#[tauri::command]
pub async fn capability_set_trusted_keys(
    keys: Vec<crate::marketplace::TrustedKey>,
) -> CmdResult<()> {
    blocking(move || {
        crate::marketplace::set_trusted_keys(&keys)?;
        let ids: Vec<&str> = keys.iter().map(|k| k.key_id.as_str()).collect();
        crate::permissions::audit(
            "capability.trusted_keys",
            "marketplace",
            &ids.join(", "),
            "ok",
        );
        Ok(())
    })
    .await
}

/// The skill bridge: what is bridged into Hermes and whether each entry still works.
#[tauri::command]
pub async fn bridge_status() -> CmdResult<Vec<crate::skill_bridge::Status>> {
    blocking(|| crate::skill_bridge::status(&crate::skill_bridge::Bridge::live())).await
}

#[tauri::command]
pub async fn bridge_check(path: String) -> CmdResult<crate::skill_bridge::Check> {
    blocking(move || {
        crate::skill_bridge::check(
            std::path::Path::new(&path),
            &crate::skill_bridge::Bridge::live(),
        )
    })
    .await
}

#[tauri::command]
pub async fn bridge_add(
    path: String,
    source: String,
    approved: Option<bool>,
) -> CmdResult<crate::skill_bridge::Entry> {
    blocking(move || {
        let summary = format!("Bridge {source} skill from {path} into Hermes skills-external");
        let approved =
            approved.unwrap_or(false) && std::env::var_os(crate::permissions::UNATTENDED).is_none();
        crate::permissions::run_guarded(
            crate::skill_bridge::ACTION,
            &path,
            &summary,
            approved,
            || {
                crate::skill_bridge::add(
                    std::path::Path::new(&path),
                    &source,
                    &crate::skill_bridge::Bridge::live(),
                )
            },
        )
    })
    .await
}

#[tauri::command]
pub async fn bridge_remove(
    folder: String,
    approved: Option<bool>,
) -> CmdResult<crate::skill_bridge::Entry> {
    blocking(move || {
        let summary = format!("Remove {folder} from Hermes skills-external bridge");
        let approved =
            approved.unwrap_or(false) && std::env::var_os(crate::permissions::UNATTENDED).is_none();
        crate::permissions::run_guarded(
            crate::skill_bridge::ACTION,
            &folder,
            &summary,
            approved,
            || crate::skill_bridge::remove(&folder, &crate::skill_bridge::Bridge::live()),
        )
    })
    .await
}

#[tauri::command]
pub async fn bridge_config() -> CmdResult<crate::skill_bridge::ConfigState> {
    blocking(|| crate::skill_bridge::config_state(&crate::skill_bridge::Bridge::live())).await
}

/// Add or remove the bridge folder in Hermes' config: guarded, audited, config backed up first.
#[tauri::command]
pub async fn bridge_set_config(
    enable: bool,
    approved: Option<bool>,
) -> CmdResult<crate::skill_bridge::ConfigState> {
    blocking(move || {
        crate::skill_bridge::set_config(
            &crate::skill_bridge::Bridge::live(),
            enable,
            approved.unwrap_or(false),
        )
    })
    .await
}

/// The global metered budget: limit, period, spend this period and whether the stop is on.
#[tauri::command]
pub async fn budget_status() -> CmdResult<crate::budget::Status> {
    blocking(|| Ok(crate::budget::status())).await
}

#[tauri::command]
pub async fn set_budget(limit_usd: f64, period: String) -> CmdResult<crate::budget::Status> {
    blocking(move || crate::budget::set(limit_usd, &period)).await
}

#[tauri::command]
pub async fn automation_tick(
    state: State<'_, AppState>,
) -> CmdResult<crate::automation::TickReport> {
    let v = current_vault(&state);
    blocking(move || Ok(crate::automation::tick(&v))).await
}

#[tauri::command]
pub async fn submit_mission(id: String, spent_usd: f64) -> CmdResult<crate::missions::Mission> {
    blocking(move || crate::missions::submit_for_review(&id, spent_usd)).await
}

#[tauri::command]
pub async fn review_mission(
    id: String,
    reviewer: String,
    verdict: String,
    note: String,
) -> CmdResult<crate::missions::Mission> {
    blocking(move || crate::missions::review(&id, &reviewer, &verdict, &note)).await
}

#[tauri::command]
pub fn mission_merge_blockers(
    id: String,
    ci_green: bool,
    allowlisted: bool,
) -> CmdResult<Vec<String>> {
    let m = crate::missions::get(&id).map_err(err)?;
    match m {
        Some(m) => Ok(crate::missions::merge_blockers(&m, ci_green, allowlisted)),
        None => Err(format!("no mission '{id}'")),
    }
}

#[tauri::command]
pub async fn memory_ledger(
    session_id: String,
    limit: Option<usize>,
) -> CmdResult<Vec<crate::memory_ledger::Entry>> {
    blocking(move || crate::memory_ledger::list(&session_id, limit.unwrap_or(400))).await
}

#[tauri::command]
pub async fn memory_recent(limit: Option<usize>) -> CmdResult<Vec<crate::memory_ledger::Entry>> {
    blocking(move || crate::memory_ledger::recent(limit.unwrap_or(50))).await
}

#[tauri::command]
pub async fn memory_notes(
    state: tauri::State<'_, AppState>,
) -> CmdResult<Vec<crate::memory_index::NoteRow>> {
    let v = current_vault(&state);
    blocking(move || {
        let _ = crate::memory_index::refresh(&v);
        crate::memory_index::notes(&v)
    })
    .await
}

/// Turn a finished mission's review into memory notes, then check its repository for repeated
/// rejection reasons. Writes are additive; rerunning a mission writes nothing.
#[tauri::command]
pub async fn extract_lessons(
    state: tauri::State<'_, AppState>,
    id: String,
) -> CmdResult<crate::learning::LearningReport> {
    let v = current_vault(&state);
    blocking(move || crate::learning::extract_lessons(&v, &id)).await
}

/// The last memory quality run, recorded by the app or by the weekly headless job.
#[tauri::command]
pub async fn memory_quality_last() -> CmdResult<Option<crate::memory_quality::MemoryQualityReport>>
{
    blocking(|| Ok(crate::memory_quality::last_report())).await
}

/// Check memory notes for stale references and contradictions. A dry run reports and writes
/// nothing.
#[tauri::command]
pub async fn memory_quality_run(
    state: tauri::State<'_, AppState>,
    dry_run: Option<bool>,
) -> CmdResult<crate::memory_quality::MemoryQualityReport> {
    let v = current_vault(&state);
    blocking(move || crate::memory_quality::run(&v, dry_run.unwrap_or(false))).await
}

/// Schedule the weekly memory quality check as a no-agent Hermes cron job.
#[tauri::command]
pub async fn memory_quality_schedule(
    state: tauri::State<'_, AppState>,
    approved: Option<bool>,
) -> CmdResult<String> {
    let v = current_vault(&state);
    blocking(move || {
        let exe = std::env::current_exe()
            .map_err(|e| anyhow::anyhow!("cannot find the Studio binary: {e}"))?;
        crate::permissions::run_guarded(
            "hermes.cron.change",
            crate::memory_quality::WEEKLY_JOB,
            &format!(
                "hermes cron create \"{}\" --name {} --no-agent --script <hermes scripts>/{}",
                crate::memory_quality::WEEKLY_SCHEDULE,
                crate::memory_quality::WEEKLY_JOB,
                crate::memory_quality::WEEKLY_JOB
            ),
            approved.unwrap_or(false),
            || crate::memory_quality::schedule(&v, &exe),
        )
    })
    .await
}

#[tauri::command]
pub fn working_dir_default() -> crate::workdir::WorkingDir {
    crate::workdir::resolve()
}

#[tauri::command]
pub async fn set_working_dir(path: String) -> CmdResult<String> {
    blocking(move || crate::workdir::remember(&path)).await
}

#[tauri::command]
pub async fn list_projects() -> CmdResult<Vec<Project>> {
    blocking(crate::projects::list).await
}

#[tauri::command]
pub async fn project_sessions(slug: String) -> CmdResult<Vec<ProjectSession>> {
    blocking(move || crate::projects::sessions(&slug)).await
}

#[tauri::command]
pub async fn create_project(name: String, folders: Vec<String>) -> CmdResult<String> {
    blocking(move || crate::projects::create(&name, &folders)).await
}

#[tauri::command]
pub async fn rename_project(slug: String, name: String) -> CmdResult<String> {
    blocking(move || crate::projects::rename(&slug, &name)).await
}

#[tauri::command]
pub async fn archive_project(slug: String, archived: bool) -> CmdResult<String> {
    blocking(move || crate::projects::set_archived(&slug, archived)).await
}

#[tauri::command]
pub async fn add_project_folder(slug: String, path: String) -> CmdResult<String> {
    blocking(move || crate::projects::add_folder(&slug, &path)).await
}

#[tauri::command]
pub async fn link_branch(
    parent: String,
    project_slug: Option<String>,
    title: String,
    child: String,
) -> CmdResult<()> {
    blocking(move || crate::projects::link_branch(&parent, project_slug.as_deref(), &title, &child))
        .await
}

/// Branch a Hermes chat at one message into a new Hermes session (imported through Hermes).
#[tauri::command]
pub async fn fork_session(
    session_id: String,
    at_message_id: i64,
) -> CmdResult<crate::fork::ForkResult> {
    blocking(move || crate::fork::fork(&session_id, at_message_id)).await
}

#[tauri::command]
pub async fn get_chat_skills(session_id: String) -> CmdResult<Vec<String>> {
    blocking(move || Ok(crate::projects::chat_skills(&session_id))).await
}

#[tauri::command]
pub async fn set_chat_skills(session_id: String, skills: Vec<String>) -> CmdResult<()> {
    blocking(move || crate::projects::set_chat_skills(&session_id, &skills)).await
}

#[tauri::command]
pub async fn list_skills() -> CmdResult<Vec<Skill>> {
    blocking(|| Ok(crate::skills::list())).await
}

#[tauri::command]
pub async fn read_skill(name: String) -> CmdResult<String> {
    blocking(move || crate::skills::read(&name)).await
}

#[tauri::command]
pub async fn search_skills(
    query: String,
    limit: Option<usize>,
) -> CmdResult<Vec<crate::skills::HubSkill>> {
    blocking(move || crate::skills::search_hub(&query, limit.unwrap_or(15))).await
}

#[tauri::command]
pub async fn install_skill(identifier: String) -> CmdResult<String> {
    blocking(move || crate::skills::install(&identifier)).await
}

#[tauri::command]
pub async fn remove_skill(name: String) -> CmdResult<String> {
    blocking(move || crate::skills::remove(&name)).await
}

#[tauri::command]
pub async fn update_skills() -> CmdResult<String> {
    blocking(crate::skills::update_all).await
}

/// Two-way mission sync with the Notion missions board.
#[tauri::command]
pub async fn notion_sync_missions(
    dry_run: Option<bool>,
) -> CmdResult<crate::notion_sync::SyncReport> {
    blocking(move || Ok(crate::notion_sync::sync(dry_run.unwrap_or(false)))).await
}

#[tauri::command]
pub async fn notion_archive_board() -> CmdResult<()> {
    blocking(crate::notion_sync::archive_board).await
}

/// Release a blocked mission to the queue; for a Notion proposal, optionally trust its author.
#[tauri::command]
pub async fn approve_mission(
    id: String,
    trust_author: Option<bool>,
) -> CmdResult<crate::missions::Mission> {
    blocking(move || crate::notion_sync::approve(&id, trust_author.unwrap_or(false))).await
}

#[tauri::command]
pub async fn mission_notion_authors(
    ids: Vec<String>,
) -> CmdResult<std::collections::BTreeMap<String, String>> {
    blocking(move || {
        Ok(ids
            .into_iter()
            .filter_map(|id| crate::notion_sync::proposal_author(&id).map(|a| (id, a)))
            .collect())
    })
    .await
}

#[tauri::command]
pub async fn notion_trusted_authors() -> CmdResult<Vec<String>> {
    blocking(|| Ok(crate::notion_sync::trusted_authors())).await
}

#[tauri::command]
pub async fn set_notion_trusted_authors(ids: Vec<String>) -> CmdResult<Vec<String>> {
    blocking(move || crate::notion_sync::set_trusted_authors(&ids)).await
}

/// Two-way goal sync with the Notion goals board: rows out per goal, proposals and Decision
/// answers to open escalations in.
#[tauri::command]
pub async fn notion_sync_goals(
    state: State<'_, AppState>,
    dry_run: Option<bool>,
) -> CmdResult<crate::notion_goals::SyncReport> {
    let v = current_vault(&state);
    blocking(move || {
        let r = crate::notion_goals::sync(&v, dry_run.unwrap_or(false));
        // A Decision takes effect at once, like an answer from the Goals screen.
        if !r.decided.is_empty() && !r.dry_run {
            let _ = crate::automation::tick(&v);
        }
        Ok(r)
    })
    .await
}

#[tauri::command]
pub async fn notion_archive_goals_board() -> CmdResult<()> {
    blocking(crate::notion_goals::archive_board).await
}

// ------------------------------------------------------------------ Phase C: agents talk

/// The agent registry: one card per agent, the same data the `agents` hub tool returns.
#[tauri::command]
pub async fn agent_cards(refresh: Option<bool>) -> CmdResult<Vec<crate::agents::AgentCard>> {
    blocking(move || Ok(crate::agents::cards(refresh.unwrap_or(false)))).await
}

#[tauri::command]
pub async fn set_agent_strengths(agent: String, strengths: Vec<String>) -> CmdResult<()> {
    blocking(move || crate::agents::set_strengths(&agent, &strengths)).await
}

/// Exchanges between agents: one mission's, or the newest across all missions.
#[tauri::command]
pub async fn agent_events(
    mission_id: Option<String>,
    limit: Option<usize>,
) -> CmdResult<Vec<crate::agent_log::Event>> {
    blocking(move || match mission_id.filter(|m| !m.trim().is_empty()) {
        Some(m) => crate::agent_log::for_mission(&m),
        None => crate::agent_log::recent(limit.unwrap_or(200)),
    })
    .await
}

/// Each mission's kind: `work`, or `decision` for a council.
#[tauri::command]
pub async fn mission_kinds(
    ids: Vec<String>,
) -> CmdResult<std::collections::BTreeMap<String, String>> {
    blocking(move || {
        Ok(ids
            .into_iter()
            .map(|id| {
                let k = crate::missions::kind(&id);
                (id, k)
            })
            .collect())
    })
    .await
}

#[tauri::command]
pub async fn set_mission_kind(id: String, kind: String) -> CmdResult<()> {
    blocking(move || crate::missions::set_kind(&id, &kind)).await
}

/// Urgent missions may spend metered money when the flat subscriptions are out of headroom.
#[tauri::command]
pub async fn set_mission_urgent(id: String, urgent: bool) -> CmdResult<()> {
    blocking(move || crate::assign::set_urgent(&id, urgent)).await
}

/// One mission's run timeline across agents, from creation to merge.
#[tauri::command]
pub async fn mission_timeline(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<crate::timeline::Timeline> {
    let v = current_vault(&state);
    blocking(move || crate::timeline::mission(&v, &id)).await
}

/// Missions for the timeline picker, with the agents and exchanges each involved.
#[tauri::command]
pub async fn timeline_missions(limit: Option<usize>) -> CmdResult<Vec<crate::timeline::Summary>> {
    blocking(move || crate::timeline::summaries(limit.unwrap_or(100))).await
}

/// Skill proposals from approved missions that repeat one role and one task class.
#[tauri::command]
pub async fn skill_proposals(
    state: State<'_, AppState>,
) -> CmdResult<Vec<crate::skill_proposals::Proposal>> {
    let v = current_vault(&state);
    blocking(move || crate::skill_proposals::proposals(&v)).await
}

/// Create one proposed skill through a Hermes turn. The owner approves every proposal.
#[tauri::command]
pub async fn skill_proposal_approve(
    state: State<'_, AppState>,
    hash: String,
    approved: Option<bool>,
) -> CmdResult<String> {
    let v = current_vault(&state);
    blocking(move || {
        let p = crate::skill_proposals::proposals(&v)?
            .into_iter()
            .find(|p| p.hash == hash)
            .ok_or_else(|| anyhow::anyhow!("no skill proposal {hash}"))?;
        crate::permissions::run_guarded(
            "hermes.skills.create",
            &p.name,
            &format!(
                "Create the Hermes skill '{}' from a Studio draft (one Hermes turn)",
                p.name
            ),
            approved.unwrap_or(false),
            || crate::skill_proposals::approve(&p, None),
        )
    })
    .await
}

/// Replay a finished mission on another agent or model in a throwaway worktree, judged and scored.
#[tauri::command]
pub async fn eval_run(
    state: State<'_, AppState>,
    mission: String,
    agent: String,
    model: Option<String>,
    approved: Option<bool>,
) -> CmdResult<crate::eval::EvalResult> {
    let v = current_vault(&state);
    blocking(move || {
        let target = format!("{mission} on {agent}");
        let summary = format!(
            "Replay mission {mission} on {agent} in a detached worktree, run its tests and review the result. It never pushes and never opens a pull request."
        );
        crate::permissions::run_guarded(
            "eval.run",
            &target,
            &summary,
            approved.unwrap_or(false),
            || crate::eval::run(&v, &mission, &agent, model.as_deref()),
        )
    })
    .await
}

/// Dismiss a proposal so the same group of missions is not proposed again.
#[tauri::command]
pub async fn skill_proposal_dismiss(hash: String) -> CmdResult<()> {
    blocking(move || crate::skill_proposals::dismiss(&hash)).await
}

/// Why is this line here? The commit, its mission trailers and the mission behind it.
#[tauri::command]
pub async fn file_provenance(
    repo: String,
    file: String,
    line: u64,
) -> CmdResult<crate::provenance::Provenance> {
    blocking(move || crate::provenance::why(&repo, &file, line)).await
}

/// Every replay note in the vault, newest first.
#[tauri::command]
pub async fn eval_list(state: State<'_, AppState>) -> CmdResult<Vec<crate::eval::EvalSummary>> {
    let v = current_vault(&state);
    blocking(move || crate::eval::list(&v)).await
}

/// A goal with the missions it created, for the Goals view.
#[derive(serde::Serialize)]
pub struct GoalDetail {
    pub goal: crate::goals::Goal,
    pub missions: Vec<crate::missions::Mission>,
    pub spent_usd: f64,
}

fn goal_detail_of(id: &str) -> anyhow::Result<GoalDetail> {
    let goal = crate::goals::get(id)?.ok_or_else(|| anyhow::anyhow!("no goal '{id}'"))?;
    let missions = crate::goals::missions_of(id)?;
    let spent_usd = crate::goals::spent(&missions);
    Ok(GoalDetail {
        goal,
        missions,
        spent_usd,
    })
}

#[tauri::command]
pub async fn goal_create(
    state: State<'_, AppState>,
    goal: crate::goals::NewGoal,
) -> CmdResult<crate::goals::Goal> {
    let v = current_vault(&state);
    blocking(move || {
        let g = crate::goals::create(&goal)?;
        if let Err(e) = crate::goals::write_note(&v, &g.id) {
            eprintln!("goals: vault note for {} failed: {e:#}", g.id);
        }
        // Planning takes about half a minute; the view polls for the result.
        crate::planner::plan_in_background(v.root.clone(), g.id.clone());
        Ok(g)
    })
    .await
}

#[tauri::command]
pub async fn goals_list(limit: Option<usize>) -> CmdResult<Vec<GoalDetail>> {
    blocking(move || {
        let mut out = Vec::new();
        for g in crate::goals::list(limit.unwrap_or(100))? {
            out.push(goal_detail_of(&g.id)?);
        }
        Ok(out)
    })
    .await
}

#[tauri::command]
pub async fn goal_detail(id: String) -> CmdResult<GoalDetail> {
    blocking(move || goal_detail_of(&id)).await
}

#[tauri::command]
pub async fn goal_set_autonomy(
    state: State<'_, AppState>,
    id: String,
    level: String,
) -> CmdResult<crate::goals::Goal> {
    let v = current_vault(&state);
    blocking(move || {
        let g = crate::goals::set_autonomy(&id, &level)?;
        let _ = crate::goals::write_note(&v, &id);
        Ok(g)
    })
    .await
}

/// Delete a goal with nothing in flight, and its missions. Its vault note is kept as the record.
#[tauri::command]
pub async fn goal_delete(id: String) -> CmdResult<()> {
    blocking(move || crate::goals::delete(&id)).await
}

/// Each repository's default autonomy level.
#[tauri::command]
pub async fn autonomy_defaults(
    repos: Vec<String>,
) -> CmdResult<std::collections::BTreeMap<String, String>> {
    blocking(move || {
        Ok(repos
            .into_iter()
            .map(|r| {
                let l = crate::goals::repo_default(&r);
                (r, l)
            })
            .collect())
    })
    .await
}

#[tauri::command]
pub async fn set_repo_autonomy(repo: String, level: String) -> CmdResult<()> {
    blocking(move || crate::goals::set_repo_default(&repo, &level)).await
}

/// Plan a goal again after the planner failed (it stays in planning with the reason).
#[tauri::command]
pub async fn goal_plan(state: State<'_, AppState>, id: String) -> CmdResult<crate::goals::Goal> {
    let v = current_vault(&state);
    blocking(move || crate::planner::plan(&v, &id)).await
}

/// Watch: the owner read the plan and lets the goal run; each mission still waits for Start.
#[tauri::command]
pub async fn goal_approve_plan(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<crate::goals::Goal> {
    let v = current_vault(&state);
    blocking(move || {
        let g = crate::planner::approve(&id)?;
        let _ = crate::goals::write_note(&v, &id);
        Ok(g)
    })
    .await
}

/// Open escalations first: the questions waiting on the owner.
#[tauri::command]
pub async fn escalations_list(
    limit: Option<usize>,
) -> CmdResult<Vec<crate::escalation::Escalation>> {
    blocking(move || crate::escalation::list(limit.unwrap_or(100))).await
}

/// Answer an escalation from Studio. The scheduler applies it on its next pass; this runs one
/// pass now so the answer takes effect at once.
#[tauri::command]
pub async fn escalation_answer(
    state: State<'_, AppState>,
    id: String,
    answer: String,
) -> CmdResult<crate::escalation::Escalation> {
    let v = current_vault(&state);
    blocking(move || {
        let e = crate::escalation::answer(&id, &answer, "studio")?;
        let _ = crate::automation::tick(&v);
        Ok(crate::escalation::get(&id)?.unwrap_or(e))
    })
    .await
}

#[tauri::command]
pub fn escalation_settings() -> serde_json::Value {
    serde_json::json!({
        "timeout_minutes": crate::escalation::timeout_minutes(),
        "notify_target": crate::hermes_surfaces::notify_target(),
    })
}

#[tauri::command]
pub async fn set_escalation_timeout(minutes: i64) -> CmdResult<()> {
    blocking(move || crate::escalation::set_timeout_minutes(minutes)).await
}

/// Write, publish and send a digest now. Returns the vault path of the note.
#[tauri::command]
pub async fn digest_now(state: State<'_, AppState>) -> CmdResult<String> {
    let v = current_vault(&state);
    blocking(move || crate::digest::send_now(&v)).await
}

#[tauri::command]
pub fn digest_settings() -> serde_json::Value {
    let times: Vec<String> = crate::digest::times()
        .iter()
        .map(|t| t.format("%H:%M").to_string())
        .collect();
    serde_json::json!({
        "times": times.join(","),
        "notify_target": crate::hermes_surfaces::notify_target(),
    })
}

#[tauri::command]
pub async fn set_digest_times(times: String) -> CmdResult<()> {
    blocking(move || crate::digest::set_times(&times)).await
}

#[tauri::command]
pub fn fleet_pause_state() -> crate::pause::State {
    crate::pause::state()
}

#[tauri::command]
pub async fn fleet_pause(reason: String) -> CmdResult<crate::pause::State> {
    blocking(move || crate::pause::pause(&reason)).await
}

#[tauri::command]
pub async fn fleet_resume() -> CmdResult<crate::pause::State> {
    blocking(crate::pause::resume).await
}

/// The newest digest note exactly as written, so Today can show the last one. Read only.
#[tauri::command]
pub async fn digest_latest(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    let v = current_vault(&state);
    blocking(move || Ok(crate::digest::latest(&v))).await
}

// ---------------------------------------------------------------------------
// Context ledger (Phase G 10)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn context_events(limit: Option<usize>) -> CmdResult<Vec<crate::context_ledger::Event>> {
    blocking(move || {
        let c = crate::store::open()?;
        crate::context_ledger::list(&c, limit.unwrap_or(200))
    })
    .await
}

#[tauri::command]
pub async fn context_scan(
    state: State<'_, AppState>,
) -> CmdResult<crate::context_ledger::ScanReport> {
    let v = current_vault(&state);
    blocking(move || {
        let c = crate::store::open()?;
        Ok(crate::context_ledger::scan(&v.root, &c))
    })
    .await
}

#[tauri::command]
pub async fn context_rerun(
    state: State<'_, AppState>,
    id: i64,
    focus: String,
) -> CmdResult<String> {
    let v = current_vault(&state);
    blocking(move || {
        let c = crate::store::open()?;
        crate::context_ledger::rerun(&v.root, &c, id, &focus)
    })
    .await
}

#[tauri::command]
pub async fn context_search(
    state: State<'_, AppState>,
    query: String,
    limit: Option<usize>,
) -> CmdResult<Vec<crate::context_ledger::Hit>> {
    let v = current_vault(&state);
    blocking(move || crate::context_ledger::search(&v, &query, limit.unwrap_or(20))).await
}

#[tauri::command]
pub async fn context_eval(
    min_messages: Option<usize>,
    bar: Option<f64>,
) -> CmdResult<crate::context_ledger::EvalReport> {
    blocking(move || {
        crate::context_ledger::evaluate(min_messages.unwrap_or(12), bar.unwrap_or(0.8))
    })
    .await
}

#[tauri::command]
pub async fn extension_run(
    req: crate::extension_runtime::RunRequest,
    approved: Option<bool>,
) -> CmdResult<crate::extension_runtime::RunRecord> {
    blocking(move || crate::extension_runtime::run(&req, approved.unwrap_or(false))).await
}

#[tauri::command]
pub async fn extension_register_mcp(
    id: String,
    targets: Vec<String>,
    approved: Option<bool>,
) -> CmdResult<crate::extension_runtime::McpRegistration> {
    blocking(move || {
        crate::extension_runtime::register_mcp(&id, &targets, approved.unwrap_or(false))
    })
    .await
}

#[tauri::command]
pub async fn extension_list_runs(
    limit: Option<usize>,
) -> CmdResult<Vec<crate::permissions::AuditEntry>> {
    blocking(move || crate::extension_runtime::list_runs(limit.unwrap_or(50))).await
}
