//! Every opt-in live test in one binary (#107): one link instead of one per file.
//!
//! These tests drive the real machine (Hermes, Claude Code, agy, GitHub, Notion, the vault), are
//! all `#[ignore]`, and run one at a time, by module, with the live-machine lock held:
//!
//! `cargo test --test live -- live_smoke:: --ignored --nocapture --test-threads 1`
//!
//! Always pass `--test-threads 1`: tests in one binary share a process, and several set
//! environment variables. `first_run` and `native_probe` stay separate binaries because they run
//! in every `cargo test` and `first_run` redirects the process environment.

mod live_acp_chat;
mod live_acp_spike;
mod live_agy_mission;
mod live_ask;
mod live_auto_review;
mod live_brain;
mod live_chat;
mod live_checkpoints;
mod live_claude_mission;
mod live_computer_use;
mod live_council;
mod live_eval;
mod live_first_run;
mod live_fork;
mod live_handoff;
mod live_hermes_surfaces;
mod live_hub_autoupdate;
mod live_hub_cutover;
mod live_hub_identity;
mod live_kanban_mission;
mod live_learning;
mod live_memory_quality;
mod live_mission_loop;
mod live_model_layer;
mod live_model_watch;
mod live_notion;
mod live_notion_goals;
mod live_notion_sync;
mod live_phase_b_done;
mod live_phase_c_done;
mod live_phase_e_done;
mod live_plan_mode;
mod live_planner;
mod live_provenance;
mod live_router;
mod live_skill_proposals;
mod live_smoke;
mod live_usage;
mod live_v3;
mod live_v4;
