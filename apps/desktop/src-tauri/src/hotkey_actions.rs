//! Hotkey actions, run in Rust: a press never needs the WebView to be alive,
//! visible or listening. Each action calls the same core function as its
//! `#[tauri::command]` (no logic duplication).
use std::str::FromStr;
use tauri::{AppHandle, Emitter, Manager};

use crate::commands;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Start,
    Stop,
    Pause,
    ReplayToggle,
    ReplaySave,
    Library,
}

impl Action {
    pub const ALL: [Action; 6] = [
        Action::Start,
        Action::Stop,
        Action::Pause,
        Action::ReplayToggle,
        Action::ReplaySave,
        Action::Library,
    ];

    /// The key in `Settings::hotkeys`.
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Pause => "pause",
            Action::ReplayToggle => "replay_toggle",
            Action::ReplaySave => "replay_save",
            Action::Library => "library",
        }
    }

    /// The pre-WS1 `hotkey-*` event suffix (still accepted by `simulate_hotkey`).
    fn legacy_event(self) -> &'static str {
        match self {
            Action::Start => "start-recording",
            Action::Stop => "stop-recording",
            Action::Pause => "pause-recording",
            Action::ReplayToggle => "replay-toggle",
            Action::ReplaySave => "replay-save",
            Action::Library => "open-library",
        }
    }

    /// An action name, or a legacy event name with or without `hotkey-`.
    pub fn from_any(s: &str) -> Result<Action, String> {
        Action::from_str(s).or_else(|e| {
            let rest = s.strip_prefix("hotkey-").unwrap_or(s);
            Action::ALL
                .into_iter()
                .find(|a| a.legacy_event() == rest)
                .ok_or(e)
        })
    }
}

impl FromStr for Action {
    type Err = String;

    fn from_str(s: &str) -> Result<Action, String> {
        Action::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| format!("unknown hotkey action: {s:?}"))
    }
}

/// Run `action` with correlation id `cid`. Logs `command.received` so the
/// press -> command chain shares one cid in the logs.
pub async fn run(app: &AppHandle, action: Action, cid: &str) -> Result<(), String> {
    let name = match action {
        Action::Start => "start_recording",
        Action::Stop => "stop_recording",
        Action::Pause => "toggle_pause",
        Action::ReplayToggle => "replay_toggle",
        Action::ReplaySave => "save_replay",
        Action::Library => "open_library",
    };
    commands::received(name, Some(cid.to_string()));
    match action {
        Action::Start => commands::start_recording_core(app, cid).await.map(drop),
        Action::Stop => commands::stop_recording_core(app, cid).await.map(drop),
        Action::Pause => commands::toggle_pause(app, cid).await,
        Action::ReplayToggle => commands::replay_toggle(app, cid).await,
        Action::ReplaySave => {
            // Same length the UI's Save button uses (its selector mirrors Settings).
            let secs = commands::settings_of(app).replay_buffer_duration_secs;
            if secs == 0 {
                return Err("Replay buffer duration is Off in Settings".into());
            }
            commands::save_replay_core(app, secs, cid).await.map(drop)
        }
        Action::Library => open_library(app),
    }
}

/// Show and focus the main window, then ask the UI to switch to the library.
fn open_library(app: &AppHandle) -> Result<(), String> {
    let win = app.get_webview_window("main").ok_or("main window not found")?;
    win.unminimize().ok();
    win.show().map_err(|e| e.to_string())?;
    win.set_focus().ok();
    app.emit("navigate", "library").map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_from_str_every_name() {
        for (name, a) in [
            ("start", Action::Start),
            ("stop", Action::Stop),
            ("pause", Action::Pause),
            ("replay_toggle", Action::ReplayToggle),
            ("replay_save", Action::ReplaySave),
            ("library", Action::Library),
        ] {
            assert_eq!(Action::from_str(name), Ok(a));
            assert_eq!(a.as_str(), name);
        }
    }

    #[test]
    fn action_from_str_unknown_is_err() {
        for bad in ["", "annotate", "Start", "start-recording", "hotkey-start-recording"] {
            assert!(Action::from_str(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn from_any_accepts_legacy_event_names() {
        assert_eq!(Action::from_any("replay_save"), Ok(Action::ReplaySave));
        assert_eq!(Action::from_any("start-recording"), Ok(Action::Start));
        assert_eq!(Action::from_any("hotkey-open-library"), Ok(Action::Library));
        assert!(Action::from_any("annotate").is_err());
    }
}
