//! AgentOS Studio: native control plane (Tauri GUI) and native MCP server (`--mcp-stdio`).

pub mod acp;
pub mod acp_host;
pub mod agent_log;
pub mod agents;
pub mod ask;
pub mod assign;
pub mod automation;
pub mod boot;
pub mod bootstrapper;
pub mod broadcast;
pub mod bus;
pub mod chat_stream;
pub mod cli_runner;
pub mod commands;
pub mod computer_use;
pub mod connect;
pub mod constitution;
pub mod council;
pub mod eval;
pub mod fleet;
pub mod fork;
pub mod github;
pub mod goals;
pub mod handoff;
pub mod help;
pub mod hermes_engine;
pub mod hermes_surfaces;
pub mod hub_identity;
pub mod kanban;
pub mod learning;
pub mod mcp_manager;
pub mod mcp_stdio;
pub mod memory_index;
pub mod memory_ledger;
pub mod memory_quality;
pub mod missions;
pub mod model_layer;
pub mod model_watch;
pub mod models;
pub mod notion;
pub mod notion_sync;
pub mod offline;
pub mod ops;
pub mod org;
pub mod permissions;
pub mod plans;
pub mod policy;
pub mod procs;
pub mod projects;
pub mod provenance;
pub mod pty;
pub mod review_agent;
pub mod router;
pub mod routing;
pub mod settings;
pub mod single;
pub mod skill_proposals;
pub mod skills;
pub mod store;
pub mod stuck;
pub mod timeline;
pub mod updates;
pub mod usage;
pub mod vault;
pub mod workdir;
pub mod worktree;

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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .manage(state)
        .setup(move |app| {
            let handle = app.handle().clone();
            if let Ok(dir) = app.path().resource_dir() {
                help::set_resource_dir(&dir);
            }
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
            // The scheduler starts headless workers itself, so it needs the app handle and the
            // shared worker table the board's Run now button uses.
            bus::set_host(handle.clone(), st.bus.workers.clone());
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
            ops::hermes_memory_view,
            ops::hermes_journey_delete,
            ops::hermes_insights,
            ops::hermes_security_audit,
            ops::hermes_cron_jobs,
            ops::hermes_cron_action,
            ops::hermes_cron_create,
            ops::hermes_profiles,
            ops::hermes_set_role_profile,
            ops::hermes_notify_target,
            ops::hermes_set_notify_target,
            ops::hermes_time_caps,
            ops::hermes_set_time_caps,
            ops::plan_prompt,
            ops::plan_run_prompt,
            ops::latest_plan,
            ops::hermes_fallback_chain,
            ops::hermes_set_fallback_chain,
            ops::hermes_credentials,
            ops::hermes_credential_action,
            ops::hermes_moa,
            ops::hermes_set_active_moa,
            ops::folder_models,
            ops::set_folder_models,
            ops::missions_parallel_per_repo,
            ops::set_missions_parallel_per_repo,
            ops::model_changes,
            ops::answer_chat_permission,
            ops::hermes_chat_transport,
            ops::set_hermes_chat_transport,
            ops::warm_hermes_chat,
            ops::approval_timeout,
            ops::set_approval_timeout,
            ops::computer_use_status,
            ops::computer_use_set_enabled,
            ops::computer_use_activity,
            ops::computer_use_screenshot,
            ops::hermes_model_aliases,
            ops::hermes_set_model_alias,
            ops::hermes_remove_model_alias,
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
            commands::fork_session,
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
            commands::extract_lessons,
            commands::memory_quality_last,
            commands::memory_quality_run,
            commands::memory_quality_schedule,
            commands::create_mission,
            commands::list_missions,
            commands::goal_create,
            commands::goals_list,
            commands::goal_detail,
            commands::goal_set_autonomy,
            commands::goal_delete,
            commands::autonomy_defaults,
            commands::set_repo_autonomy,
            commands::dispatch_mission,
            commands::submit_mission,
            commands::review_mission,
            commands::mission_merge_blockers,
            commands::review_with_agent,
            commands::merge_mission,
            commands::model_catalog,
            commands::router_suggestions,
            commands::router_verdicts,
            commands::routing_policy,
            commands::mission_pins,
            commands::agent_cards,
            commands::set_agent_strengths,
            commands::agent_events,
            commands::mission_kinds,
            commands::set_mission_kind,
            commands::set_mission_urgent,
            commands::mission_timeline,
            commands::timeline_missions,
            commands::eval_run,
            commands::eval_list,
            commands::router_auto,
            commands::set_router_auto,
            commands::help_docs,
            commands::offline_status,
            commands::tool_policies,
            commands::set_tool_policy,
            commands::hub_tools,
            commands::studio_update_check,
            commands::studio_update_install,
            commands::session_usage,
            commands::provider_credits,
            commands::provider_connections,
            commands::save_provider_key,
            commands::notion_publish,
            commands::notion_pull_inbox,
            commands::notion_sync_missions,
            commands::notion_archive_board,
            commands::approve_mission,
            commands::mission_notion_authors,
            commands::notion_trusted_authors,
            commands::set_notion_trusted_authors,
            commands::notion_status,
            commands::automation_status,
            commands::set_automation,
            commands::automation_tick,
            commands::skill_proposals,
            commands::skill_proposal_approve,
            commands::skill_proposal_dismiss,
            commands::file_provenance,
        ])
        .build(tauri::generate_context!())
        .expect("error while building AgentOS Studio")
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                pty::kill_all(&pty_for_exit);
            }
        });
}
