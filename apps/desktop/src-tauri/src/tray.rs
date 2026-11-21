/// System tray setup.
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, Emitter, Manager, Runtime,
};

pub fn setup_tray(app: &mut App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "Open ClipLite", true, None::<&str>)?;
    let start_item = MenuItem::with_id(app, "start", "Start Recording", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&show_item, &start_item, &quit_item])?;

    TrayIconBuilder::with_id("main-tray")
        .tooltip("ClipLite")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            "start" => {
                let _ = app.emit("tray-start-recording", ());
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                let app = tray.app_handle();
                // Single click: show/hide window
                if let Some(win) = app.get_webview_window("main") {
                    if win.is_visible().unwrap_or(false) {
                        let _ = win.hide();
                    } else {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
            }
        })
        .build(app)?;

    Ok(())
}

/// What the tray tooltip shows: recording and replay-buffer state.
static TRAY_STATE: std::sync::Mutex<(bool, Option<u32>)> = std::sync::Mutex::new((false, None));

fn refresh<R: Runtime>(app: &tauri::AppHandle<R>, update: impl FnOnce(&mut (bool, Option<u32>))) {
    let text = {
        let mut st = TRAY_STATE.lock().unwrap_or_else(|e| e.into_inner());
        update(&mut st);
        tooltip(st.0, st.1)
    };
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_tooltip(Some(&text));
    }
}

/// Update the tray tooltip to reflect recording state.
pub fn set_recording_state<R: Runtime>(app: &tauri::AppHandle<R>, is_recording: bool) {
    refresh(app, |st| st.0 = is_recording);
}

/// Update the tray tooltip to reflect the replay buffer (`None` = off).
pub fn set_replay_state<R: Runtime>(app: &tauri::AppHandle<R>, buffer_secs: Option<u32>) {
    refresh(app, |st| st.1 = buffer_secs);
}

/// Tooltip text for the tray state (`None` buffer = off).
pub fn tooltip(recording: bool, buffer_secs: Option<u32>) -> String {
    let head = if recording { "ClipLite — Recording…" } else { "ClipLite" };
    match buffer_secs {
        Some(n) => format!("{head}\nReplay buffer: on ({n} s)"),
        None => format!("{head}\nReplay buffer: off"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_shows_buffer_and_recording() {
        assert_eq!(tooltip(false, Some(60)), "ClipLite\nReplay buffer: on (60 s)");
        assert_eq!(tooltip(false, None), "ClipLite\nReplay buffer: off");
        assert_eq!(tooltip(true, Some(120)), "ClipLite — Recording…\nReplay buffer: on (120 s)");
    }
}
