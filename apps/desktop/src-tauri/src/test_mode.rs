//! Test-mode support for the hardware-in-the-loop harness.
//!
//! `CLIPLITE_TEST_MODE=1` auto-starts the replay buffer from persisted
//! Settings so the harness can drive the app without clicking the UI.

use crate::commands::{AudioConfig, EncConfig, MonitorInfo, Region};
use crate::settings::Settings;
use tauri::{AppHandle, Manager};

/// True when `CLIPLITE_TEST_MODE` is exactly "1".
pub fn is_enabled(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Build replay start args from persisted Settings, mirroring what the
/// React UI sends (selected monitor, audio ids, fps/quality, mp4).
pub fn replay_args(s: &Settings, monitors: &[MonitorInfo]) -> (Region, AudioConfig, EncConfig) {
    let m = monitors
        .iter()
        .find(|m| m.index == s.selected_monitor)
        .or_else(|| monitors.first());
    let region = match m {
        Some(m) => Region { x: m.x, y: m.y, width: m.width as i32, height: m.height as i32, monitor: m.index },
        None => Region { x: 0, y: 0, width: 1920, height: 1080, monitor: 0 },
    };
    let audio = AudioConfig {
        mic_device: s.mic_device.clone(),
        sys_device_id: s.sys_audio_device_id.clone(),
        mic_device_id: s.mic_device_id.clone(),
    };
    let enc = EncConfig {
        fps: s.default_fps,
        quality_crf: Some(s.default_quality_crf),
        format: Some("mp4".into()),
        hw_encoder: None,
    };
    (region, audio, enc)
}

/// If test mode is enabled, start the replay buffer once the encoder cache
/// is populated (max 10 s wait).
pub fn maybe_autostart(app: &AppHandle) {
    if !is_enabled(std::env::var("CLIPLITE_TEST_MODE").ok().as_deref()) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for _ in 0..100 {
            if !app.state::<crate::EncoderCache>().0.lock().unwrap().is_empty() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let settings = app.state::<crate::settings::SettingsState>().0.lock().unwrap().clone();
        let (region, audio, enc) = replay_args(&settings, &crate::commands::get_monitors());
        match crate::commands::start_replay(app.clone(), region, audio, enc,
                                            app.state(), app.state(), app.state()).await {
            Ok(path) => log::info!("[test-mode] replay auto-started: {path}"),
            Err(e)   => log::error!("[test-mode] replay auto-start failed: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::MonitorInfo;
    use crate::settings::Settings;

    fn mon(index: u32, x: i32, w: u32, h: u32) -> MonitorInfo {
        MonitorInfo { index, name: format!("M{index}"), x, y: 0, width: w, height: h,
                      is_primary: index == 0, refresh_rate: 60.0 }
    }

    #[test]
    fn builds_replay_args_from_settings() {
        let mut s = Settings::default();
        s.selected_monitor = 1;
        s.default_fps = 60;
        s.default_quality_crf = 23;
        s.sys_audio_device_id = Some("sys-id".into());
        s.mic_device_id = Some("mic-id".into());
        s.mic_device = Some("Mic".into());
        let (region, audio, enc) = replay_args(&s, &[mon(0, 0, 1920, 1080), mon(1, 1920, 2560, 1440)]);
        assert_eq!((region.x, region.y, region.width, region.height, region.monitor), (1920, 0, 2560, 1440, 1));
        assert_eq!(audio.sys_device_id.as_deref(), Some("sys-id"));
        assert_eq!(audio.mic_device_id.as_deref(), Some("mic-id"));
        assert_eq!(audio.mic_device.as_deref(), Some("Mic"));
        assert_eq!((enc.fps, enc.quality_crf, enc.format.as_deref(), enc.hw_encoder.as_deref()),
                   (60, Some(23), Some("mp4"), None));
    }

    #[test]
    fn falls_back_to_first_monitor_when_selection_missing() {
        let mut s = Settings::default();
        s.selected_monitor = 5;
        let (region, _, _) = replay_args(&s, &[mon(0, 0, 1920, 1080)]);
        assert_eq!((region.width, region.height, region.monitor), (1920, 1080, 0));
    }

    #[test]
    fn enabled_only_for_exact_one() {
        assert!(is_enabled(Some("1")));
        assert!(!is_enabled(Some("0")));
        assert!(!is_enabled(None));
    }
}
