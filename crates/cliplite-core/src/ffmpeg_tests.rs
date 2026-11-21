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

// ── WS2: engine / remux / export builders ─────────────────────────────────────

fn engine(source: CaptureSource, audio: bool, w: i32, h: i32, hw: &str) -> Vec<String> {
    let region = Region { x: 0, y: 0, width: w, height: h, monitor: 0 };
    let enc_cfg = EncConfig { fps: 30, quality_crf: 23, format: "mp4".into(), hw_encoder: hw.into() };
    let fmt48 = AudioFormat { sample_rate: 48000, channels: 2 };
    build_engine_command(
        Path::new(r"C:\ff mpeg\ffmpeg.exe"),
        source,
        &region,
        &enc_cfg,
        if audio { Some(&fmt48) } else { None },
        r"\.\pipe\cliplite_engine_audio",
    )
}

#[test]
fn engine_cmd_matrix() {
    let sources = [
        (CaptureSource::Gfxcapture, "gfxcapture"),
        (CaptureSource::Ddagrab, "ddagrab"),
        (CaptureSource::TestPattern, "test_pattern"),
    ];
    for (src, name) in sources {
        for (audio, an) in [(true, "audio"), (false, "noaudio")] {
            let cmd = engine(src, audio, 1920, 1080, "h264_nvenc");
            assert!(cmd.ends_with(&["-f".to_string(), "mpegts".into(), "pipe:1".into()]), "{cmd:?}");
            assert!(cmd.windows(2).any(|w| w[0] == "-g" && w[1] == "30"), "{cmd:?}");
            assert!(cmd.windows(2).any(|w| w[0] == "-keyint_min" && w[1] == "30"), "{cmd:?}");
            assert!(!cmd.iter().any(|a| a.contains("display_border=1")), "{cmd:?}");
            assert!(cmd.windows(2).any(|w| w[0] == "-pes_payload_size" && w[1] == "0"), "{cmd:?}");
            snap!(format!("engine_cmd__{name}_{an}"), cmd);
        }
    }
}

#[test]
fn engine_cmd_region_2560x1440() {
    snap!("engine_cmd__gfxcapture_audio_2560x1440", engine(CaptureSource::Gfxcapture, true, 2560, 1440, "h264_nvenc"));
}

#[test]
fn engine_cmd_libx264_real_source() {
    snap!("engine_cmd__ddagrab_audio_libx264", engine(CaptureSource::Ddagrab, true, 1920, 1080, "libx264"));
}

#[test]
fn test_pattern_always_libx264() {
    let cmd = engine(CaptureSource::TestPattern, false, 640, 360, "h264_nvenc");
    assert!(cmd.windows(2).any(|w| w[0] == "-c:v" && w[1] == "libx264"), "{cmd:?}");
    assert!(!cmd.iter().any(|a| a == "h264_nvenc"));
}

#[test]
fn test_pattern_inputs_run_at_real_time() {
    for audio in [false, true] {
        let cmd = engine(CaptureSource::TestPattern, audio, 640, 360, "libx264");
        let inputs = cmd.iter().filter(|a| *a == "-i").count();
        let paced = cmd.windows(3).filter(|w| w[0] == "-re" && w[1] == "-f" && w[2] == "lavfi").count();
        assert_eq!(paced, inputs, "{cmd:?}");
    }
    let real = engine(CaptureSource::Ddagrab, true, 1920, 1080, "h264_nvenc");
    assert!(!real.iter().any(|a| a == "-re"), "{real:?}");
}

#[test]
fn concat_remux_cmd() {
    snap!("concat_remux_cmd", build_concat_remux_command(
        Path::new(r"C:\ff mpeg\ffmpeg.exe"), Path::new(r"C:\tmp\list.txt"), Path::new(r"C:\out dir\clip.mp4")));
}

#[test]
fn export_cmds() {
    for (fmt, name) in [(ExportFormat::Mp4Copy, "mp4copy"), (ExportFormat::Webm, "webm"), (ExportFormat::Gif, "gif")] {
        snap!(format!("export_cmd__{name}"), build_export_command(
            Path::new(r"C:\ff mpeg\ffmpeg.exe"), Path::new(r"C:\in dir\clip.mp4"), 1.5, 12.25, fmt, Path::new(r"C:\out dir\out.bin")));
    }
}
