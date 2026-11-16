//! The fleet keeps working with the window closed (Beyond B0.3).
//!
//! The scheduler, the watcher and the goal loop live in the GUI process, so closing the window
//! used to stop every agent until the owner opened Studio again. Closing now hides the window to
//! the system tray; the tray menu opens it again, pauses or resumes the fleet, or quits (which
//! is the only way to stop the fleet from the tray). kv `window.close_to_tray` = `0` restores the
//! old behaviour.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime, Window, WindowEvent};

use crate::store;

/// Set by the tray's Quit, so the close that follows really exits.
static QUITTING: AtomicBool = AtomicBool::new(false);

pub const CLOSE_KEY: &str = "window.close_to_tray";
const HINT_KEY: &str = "window.tray_hint_shown";

/// Should closing the main window hide it instead of quitting? Pure.
pub fn hide_on_close(setting: Option<&str>, quitting: bool) -> bool {
    !quitting && setting.map(str::trim) != Some("0")
}

/// The tray menu's pause entry, by the fleet's current state. Pure.
pub fn pause_label(paused: bool) -> &'static str {
    if paused {
        "Resume the fleet"
    } else {
        "Pause the fleet"
    }
}

fn kv(key: &str) -> Option<String> {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, key).ok().flatten())
}

fn kv_set(key: &str, value: &str) {
    if let Ok(c) = store::open() {
        let _ = store::kv_set(&c, key, value);
    }
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Put the tray icon up. Failure is not fatal: the app then simply quits on close as before.
pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open AgentOS Studio", true, None::<&str>)?;
    let pause = MenuItem::with_id(
        app,
        "pause",
        pause_label(crate::pause::paused()),
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit (stops the fleet)", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &pause, &sep, &quit])?;
    let pause_item = pause.clone();
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("AgentOS Studio: the fleet keeps working")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, ev| match ev.id().as_ref() {
            "open" => show_main(app),
            "pause" => {
                let now_paused = if crate::pause::paused() {
                    crate::pause::resume().map(|_| false)
                } else {
                    crate::pause::pause("Paused from the tray").map(|_| true)
                };
                match now_paused {
                    Ok(p) => {
                        let _ = pause_item.set_text(pause_label(p));
                    }
                    Err(e) => eprintln!("tray: pause toggle failed: {e:#}"),
                }
            }
            "quit" => {
                QUITTING.store(true, Ordering::SeqCst);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Window events for the main window: a close hides it to the tray while the fleet keeps
/// working, and the first time says so once.
pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    if window.label() != "main"
        || !hide_on_close(kv(CLOSE_KEY).as_deref(), QUITTING.load(Ordering::SeqCst))
        || window.app_handle().tray_by_id("main").is_none()
    {
        return;
    }
    api.prevent_close();
    let _ = window.hide();
    if kv(HINT_KEY).is_none() {
        kv_set(HINT_KEY, "1");
        use tauri_plugin_notification::NotificationExt;
        let _ = window
            .app_handle()
            .notification()
            .builder()
            .title("AgentOS Studio is still working")
            .body("The fleet keeps running in the tray. Use the tray icon to open Studio or quit.")
            .show();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_hides_to_the_tray_unless_turned_off_or_quitting() {
        assert!(hide_on_close(None, false));
        assert!(hide_on_close(Some("1"), false));
        assert!(!hide_on_close(Some("0"), false));
        assert!(!hide_on_close(Some(" 0 "), false));
        assert!(!hide_on_close(None, true), "the tray's Quit really quits");
    }

    #[test]
    fn the_pause_entry_names_what_it_will_do() {
        assert_eq!(pause_label(false), "Pause the fleet");
        assert_eq!(pause_label(true), "Resume the fleet");
    }
}
