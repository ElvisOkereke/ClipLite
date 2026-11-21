/// FFmpeg discovery, process spawning, and command building.
/// Pure Rust — no launcher.exe, no Python, no external shims.
///
/// Why the Tauri exe doesn't need a launcher:
/// The Tauri app itself is built with a proper Windows application manifest
/// (PerMonitorV2 DPI awareness, Windows 10/11 compatibility) via tauri.conf.json.
/// FFmpeg inherits this context as a child process, so ddagrab's AcquireNextFrame
/// delivers frames at the full monitor refresh rate — no shim needed.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use anyhow::{anyhow, Result};

// ── FFmpeg / ffprobe discovery ────────────────────────────────────────────────

/// Locate FFmpeg. Checks `<exe_dir>/bin/ffmpeg[.exe]` first, then PATH.
pub fn find_ffmpeg() -> Result<PathBuf> {
    let dir = bin_dir();

    #[cfg(windows)] let name = "ffmpeg.exe";
    #[cfg(not(windows))] let name = "ffmpeg";

    let bundled = dir.join(name);
    if bundled.exists() { return Ok(bundled); }

    which::which("ffmpeg").map_err(|_| anyhow!(
        "FFmpeg not found.\n\
         Place ffmpeg.exe in the bin/ folder next to the app,\n\
         or install it on PATH.\n\
         Download: https://ffmpeg.org/download.html"
    ))
}

/// Locate ffprobe in the same directory as ffmpeg.
pub fn find_ffprobe() -> Result<PathBuf> {
    let dir = find_ffmpeg()?.parent().unwrap_or(Path::new(".")).to_path_buf();

    #[cfg(windows)] let name = "ffprobe.exe";
    #[cfg(not(windows))] let name = "ffprobe";

    let p = dir.join(name);
    if p.exists() { return Ok(p); }
    which::which("ffprobe").map_err(|_| anyhow!("ffprobe not found"))
}

fn bin_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_default()
        .join("bin")
}

// ── Hardware encoder detection ────────────────────────────────────────────────

pub fn detect_hw_encoder() -> String {
    let ffmpeg = match find_ffmpeg() {
        Ok(p) => p,
        Err(_) => return "libx264".into(),
    };

    let tests: &[(&str, &[&str])] = &[
        ("h264_nvenc", &[
            "-f", "lavfi", "-i", "color=c=black:s=256x256:r=30,format=bgra",
            "-frames:v", "10", "-c:v", "h264_nvenc", "-pix_fmt", "yuv420p",
            "-f", "null", "-",
        ]),
        ("h264_videotoolbox", &[
            "-f", "lavfi", "-i", "nullsrc=s=128x128,format=yuv420p",
            "-frames:v", "5", "-c:v", "h264_videotoolbox", "-f", "null", "-",
        ]),
        ("h264_vaapi", &[
            "-f", "lavfi", "-i", "nullsrc=s=128x128,format=yuv420p",
            "-frames:v", "5", "-c:v", "h264_vaapi", "-f", "null", "-",
        ]),
    ];

    for (enc, args) in tests {
        let ok = Command::new(&ffmpeg)
            .args(*args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok { return enc.to_string(); }
    }
    "libx264".into()
}

// ── Engine / remux / export builders ──────────────────────────────────────────
use crate::ipc::{EncConfig, Region};

/// Per-encoder video arguments (`-c:v <enc>` + rate control).
fn add_video_encoder_args(cmd: &mut Vec<String>, enc: &str, crf: u32, width: i32) {
    let maxrate = if width >= 2560 { "16M" } else { "8M" };
    let bufsize = if width >= 2560 { "32M" } else { "16M" };
    cmd.extend(["-c:v".into(), enc.into()]);
    match enc {
        "h264_nvenc" => cmd.extend([
            "-cq".into(), crf.to_string(), "-preset".into(), "p2".into(),
            "-tune".into(), "hq".into(), "-maxrate".into(), maxrate.into(),
            "-bufsize".into(), bufsize.into(), "-bf".into(), "2".into(),
        ]),
        "libx264" => cmd.extend([
            "-pix_fmt".into(), "yuv420p".into(), "-crf".into(), crf.to_string(),
            "-preset".into(), "superfast".into(), "-maxrate".into(), maxrate.into(),
            "-bufsize".into(), bufsize.into(), "-threads".into(), "0".into(),
        ]),
        _ => cmd.extend(["-pix_fmt".into(), "yuv420p".into(), "-b:v".into(), maxrate.into()]),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSource { Gfxcapture, Ddagrab, TestPattern }

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat { Mp4Copy, Webm, Gif }

/// Capture + encode -> MPEG-TS on FFmpeg's stdout (`-f mpegts pipe:1`), GOP = fps
/// (1 s keyframes), `-mpegts_flags +resend_headers`, one PES per audio frame
/// (`-pes_payload_size 0`, keeps keyframe cuts A/V aligned), audio from `pipe_name`
/// (or a sine tone for `TestPattern`) when `sys_audio_format` is Some.
/// `TestPattern` is always encoded with libx264 so it runs without a GPU, and
/// its inputs are read at real time (`-re`).
pub fn build_engine_command(
    ffmpeg:           &Path,
    source:           CaptureSource,
    region:           &Region,
    enc:              &EncConfig,
    sys_audio_format: Option<&crate::audio::AudioFormat>,
    pipe_name:        &str,
) -> Vec<String> {
    let mut cmd = vec![ffmpeg.to_string_lossy().into_owned()];
    let fps = enc.fps;
    let has_audio = sys_audio_format.is_some();
    let encoder = if source == CaptureSource::TestPattern { "libx264" } else { enc.hw_encoder.as_str() };
    let needs_dl = encoder == "libx264" && source != CaptureSource::TestPattern;

    // Audio input first (index 0), video second.
    if let Some(f) = sys_audio_format {
        if source == CaptureSource::TestPattern {
            cmd.extend(["-re".into(), "-f".into(), "lavfi".into(), "-i".into(), "sine=f=440".into()]);
        } else {
            cmd.extend([
                "-f".into(), "f32le".into(),
                "-ar".into(), f.sample_rate.to_string(),
                "-ac".into(), f.channels.to_string(),
                "-thread_queue_size".into(), "4096".into(),
                "-i".into(), pipe_name.to_string(),
            ]);
        }
    }

    let input = match source {
        CaptureSource::Ddagrab => format!(
            "ddagrab=output_idx={}:framerate={}:draw_mouse=1", region.monitor, fps),
        CaptureSource::Gfxcapture => format!(
            "gfxcapture=monitor_idx={}:max_framerate={}:capture_cursor=1", region.monitor, fps),
        CaptureSource::TestPattern => format!(
            "testsrc2=s={}x{}:r={}", region.width, region.height, fps),
    };
    // lavfi test sources run as fast as the encoder allows; `-re` paces them
    // at real time like a capture device (the ring and recordings need that).
    if source == CaptureSource::TestPattern {
        cmd.push("-re".into());
    }
    cmd.extend(["-f".into(), "lavfi".into(), "-i".into(), input]);

    let vid = usize::from(has_audio);
    cmd.extend(["-map".into(), format!("{vid}:v")]);
    if needs_dl {
        cmd.extend(["-vf".into(), "hwdownload,format=bgra".into()]);
    }
    if has_audio {
        cmd.extend(["-map".into(), "0:a".into()]);
    }

    add_video_encoder_args(&mut cmd, encoder, enc.quality_crf, region.width);
    cmd.extend(["-g".into(), fps.to_string(), "-keyint_min".into(), fps.to_string()]);

    if has_audio {
        cmd.extend([
            "-af".into(), "aresample=async=1000".into(),
            "-c:a".into(), "aac".into(), "-b:a".into(), "128k".into(),
        ]);
    } else {
        cmd.push("-an".into());
    }

    // One PES per audio frame: the default packs ~0.16 s of AAC per PES, so a
    // clip cut at a keyframe would start with up to that much earlier audio.
    cmd.extend([
        "-pes_payload_size".into(), "0".into(),
        "-mpegts_flags".into(), "+resend_headers".into(),
        "-f".into(), "mpegts".into(), "pipe:1".into(),
    ]);
    cmd
}

/// `-f concat -safe 0 -i <list> -c copy -movflags +faststart <out>`
pub fn build_concat_remux_command(ffmpeg: &Path, list_file: &Path, out: &Path) -> Vec<String> {
    vec![
        ffmpeg.to_string_lossy().into_owned(), "-y".into(),
        "-f".into(), "concat".into(), "-safe".into(), "0".into(),
        "-i".into(), list_file.to_string_lossy().into_owned(),
        "-c".into(), "copy".into(), "-movflags".into(), "+faststart".into(),
        out.to_string_lossy().into_owned(),
    ]
}

/// Trim/export: Mp4Copy = stream copy; Webm = libvpx-vp9 + libopus;
/// Gif = fps=15,scale=640:-1 palette (split/palettegen/paletteuse).
pub fn build_export_command(
    ffmpeg:  &Path,
    input:   &Path,
    start_s: f64,
    end_s:   f64,
    fmt:     ExportFormat,
    out:     &Path,
) -> Vec<String> {
    let mut cmd = vec![
        ffmpeg.to_string_lossy().into_owned(), "-y".into(),
        "-ss".into(), format!("{start_s:.3}"),
        "-to".into(), format!("{end_s:.3}"),
        "-i".into(), input.to_string_lossy().into_owned(),
    ];
    match fmt {
        ExportFormat::Mp4Copy => cmd.extend(["-c".into(), "copy".into()]),
        ExportFormat::Webm => cmd.extend([
            "-c:v".into(), "libvpx-vp9".into(), "-b:v".into(), "0".into(), "-crf".into(), "32".into(),
            "-c:a".into(), "libopus".into(), "-b:a".into(), "128k".into(),
        ]),
        ExportFormat::Gif => cmd.extend([
            "-vf".into(),
            "fps=15,scale=640:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse".into(),
            "-an".into(),
        ]),
    }
    cmd.push(out.to_string_lossy().into_owned());
    cmd
}


#[cfg(test)]
#[path = "ffmpeg_tests.rs"]
mod tests;
