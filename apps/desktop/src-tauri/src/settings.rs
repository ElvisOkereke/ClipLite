/// Application settings with JSON persistence.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// All fields have #[serde(default)] so settings files written by older versions
/// (missing newer fields) still deserialize correctly instead of falling back
/// to Settings::default() entirely and wiping all persisted values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Schema version; a file without it is v1 (field default 0 -> `migrate`).
    #[serde(default)]
    pub settings_version: u32,
    pub output_dir: String,
    pub default_format: String,
    pub default_fps: u32,
    pub default_quality_crf: u32,
    pub mic_device: Option<String>,
    pub mic_device_id: Option<String>,          // WASAPI endpoint ID for mic
    pub sys_audio_device: Option<String>,
    pub sys_audio_device_id: Option<String>,    // WASAPI endpoint ID for speaker loopback
    pub capture_cursor: bool,
    pub show_keystroke_hud: bool,
    pub hotkeys: HashMap<String, String>,
    pub hw_encoder: String,
    pub minimize_to_tray: bool,
    pub filename_template: String,
    pub replay_buffer_duration_secs: u32,   // 0 = disabled
    pub replay_output_dir: String,          // empty = same as output_dir
    pub replay_filename_template: String,   // e.g. "replay_{datetime}"
    pub selected_monitor: u32,              // last-used monitor index (0 = primary)
    pub max_replay_buffer_size_mb: u32,     // replay ring memory cap in MB (0 = no byte cap; the duration governs)
}

/// Current settings schema. v2 = in-memory replay engine (WS2).
pub const SETTINGS_VERSION: u32 = 2;

impl Default for Settings {
    fn default() -> Self {
        let mut hotkeys = HashMap::new();
        hotkeys.insert("start".to_string(),         "CommandOrControl+Shift+R".to_string());
        hotkeys.insert("stop".to_string(),          "CommandOrControl+Shift+S".to_string());
        hotkeys.insert("pause".to_string(),         "CommandOrControl+Shift+P".to_string());
        hotkeys.insert("library".to_string(),       "CommandOrControl+Shift+L".to_string());
        hotkeys.insert("replay_toggle".to_string(), "CommandOrControl+Shift+B".to_string());
        hotkeys.insert("replay_save".to_string(),   "CommandOrControl+Shift+F".to_string());

        let output_dir = dirs::video_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default())
            .join("ClipLite")
            .to_string_lossy()
            .to_string();

        Self {
            settings_version: SETTINGS_VERSION,
            output_dir,
            default_format: "mp4".to_string(),
            default_fps: 30,
            default_quality_crf: 28,
            mic_device: None,
            mic_device_id: None,
            sys_audio_device: None,
            sys_audio_device_id: None,
            capture_cursor: true,
            show_keystroke_hud: true,
            hotkeys,
            hw_encoder: "auto".to_string(),
            minimize_to_tray: true,
            filename_template: "recording_{datetime}".to_string(),
            replay_buffer_duration_secs: 60,
            replay_output_dir: String::new(),   // empty = same as output_dir
            replay_filename_template: "replay_{datetime}".to_string(),
            selected_monitor: 0,
            max_replay_buffer_size_mb: 512,
        }
    }
}

pub use cliplite_core::paths::app_dir;

impl Settings {
    /// Upgrade an older settings file in place; true if anything changed.
    /// v1 -> v2: buffer on by default (0 -> 60 s), recordings are MP4 only,
    /// the old 500 MB default cap becomes 512 MB.
    pub fn migrate(&mut self) -> bool {
        if self.settings_version >= SETTINGS_VERSION {
            return false;
        }
        if self.replay_buffer_duration_secs == 0 {
            self.replay_buffer_duration_secs = 60;
        }
        if matches!(self.default_format.as_str(), "webm" | "gif") {
            self.default_format = "mp4".to_string();
        }
        if self.max_replay_buffer_size_mb == 500 {
            self.max_replay_buffer_size_mb = 512;
        }
        self.settings_version = SETTINGS_VERSION;
        true
    }

    fn settings_path() -> PathBuf {
        app_dir().join("settings.json")
    }

    pub fn load() -> Self {
        let path = Self::settings_path();
        if !path.exists() {
            return Self::default();
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => { log::warn!("Failed to read settings: {e}"); return Self::default(); }
        };
        match serde_json::from_str::<Self>(&content) {
            Ok(mut s) => {
                let from = s.settings_version;
                if s.migrate() {
                    log::info!("Settings migrated from v{from} to v{SETTINGS_VERSION}");
                    if let Err(e) = s.save() {
                        log::warn!("Failed to save migrated settings: {e}");
                    }
                }
                s
            }
            Err(e) => {
                log::warn!("Failed to parse settings ({e}), using defaults. \
                            Old file backed up as settings.json.bak");
                // Back up the corrupt file before overwriting
                let bak = path.with_extension("json.bak");
                std::fs::copy(&path, &bak).ok();
                Self::default()
            }
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::settings_path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn resolve_output_dir(&self) -> PathBuf {
        let expanded = if self.output_dir.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                home.join(self.output_dir.trim_start_matches("~/").trim_start_matches('~'))
            } else {
                PathBuf::from(&self.output_dir)
            }
        } else {
            PathBuf::from(&self.output_dir)
        };
        std::fs::create_dir_all(&expanded).ok();
        expanded
    }

    pub fn build_output_path(&self, extension: &str) -> PathBuf {
        let now  = chrono::Local::now();
        let name = self.apply_template(&self.filename_template, &now);
        self.resolve_output_dir().join(format!("{}{}", name, extension))
    }

    pub fn build_replay_output_path(&self, extension: &str) -> PathBuf {
        let now = chrono::Local::now();
        let name = self.apply_template(&self.replay_filename_template, &now);
        let dir = if self.replay_output_dir.is_empty() {
            self.resolve_output_dir()
        } else {
            let d = PathBuf::from(&self.replay_output_dir);
            std::fs::create_dir_all(&d).ok();
            d
        };
        dir.join(format!("{}{}", name, extension))
    }

    fn apply_template(&self, template: &str, now: &chrono::DateTime<chrono::Local>) -> String {
        template
            .replace("{datetime}", &now.format("%Y%m%d_%H%M%S").to_string())
            .replace("{date}",     &now.format("%Y%m%d").to_string())
            .replace("{time}",     &now.format("%H%M%S").to_string())
    }
}

#[derive(Default)]
pub struct SettingsState(pub Mutex<Settings>);

#[cfg(test)]
mod tests {
    use super::*;

    fn v1(duration: u32, format: &str, cap: u32) -> Settings {
        let json = serde_json::json!({
            "replay_buffer_duration_secs": duration,
            "default_format": format,
            "max_replay_buffer_size_mb": cap,
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn file_without_version_is_v1() {
        assert_eq!(v1(0, "gif", 500).settings_version, 0);
        assert_eq!(Settings::default().settings_version, SETTINGS_VERSION);
    }

    #[test]
    fn migrate_v1_defaults() {
        let mut s = v1(0, "gif", 500);
        assert!(s.migrate());
        assert_eq!((s.replay_buffer_duration_secs, s.default_format.as_str(), s.max_replay_buffer_size_mb, s.settings_version),
                   (60, "mp4", 512, 2));
        let mut w = v1(30, "webm", 100);
        assert!(w.migrate());
        assert_eq!((w.replay_buffer_duration_secs, w.default_format.as_str(), w.max_replay_buffer_size_mb), (30, "mp4", 100));
    }

    #[test]
    fn migrate_keeps_user_choices() {
        let mut s = v1(180, "mp4", 2048);
        let before = serde_json::to_value(&s).unwrap();
        assert!(s.migrate());
        let mut after = serde_json::to_value(&s).unwrap();
        assert_eq!(after["settings_version"], 2);
        after["settings_version"] = before["settings_version"].clone();
        assert_eq!(after, before, "only the version changes");
    }

    #[test]
    fn migrate_is_idempotent() {
        let mut s = v1(0, "gif", 500);
        assert!(s.migrate());
        let once = serde_json::to_value(&s).unwrap();
        assert!(!s.migrate());
        assert_eq!(serde_json::to_value(&s).unwrap(), once);
        assert!(!Settings::default().clone().migrate(), "fresh defaults are already v2");
    }
}

