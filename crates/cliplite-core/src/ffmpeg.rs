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

// ── Process spawning ──────────────────────────────────────────────────────────

// ── Command builder (recorder process) ────────────────────────────────────────
use crate::ipc::{AudioConfig, EncConfig, Region};

// legacy: removed in WS2 Task 6
pub fn build_record_command(
    ffmpeg:           &Path,
    region:           &Region,
    audio_cfg:        &AudioConfig,
    enc_cfg:          &EncConfig,
    output_path:      &str,
    sys_audio_format: Option<&crate::audio::AudioFormat>,
) -> Vec<String> {
    build_record_command_with_pipe(ffmpeg, region, audio_cfg, enc_cfg, output_path, sys_audio_format, r"\\.\pipe\cliplite_sysaudio")
}

// legacy: removed in WS2 Task 6
pub fn build_record_command_with_pipe(
    ffmpeg:           &Path,
    region:           &Region,
    audio_cfg:        &AudioConfig,
    enc_cfg:          &EncConfig,
    output_path:      &str,
    sys_audio_format: Option<&crate::audio::AudioFormat>,
    pipe_name:        &str,
) -> Vec<String> {
    let mut cmd = vec![ffmpeg.to_string_lossy().into_owned(), "-y".into()];

    let enc     = enc_cfg.hw_encoder.as_str();
    let crf     = enc_cfg.quality_crf;
    let fmt     = enc_cfg.format.as_str();
    let fps     = enc_cfg.fps;
    // Audio now comes ONLY via the named pipe (WASAPI captured in Rust).
    // Both sys audio and mic are mixed in rec_audio.rs before the pipe.
    // has_audio = true when either sys OR mic (or both) is selected.
    let has_audio = audio_cfg.sys_device_id.is_some()
        || audio_cfg.mic_device_id.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
        || audio_cfg.mic_device.as_ref().map(|s| !s.is_empty()).unwrap_or(false);
    let has_sys   = has_audio; // pipe carries all audio
    let has_mic   = false;     // mic is mixed into pipe, not a separate FFmpeg input
    let n_audio   = usize::from(has_audio);
    let needs_dl  = enc == "libx264";

    // System audio pipe
    #[cfg(windows)]
    if has_sys {
        let rate = sys_audio_format.map(|f| f.sample_rate).unwrap_or(48000);
        let ch   = sys_audio_format.map(|f| f.channels as u32).unwrap_or(2);
        cmd.extend([
            "-f".into(), "f32le".into(),
            "-ar".into(), rate.to_string(),
            "-ac".into(), ch.to_string(),
            "-thread_queue_size".into(), "4096".into(),
            "-i".into(), pipe_name.to_string(),
        ]);
    }

    // Video input
    add_video_input(&mut cmd, region, fps);

    // Microphone is captured in Rust and mixed into the named pipe.
    // No dshow mic input here — that caused D3D stalls.

    // Stream mapping
    #[cfg(windows)]
    {
        let vid = if has_sys { 1usize } else { 0 };
        let mic = if has_sys { 2usize } else { 1 };
        let dl  = "hwdownload,format=bgra";

        if n_audio == 2 {
            let fc = if needs_dl {
                format!("[{vid}:v]{dl}[vout];[0:a][{mic}:a]amix=inputs=2:duration=first:dropout_transition=0[aout]")
            } else {
                format!("[0:a][{mic}:a]amix=inputs=2:duration=first:dropout_transition=0[aout]")
            };
            cmd.extend(["-filter_complex".into(), fc]);
            if needs_dl {
                cmd.extend(["-map".into(), "[vout]".into(), "-map".into(), "[aout]".into()]);
            } else {
                cmd.extend(["-map".into(), format!("{vid}:v"), "-map".into(), "[aout]".into()]);
            }
        } else if has_sys {
            if needs_dl {
                cmd.extend(["-map".into(), "1:v".into(), "-vf".into(), dl.into(), "-map".into(), "0:a".into()]);
            } else {
                cmd.extend(["-map".into(), "1:v".into(), "-map".into(), "0:a".into()]);
            }
        } else if has_mic {
            if needs_dl {
                cmd.extend(["-map".into(), format!("{vid}:v"), "-vf".into(), dl.into(), "-map".into(), format!("{mic}:a")]);
            } else {
                cmd.extend(["-map".into(), format!("{vid}:v"), "-map".into(), format!("{mic}:a")]);
            }
        } else {
            if needs_dl {
                cmd.extend(["-map".into(), format!("{vid}:v"), "-vf".into(), dl.into()]);
            } else {
                cmd.extend(["-map".into(), format!("{vid}:v")]);
            }
        }
    }
    #[cfg(not(windows))]
    {
        if n_audio == 2 {
            cmd.extend([
                "-filter_complex".into(),
                "[0:a][1:a]amix=inputs=2:duration=first:dropout_transition=0[aout]".into(),
                "-map".into(), "0:v".into(), "-map".into(), "[aout]".into(),
            ]);
        } else if n_audio == 1 {
            cmd.extend(["-map".into(), "0:v".into(), "-map".into(), "1:a".into()]);
        } else {
            cmd.extend(["-map".into(), "0:v".into()]);
        }
    }

    // Video encoder
    add_video_encoder_args(&mut cmd, enc, crf, region.width);

    // Audio encoder + sync correction
    if n_audio > 0 && fmt != "gif" {
        // aresample=async=1000: allows FFmpeg to stretch/compress audio by up to
        // 1000 samples/sec to keep A/V in sync. Fixes progressive desync caused by
        // the ~22ms audio-leads-video gap baked in at pipe startup.
        cmd.extend(["-af".into(), "aresample=async=1000".into()]);
        if fmt == "webm" {
            cmd.extend(["-c:a".into(), "libopus".into(), "-b:a".into(), "128k".into()]);
        } else {
            cmd.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "128k".into()]);
        }
    } else {
        cmd.push("-an".into());
    }

    if fmt == "mp4" { cmd.extend(["-movflags".into(), "+faststart".into()]); }
    cmd.push(output_path.into());
    cmd
}

/// Per-encoder video arguments (`-c:v <enc>` + rate control), shared by the
/// legacy record builder and the engine builder.
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

// -- WS2 engine / remux / export builders ------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSource { Gfxcapture, Ddagrab, TestPattern }

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat { Mp4Copy, Webm, Gif }

/// Capture + encode -> MPEG-TS on FFmpeg's stdout (`-f mpegts pipe:1`), GOP = fps
/// (1 s keyframes), `-mpegts_flags +resend_headers`, audio from `pipe_name`
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

    // Audio input first (index 0), video second, as in the legacy builder.
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

    cmd.extend([
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

fn add_video_input(cmd: &mut Vec<String>, r: &Region, fps: u32) {
    #[cfg(windows)]
    cmd.extend([
        "-f".into(), "lavfi".into(),
        "-i".into(), format!("ddagrab=output_idx={}:framerate={}:draw_mouse=1", r.monitor, fps),
    ]);
    #[cfg(target_os = "linux")]
    {
        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into());
        cmd.extend(["-f".into(), "x11grab".into(), "-r".into(), fps.to_string(),
                    "-s".into(), format!("{}x{}", r.width, r.height),
                    "-i".into(), format!("{}+{},{}", display, r.x, r.y)]);
    }
    #[cfg(target_os = "macos")]
    cmd.extend(["-f".into(), "avfoundation".into(), "-framerate".into(), fps.to_string(),
                "-i".into(), format!("{}:none", r.monitor),
                "-vf".into(), format!("crop={}:{}:{}:{}", r.width, r.height, r.x, r.y)]);
}


#[cfg(test)]
#[path = "ffmpeg_tests.rs"]
mod tests;
