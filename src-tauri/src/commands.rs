//! Tauri commands: the only surface the React UI talks to.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

use crate::bus::{self, BusState, WorkerInfo};
use crate::chat_stream::{self, ApplyResult, ChatRequest, ChatStarted, ChatState};
use crate::cli_runner::{self, CliState};
use crate::constitution;
use crate::hermes_engine::{
    self, ChatMessage, HermesState, HermesStatus, ProviderConfig, ProviderTest, SessionSummary,
};
use crate::mcp_manager::{self, AgentOutcome, CatalogEntry, McpServer, ProbeResult, ServerSpec};
use crate::settings::Settings;
use crate::vault::{self, Board, NoteInfo, SearchHit, Vault};
use crate::{bootstrapper, broadcast, fleet};

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub bus: BusState,
    pub hermes: Arc<HermesState>,
    pub chat: ChatState,
    pub cli: CliState,
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
pub async fn delete_session(session_id: String) -> CmdResult<()> {
    blocking(move || hermes_engine::delete_session(&session_id)).await
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
pub async fn send_chat_message(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: Option<String>,
    prompt: String,
    working_dir: String,
    agent_backend: String,
    model: Option<String>,
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
