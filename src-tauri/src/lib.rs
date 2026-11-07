//! AgentOS Studio: native control plane (Tauri GUI) and native MCP server (`--mcp-stdio`).

pub mod automation;
pub mod boot;
pub mod bootstrapper;
pub mod broadcast;
pub mod bus;
pub mod chat_stream;
pub mod cli_runner;
pub mod commands;
pub mod constitution;
pub mod fleet;
pub mod github;
pub mod hermes_engine;
pub mod mcp_manager;
pub mod mcp_stdio;
pub mod memory_index;
pub mod memory_ledger;
pub mod missions;
pub mod models;
pub mod notion;
pub mod ops;
pub mod org;
pub mod permissions;
pub mod policy;
pub mod procs;
pub mod projects;
pub mod pty;
pub mod review_agent;
pub mod settings;
pub mod single;
pub mod skills;
pub mod store;
pub mod updates;
pub mod vault;
pub mod workdir;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Emitter, Manager};

pub fn run_gui() {
    boot::install_panic_hook();
    // A second GUI instance would double the heartbeat, the watcher and every task thread.
    if single::already_running() {
        return;
    }
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
        pty: pty::PtyState::default(),
    };
    let pty_for_exit = state.pty.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(state)
        .setup(move |app| {
            let handle = app.handle().clone();
            if let Err(e) = single::claim(handle.clone()) {
                eprintln!("single instance guard not claimed: {e:#}");
            }
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
            // Missions tick once a minute while the app is open. Off by default; the Missions
            // board turns it on, and it advances running work either way.
            automation::start_scheduler(root.clone());
            // Staged startup checks: streamed to the boot screen, cached for the next launch.
            let boot_handle = handle.clone();
            std::thread::spawn(move || {
                boot::run(Some(boot_handle));
            });
            // Update checks: shortly after start, then every 6 hours. New versions raise an
            // in-app event and, once per version, an OS notification.
            let upd = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(20));
                loop {
                    let report = updates::check_all();
                    for p in updates::take_notifications(&report) {
                        use tauri_plugin_notification::NotificationExt;
                        let _ = upd
                            .notification()
                            .builder()
                            .title(format!("{} update available", p.label))
                            .body(updates::notification_body(&p))
                            .show();
                    }
                    if upd.emit(updates::EV_UPDATES, &report).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(6 * 3600));
                }
            });
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
            ops::pty_spawn,
            ops::pty_write,
            ops::pty_resize,
            ops::pty_kill,
            ops::pty_list,
            ops::pty_snapshot,
            ops::check_updates,
            ops::last_update_report,
            ops::apply_update,
            ops::skip_update,
            ops::snooze_update,
            ops::clear_update_choice,
            ops::list_update_backups,
            ops::rollback_update,
            ops::org_snapshot,
            ops::org_create_workspace,
            ops::org_update_workspace,
            ops::org_delete_workspace,
            ops::org_create_folder,
            ops::org_update_folder,
            ops::org_delete_folder,
            ops::org_place_chat,
            ops::org_set_chat_flags,
            ops::org_set_tags,
            ops::org_add_attachment,
            ops::org_remove_attachment,
            ops::org_context_files,
            ops::org_link_project,
            ops::gh_auth_status,
            ops::gh_login,
            ops::gh_repos,
            ops::gh_repo_for_dir,
            ops::gh_branches,
            ops::gh_commits,
            ops::gh_prs,
            ops::gh_pr_detail,
            ops::gh_pr_diff,
            ops::gh_pr_merge,
            ops::gh_pr_review,
            ops::gh_pr_create,
            ops::gh_issues,
            ops::gh_issue_detail,
            ops::gh_issue_create,
            ops::gh_comment,
            ops::gh_issue_close,
            ops::gh_runs,
            ops::gh_run_detail,
            ops::gh_run_failed_log,
            ops::gh_run_rerun,
            ops::list_policies,
            ops::set_policy,
            ops::list_audit,
            ops::open_external,
            ops::reveal_path,
            commands::boot_snapshot,
            commands::boot_run,
            commands::boot_logs_dir,
            commands::boot_read_log,
            commands::boot_open_logs,
            commands::list_projects,
            commands::project_sessions,
            commands::create_project,
            commands::rename_project,
            commands::archive_project,
            commands::add_project_folder,
            commands::link_branch,
            commands::get_chat_skills,
            commands::set_chat_skills,
            commands::list_skills,
            commands::read_skill,
            commands::search_skills,
            commands::install_skill,
            commands::remove_skill,
            commands::update_skills,
            commands::working_dir_default,
            commands::set_working_dir,
            commands::get_pref,
            commands::set_pref,
            commands::ci_badge,
            commands::memory_ledger,
            commands::memory_recent,
            commands::memory_notes,
            commands::create_mission,
            commands::list_missions,
            commands::dispatch_mission,
            commands::submit_mission,
            commands::review_mission,
            commands::mission_merge_blockers,
            commands::review_with_agent,
            commands::merge_mission,
            commands::model_catalog,
            commands::tool_policies,
            commands::set_tool_policy,
            commands::hub_tools,
            commands::notion_publish,
            commands::notion_pull_inbox,
            commands::notion_status,
            commands::automation_status,
            commands::set_automation,
            commands::automation_tick,
        ])
        .build(tauri::generate_context!())
        .expect("error while building AgentOS Studio")
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                pty::kill_all(&pty_for_exit);
            }
        });
}
