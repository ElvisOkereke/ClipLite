//! Capture-source spike: does `gfxcapture` or `ddagrab` survive a game
//! entering exclusive fullscreen mid-capture? Pure helpers are unit-tested;
//! the trial runner (windows only) drives FFmpeg and the fake game.
use serde::{Deserialize, Serialize};

pub const TRIAL_SECS: u64 = 15;
pub const FPS: u32 = 30;
pub const GAME_START_SECS: u64 = 4;
pub const EXPECTED_FRAMES: u64 = TRIAL_SECS * FPS as u64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Trial {
    pub alive_at_end: bool,
    pub exit_code: Option<i32>,
    pub frames: u64,
    pub expected_frames: u64,
    pub stderr_tail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpikeReport {
    pub source: String,
    pub trials: Vec<Trial>,
}

/// Last `frame=N` value from FFmpeg `-progress` output.
pub fn last_progress_frame(text: &str) -> Option<u64> {
    text.lines().rev().find_map(|l| l.trim().strip_prefix("frame=").and_then(|v| v.trim().parse().ok()))
}

/// Pass = still alive at the end AND at least 90% of the expected frames.
pub fn trial_passed(alive_at_end: bool, frames: u64, expected_frames: u64) -> bool {
    alive_at_end && frames * 10 >= expected_frames * 9
}

/// The lavfi source string for `-f lavfi -i`.
pub fn source_filter(source: &str, monitor: u32, fps: u32) -> String {
    match source {
        "gfxcapture" => format!("gfxcapture=monitor_idx={monitor}:max_framerate={fps}:capture_cursor=1"),
        _ => format!("ddagrab=output_idx={monitor}:framerate={fps}:draw_mouse=1"),
    }
}

/// Last `max` bytes of `s`, cut on a char boundary.
pub fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut i = s.len() - max;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    s[i..].to_string()
}

#[cfg(windows)]
pub use runner::run_trial;

#[cfg(windows)]
mod runner {
    use super::*;
    use crate::harness::launch_game;
    use crate::job::Job;
    use anyhow::Result;
    use std::io::Read;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// One trial: FFmpeg capture for 15 s; fake game (exclusive fullscreen)
    /// launched at t = 4 s. FFmpeg runs until we stop it, so "alive at the
    /// end" is a real observation rather than a `-t` self-exit.
    pub fn run_trial(job: &Job, ffmpeg: &Path, fakegame: &Path, source: &str, gpu_load: u8) -> Result<Trial> {
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-nostats", "-progress", "pipe:2", "-f", "lavfi", "-i"])
            .arg(source_filter(source, 0, FPS))
            .args(["-c:v", "h264_nvenc", "-f", "null", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = job.spawn(&mut cmd)?;
        let mut stderr = child.stderr.take().unwrap();
        let log = Arc::new(Mutex::new(String::new()));
        let reader = {
            let log = log.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    log.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            })
        };

        let t0 = Instant::now();
        std::thread::sleep(Duration::from_secs(GAME_START_SECS));
        let args: Vec<String> = ["--mode", "exclusive", "--gpu-load", &gpu_load.to_string()].iter().map(|s| s.to_string()).collect();
        let game = launch_game(job, fakegame, &args);
        let end = Duration::from_secs(TRIAL_SECS);
        while t0.elapsed() < end {
            if child.try_wait()?.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let (alive_at_end, exit_code) = match child.try_wait()? {
            None => {
                let _ = child.kill();
                let _ = child.wait();
                (true, None)
            }
            Some(st) => (false, st.code()),
        };
        let _ = reader.join();
        if let Ok(g) = game {
            g.close();
        }
        let text = log.lock().unwrap().clone();
        Ok(Trial {
            alive_at_end,
            exit_code,
            frames: last_progress_frame(&text).unwrap_or(0),
            expected_frames: EXPECTED_FRAMES,
            stderr_tail: tail(&text, 2000),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffmpeg_progress_frames() {
        assert_eq!(last_progress_frame("frame=12\nfps=30\nframe=431\nprogress=end\n"), Some(431));
        assert_eq!(last_progress_frame("nothing"), None);
    }
    #[test]
    fn trial_verdict() {
        assert!(trial_passed(true, 440, 450));
        assert!(!trial_passed(true, 300, 450)); // starved
        assert!(!trial_passed(false, 450, 450)); // died
    }
    #[test]
    fn source_filter_strings() {
        assert_eq!(source_filter("ddagrab", 0, 30), "ddagrab=output_idx=0:framerate=30:draw_mouse=1");
        assert_eq!(source_filter("gfxcapture", 0, 30), "gfxcapture=monitor_idx=0:max_framerate=30:capture_cursor=1");
    }
    #[test]
    fn tail_respects_char_boundary() {
        assert_eq!(tail("abc", 10), "abc");
        assert_eq!(tail("aébc", 3), "bc");
    }
}
