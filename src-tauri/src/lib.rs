//! AgentOS Studio: native control plane (Tauri GUI) and native MCP server (`--mcp-stdio`).

pub mod bootstrapper;
pub mod broadcast;
pub mod bus;
pub mod commands;
pub mod constitution;
pub mod fleet;
pub mod mcp_stdio;
pub mod procs;
pub mod settings;
pub mod vault;

use std::sync::Mutex;

use tauri::Manager;

pub fn run_gui() {
    let state = commands::AppState {
        settings: Mutex::new(settings::Settings::load()),
        bus: bus::BusState::default(),
    };
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .setup(|app| {
            let handle = app.handle().clone();
            let st = app.state::<commands::AppState>();
            let root = st
                .settings
                .lock()
                .map(|s| s.vault_path())
                .unwrap_or_default();
            let v = vault::Vault::new(&root);
            if v.structure_ok().0 {
                if let Err(e) = bus::start_watcher(handle, &st.bus, &root) {
                    eprintln!("vault watcher not started: {e:#}");
                }
            }
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running AgentOS Studio");
}
