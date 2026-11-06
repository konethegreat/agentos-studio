//! AgentOS Studio: native control plane (Tauri GUI) and native MCP server (`--mcp-stdio`).

pub mod bootstrapper;
pub mod broadcast;
pub mod bus;
pub mod chat_stream;
pub mod cli_runner;
pub mod commands;
pub mod constitution;
pub mod fleet;
pub mod hermes_engine;
pub mod mcp_manager;
pub mod mcp_stdio;
pub mod procs;
pub mod settings;
pub mod vault;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Emitter, Manager};

pub fn run_gui() {
    // GUI processes inherit a stale PATH; heal it once so every spawn finds the CLIs.
    let healed = procs::heal_process_path();
    let hermes = Arc::new(hermes_engine::HermesState::default());
    if let Ok(mut h) = hermes.path_healed.lock() {
        *h = healed;
    }
    let state = commands::AppState {
        settings: Mutex::new(settings::Settings::load()),
        bus: bus::BusState::default(),
        hermes: hermes.clone(),
        chat: chat_stream::ChatState::default(),
        cli: cli_runner::CliState::default(),
    };
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .setup(move |app| {
            let handle = app.handle().clone();
            let st = app.state::<commands::AppState>();
            let root = st
                .settings
                .lock()
                .map(|s| s.vault_path())
                .unwrap_or_default();
            let v = vault::Vault::new(&root);
            if v.structure_ok().0 {
                if let Err(e) = bus::start_watcher(handle.clone(), &st.bus, &root) {
                    eprintln!("vault watcher not started: {e:#}");
                }
            }
            // Heartbeat: Hermes version, provider, model and MCP count every 15 seconds.
            let beat = hermes.clone();
            std::thread::spawn(move || loop {
                let status = beat.status(false);
                if handle.emit(hermes_engine::EV_STATUS, status).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_secs(15));
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::check_full_stack_status,
            commands::install_or_repair_component,
            commands::scaffold_vault,
            commands::list_board,
            commands::read_task,
            commands::quick_mission_dispatch,
            commands::run_task_now,
            commands::move_task,
            commands::stop_worker,
            commands::list_workers,
            commands::scan_fleet,
            commands::sync_fleet_rules,
            commands::create_new_project_from_ui,
            commands::load_constitution,
            commands::toggle_law,
            commands::save_and_broadcast,
            commands::list_memory_notes,
            commands::read_note,
            commands::search_memory,
            commands::get_hermes_status,
            commands::get_provider_config,
            commands::set_active_provider,
            commands::list_available_models,
            commands::test_provider,
            commands::login_provider,
            commands::list_hermes_sessions,
            commands::load_session_messages,
            commands::rename_session,
            commands::delete_session,
            commands::export_session_to_obsidian,
            commands::get_all_mcp_servers,
            commands::add_mcp_server,
            commands::remove_mcp_server,
            commands::test_mcp_server,
            commands::trigger_mcp_oauth,
            commands::set_mcp_enabled,
            commands::sync_mcp_server,
            commands::cutover_agentos_hub,
            commands::mcp_catalog,
            commands::install_catalog_server,
            commands::gh_token,
            commands::send_chat_message,
            commands::stop_active_chat,
            commands::active_chat,
            commands::apply_code_to_file,
            commands::execute_cli_command,
            commands::kill_cli_command,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AgentOS Studio");
}
