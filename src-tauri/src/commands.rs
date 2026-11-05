//! Tauri commands: the only surface the React UI talks to.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, State};

use crate::bus::{self, BusState, WorkerInfo};
use crate::constitution;
use crate::settings::Settings;
use crate::vault::{self, Board, NoteInfo, SearchHit, Vault};
use crate::{bootstrapper, broadcast, fleet};

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub bus: BusState,
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
) -> CmdResult<Value> {
    let v = current_vault(&state);
    let workers = state.bus.workers.clone();
    bus::quick_mission_dispatch(app, workers, v, &prompt, project_path, &target_agent).map_err(err)
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
