//! Characterization tests: pin the exact FFmpeg commands the recorder builds,
//! so refactors and encoder work (WS3) show every change.
use super::*;
use crate::audio::AudioFormat;
use std::path::Path;

macro_rules! snap {
    ($name:expr, $value:expr) => {
        insta::with_settings!({ prepend_module_to_snapshot => false, omit_expression => true }, {
            insta::assert_debug_snapshot!($name, $value);
        });
    };
}

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
