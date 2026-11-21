//! Rust owns capture config: every engine start (UI, hotkey, tray, launch,
//! test mode) builds its `EngineArgs` from persisted Settings here.
use std::path::Path;

use cliplite_core::ffmpeg::CaptureSource;
use cliplite_core::ipc::{AudioConfig, EncConfig, EngineArgs, Region};

use crate::commands::MonitorInfo;
use crate::settings::{Settings, DEFAULT_REPLAY_CAP_MB};

/// Capture source until ADR 0001 decides (provisional: ddagrab, known-good).
pub const DEFAULT_CAPTURE_SOURCE: CaptureSource = CaptureSource::Ddagrab;

/// `CLIPLITE_CAPTURE_SOURCE=ddagrab|gfxcapture|test_pattern` overrides the default
/// (HIL A/B runs; `test_pattern` for checks that must not capture the screen).
pub fn capture_source(env: Option<&str>) -> CaptureSource {
    let v = env.unwrap_or("").trim().to_ascii_lowercase();
    match v.as_str() {
        "" => DEFAULT_CAPTURE_SOURCE,
        "ddagrab" => CaptureSource::Ddagrab,
        "gfxcapture" => CaptureSource::Gfxcapture,
        "test_pattern" => CaptureSource::TestPattern,
        other => {
            log::warn!("CLIPLITE_CAPTURE_SOURCE={other:?} is not ddagrab|gfxcapture|test_pattern; using {DEFAULT_CAPTURE_SOURCE:?}");
            DEFAULT_CAPTURE_SOURCE
        }
    }
}

/// Engine start args from Settings: selected monitor (else the first), audio
/// device ids, fps/quality, always MP4.
pub fn engine_args(s: &Settings, monitors: &[MonitorInfo], ffmpeg: &Path, encoder: &str, source: CaptureSource) -> EngineArgs {
    let m = monitors
        .iter()
        .find(|m| m.index == s.selected_monitor)
        .or_else(|| monitors.first());
    let region = match m {
        Some(m) => Region { x: m.x, y: m.y, width: m.width as i32, height: m.height as i32, monitor: m.index },
        None => Region { x: 0, y: 0, width: 1920, height: 1080, monitor: 0 },
    };
    EngineArgs {
        ffmpeg_path: ffmpeg.to_string_lossy().into_owned(),
        source,
        region,
        audio_cfg: AudioConfig {
            mic_device: s.mic_device.clone(),
            sys_device_id: s.sys_audio_device_id.clone(),
            mic_device_id: s.mic_device_id.clone(),
        },
        enc_cfg: EncConfig {
            fps: s.default_fps,
            quality_crf: s.default_quality_crf,
            format: "mp4".to_string(),
            hw_encoder: encoder.to_string(),
        },
        audio_pipe: None,
    }
}

/// Ring limits for the engine: `(duration + 2 s headroom, memory cap MB)`.
/// The ring is always bounded: a cap of 0 (v1 "unlimited", or a 0 typed in
/// the UI) means the default cap.
pub fn ring_limits(s: &Settings) -> (u32, u32) {
    let max_mb = match s.max_replay_buffer_size_mb {
        0 => DEFAULT_REPLAY_CAP_MB,
        mb => mb,
    };
    (s.replay_buffer_duration_secs.saturating_add(2), max_mb)
}

/// Encoder for the engine: the startup-detected one, else an explicit
/// Settings choice, else NVENC (optimistic, as before).
pub fn resolve_encoder(cached: &str, settings_hw: &str) -> String {
    if !cached.is_empty() {
        cached.to_string()
    } else if !settings_hw.is_empty() && settings_hw != "auto" {
        settings_hw.to_string()
    } else {
        "h264_nvenc".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(index: u32, x: i32, w: u32, h: u32) -> MonitorInfo {
        MonitorInfo { index, name: format!("M{index}"), x, y: 0, width: w, height: h,
                      is_primary: index == 0, refresh_rate: 60.0 }
    }

    #[test]
    fn engine_args_from_settings() {
        let mut s = Settings::default();
        s.selected_monitor = 1;
        s.default_fps = 60;
        s.default_quality_crf = 23;
        s.default_format = "webm".into();
        s.sys_audio_device_id = Some("sys-id".into());
        s.mic_device_id = Some("mic-id".into());
        s.mic_device = Some("Mic".into());
        let a = engine_args(&s, &[mon(0, 0, 1920, 1080), mon(1, 1920, 2560, 1440)],
                            Path::new(r"C:\a b\ffmpeg.exe"), "h264_nvenc", CaptureSource::Gfxcapture);
        assert_eq!(a.ffmpeg_path, r"C:\a b\ffmpeg.exe");
        assert_eq!(a.source, CaptureSource::Gfxcapture);
        let r = &a.region;
        assert_eq!((r.x, r.y, r.width, r.height, r.monitor), (1920, 0, 2560, 1440, 1));
        assert_eq!(a.audio_cfg.sys_device_id.as_deref(), Some("sys-id"));
        assert_eq!(a.audio_cfg.mic_device_id.as_deref(), Some("mic-id"));
        assert_eq!(a.audio_cfg.mic_device.as_deref(), Some("Mic"));
        let e = &a.enc_cfg;
        assert_eq!((e.fps, e.quality_crf, e.format.as_str(), e.hw_encoder.as_str()), (60, 23, "mp4", "h264_nvenc"));
        assert!(a.audio_pipe.is_none(), "the recorder picks its engine pipe");
    }

    #[test]
    fn engine_args_fall_back_to_first_monitor() {
        let mut s = Settings::default();
        s.selected_monitor = 5;
        let a = engine_args(&s, &[mon(0, 0, 1920, 1080)], Path::new("ffmpeg"), "libx264", CaptureSource::Ddagrab);
        assert_eq!((a.region.width, a.region.height, a.region.monitor), (1920, 1080, 0));
        let a = engine_args(&s, &[], Path::new("ffmpeg"), "libx264", CaptureSource::Ddagrab);
        assert_eq!((a.region.width, a.region.height, a.region.monitor), (1920, 1080, 0));
    }

    #[test]
    fn ring_limits_add_headroom() {
        let mut s = Settings::default();
        s.replay_buffer_duration_secs = 60;
        s.max_replay_buffer_size_mb = 512;
        assert_eq!(ring_limits(&s), (62, 512));
        s.replay_buffer_duration_secs = 0;
        assert_eq!(ring_limits(&s), (2, 512), "buffer off: recording-only engine keeps a tiny ring");
    }

    #[test]
    fn ring_limits_zero_cap_is_default_cap() {
        let mut s = Settings::default();
        s.replay_buffer_duration_secs = 60;
        s.max_replay_buffer_size_mb = 0;
        assert_eq!(ring_limits(&s), (62, 512), "memory stays bounded");
    }

    #[test]
    fn capture_source_default_and_env_override() {
        assert_eq!(DEFAULT_CAPTURE_SOURCE, CaptureSource::Ddagrab);
        assert_eq!(capture_source(None), DEFAULT_CAPTURE_SOURCE);
        assert_eq!(capture_source(Some("")), DEFAULT_CAPTURE_SOURCE);
        assert_eq!(capture_source(Some("gfxcapture")), CaptureSource::Gfxcapture);
        assert_eq!(capture_source(Some("ddagrab")), CaptureSource::Ddagrab);
        assert_eq!(capture_source(Some(" Test_Pattern ")), CaptureSource::TestPattern);
        assert_eq!(capture_source(Some("bogus")), DEFAULT_CAPTURE_SOURCE);
    }

    #[test]
    fn encoder_prefers_detected_then_explicit_setting() {
        assert_eq!(resolve_encoder("libx264", "auto"), "libx264");
        assert_eq!(resolve_encoder("", "h264_qsv"), "h264_qsv");
        assert_eq!(resolve_encoder("", "auto"), "h264_nvenc");
        assert_eq!(resolve_encoder("", ""), "h264_nvenc");
    }
}
