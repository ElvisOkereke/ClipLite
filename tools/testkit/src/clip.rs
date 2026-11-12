//! Clip validation with ffprobe/ffmpeg: duration, streams, and black frames.
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ClipReport {
    pub duration_s: f64,
    pub has_video: bool,
    pub has_audio: bool,
    /// blackdetect covers ≥ 90% of the duration (capture saw nothing).
    pub black: bool,
}

fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// Sum of `black_duration:` values in ffmpeg blackdetect stderr output.
pub fn total_black_seconds(stderr: &str) -> f64 {
    stderr
        .split_whitespace()
        .filter_map(|tok| tok.strip_prefix("black_duration:"))
        .filter_map(|v| v.parse::<f64>().ok())
        .sum()
}

pub fn validate(ffprobe: &Path, ffmpeg: &Path, file: &Path) -> Result<ClipReport> {
    let out = no_window(&mut Command::new(ffprobe))
        .args(["-v", "error", "-show_entries", "format=duration:stream=codec_type", "-of", "json"])
        .arg(file)
        .output()
        .context("run ffprobe")?;
    if !out.status.success() {
        return Err(anyhow!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let duration_s = v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0);
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let has = |kind: &str| streams.iter().any(|s| s["codec_type"] == kind);

    let bd = no_window(&mut Command::new(ffmpeg))
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(file)
        .args(["-vf", "blackdetect=d=0.5:pix_th=0.10", "-an", "-f", "null", "-"])
        .output()
        .context("run ffmpeg blackdetect")?;
    let black_s = total_black_seconds(&String::from_utf8_lossy(&bd.stderr));

    Ok(ClipReport {
        duration_s,
        has_video: has("video"),
        has_audio: has("audio"),
        black: duration_s > 0.0 && black_s >= 0.9 * duration_s,
    })
}

/// Generate a black clip and a test-pattern clip (paths contain a space) and
/// check the validator classifies both correctly.
pub fn self_test(ffmpeg_dir: &Path) -> Result<()> {
    let (ffmpeg, ffprobe) = (ffmpeg_dir.join("ffmpeg.exe"), ffmpeg_dir.join("ffprobe.exe"));
    let dir = std::env::temp_dir().join("clip lite validator test");
    std::fs::create_dir_all(&dir)?;
    for (name, src, want_black) in [("black clip.mp4", "color=black:s=320x240:d=3", true), ("test clip.mp4", "testsrc2=s=320x240:d=3", false)] {
        let f = dir.join(name);
        let st = no_window(&mut Command::new(&ffmpeg))
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", src, "-pix_fmt", "yuv420p"])
            .arg(&f)
            .status()?;
        if !st.success() {
            return Err(anyhow!("ffmpeg failed generating {name}"));
        }
        let r = validate(&ffprobe, &ffmpeg, &f)?;
        if !r.has_video || (r.duration_s - 3.0).abs() > 0.2 || r.black != want_black {
            return Err(anyhow!("{name}: unexpected report {r:?} (want black={want_black})"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_black_durations() {
        let s = "[blackdetect @ 0] black_start:0 black_end:1.5 black_duration:1.5\n\
                 [blackdetect @ 0] black_start:2 black_end:3.25 black_duration:1.25";
        assert!((total_black_seconds(s) - 2.75).abs() < 1e-9);
        assert_eq!(total_black_seconds("no matches here"), 0.0);
    }

    /// Needs ffmpeg/ffprobe; run via `hil self-test`, or set CLIPLITE_FFMPEG_DIR.
    #[test]
    #[ignore = "needs ffmpeg; run in `hil self-test`"]
    fn validator_detects_black() {
        let bin = std::path::PathBuf::from(std::env::var("CLIPLITE_FFMPEG_DIR").expect("CLIPLITE_FFMPEG_DIR"));
        self_test(&bin).unwrap();
    }
}
