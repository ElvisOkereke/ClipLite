//! Characterization tests: pin the exact FFmpeg command the recorder builds
//! today, so refactors (WS0) and encoder work (WS3) show every change.
use super::*;
use crate::audio::AudioFormat;
use std::path::Path;

#[derive(Debug, Clone, Copy)]
enum AudioCase {
    None,
    Sys,
    SysAndMic,
}

fn audio_cfg(case: AudioCase) -> AudioConfig {
    match case {
        AudioCase::None => AudioConfig { mic_device: None, sys_device_id: None, mic_device_id: None },
        AudioCase::Sys => AudioConfig {
            mic_device: None,
            sys_device_id: Some("sys-endpoint".into()),
            mic_device_id: None,
        },
        AudioCase::SysAndMic => AudioConfig {
            mic_device: Some("Mic (USB)".into()),
            sys_device_id: Some("sys-endpoint".into()),
            mic_device_id: Some("mic-endpoint".into()),
        },
    }
}

fn build(enc: &str, fmt: &str, case: AudioCase, w: i32, h: i32) -> Vec<String> {
    let region = Region { x: 0, y: 0, width: w, height: h, monitor: 0 };
    let enc_cfg = EncConfig { fps: 60, quality_crf: 23, format: fmt.into(), hw_encoder: enc.into() };
    let fmt48 = AudioFormat { sample_rate: 48000, channels: 2 };
    let sys_fmt = match case {
        AudioCase::None => None,
        _ => Some(&fmt48),
    };
    build_record_command_with_pipe(
        Path::new(r"C:\ff mpeg\ffmpeg.exe"),
        &region,
        &audio_cfg(case),
        &enc_cfg,
        r"C:\out dir\clip.mp4",
        sys_fmt,
        r"\\.\pipe\cliplite_sysaudio_replay",
    )
}

macro_rules! snap {
    ($name:expr, $value:expr) => {
        insta::with_settings!({ prepend_module_to_snapshot => false, omit_expression => true }, {
            insta::assert_debug_snapshot!($name, $value);
        });
    };
}

#[cfg(windows)]
#[test]
fn ffmpeg_args_encoder_audio_region_matrix() {
    for enc in ["h264_nvenc", "libx264", "h264_amf", "h264_qsv"] {
        for case in [AudioCase::None, AudioCase::Sys, AudioCase::SysAndMic] {
            for (w, h) in [(1920, 1080), (2560, 1440)] {
                snap!(format!("ffmpeg_args__{enc}_{case:?}_{w}x{h}"), build(enc, "mp4", case, w, h));
            }
        }
    }
}

#[cfg(windows)]
#[test]
fn ffmpeg_args_webm_and_gif() {
    for fmt in ["webm", "gif"] {
        snap!(
            format!("ffmpeg_args__h264_nvenc_Sys_{fmt}"),
            build("h264_nvenc", fmt, AudioCase::Sys, 1920, 1080)
        );
    }
}
