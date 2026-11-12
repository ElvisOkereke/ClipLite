//! Fake game: a D3D11 load generator used to reproduce in-game conditions
//! (exclusive fullscreen, GPU/CPU saturation) and to measure frame times.
//!
//! It is deliberately a plain test tool: an animated colour field with a
//! sweeping bar (so captured video is never static or black), titled
//! "ClipLite Test Load".

use std::io::Write;
use std::path::PathBuf;

#[cfg(windows)]
pub mod render;

pub const WINDOW_TITLE: &str = "ClipLite Test Load";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// 1280x720 decorated window.
    Windowed,
    /// Borderless popup covering the primary monitor.
    Borderless,
    /// DXGI exclusive fullscreen (legacy blt-model swap chain).
    Exclusive,
}

#[derive(Debug, Clone, clap::Parser)]
#[command(name = "fakegame", about = "D3D11 load generator for ClipLite HIL tests")]
pub struct Args {
    #[arg(long, value_enum)]
    pub mode: Mode,
    /// Backbuffer width (default: 1280 windowed, monitor width otherwise).
    #[arg(long)]
    pub width: Option<u32>,
    /// Backbuffer height (default: 720 windowed, monitor height otherwise).
    #[arg(long)]
    pub height: Option<u32>,
    /// Pixel-shader work per frame, 0-100.
    #[arg(long, default_value_t = 70, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub gpu_load: u8,
    /// Busy-spinning CPU threads.
    #[arg(long, default_value_t = 0)]
    pub cpu_threads: usize,
    /// Exit after this many seconds (0 = run until closed).
    #[arg(long, default_value_t = 0)]
    pub duration_secs: u64,
    /// Write per-frame times as CSV.
    #[arg(long)]
    pub frametimes: Option<PathBuf>,
    /// Present with vsync (default: uncapped, GPU-bound).
    #[arg(long)]
    pub vsync: bool,
}

/// Shader loop iterations for a GPU load percentage (quadratic so the low end
/// stays light). Measured on an RTX 3070 at 1440p exclusive: 50 ≈ 290 fps, 100 ≈ 80 fps (heavy game).
pub fn iterations_for(gpu_load: u8) -> u32 {
    let l = gpu_load.min(100) as u32;
    l * l * 2 / 5
}

/// CSV writer for present-to-present frame times.
pub struct FrametimeWriter<W: Write> {
    out: W,
}

impl<W: Write> FrametimeWriter<W> {
    pub fn new(mut out: W) -> std::io::Result<Self> {
        writeln!(out, "frame,t_ms,frametime_ms")?;
        Ok(Self { out })
    }

    pub fn record(&mut self, frame: u64, t_ms: f64, frametime_ms: f64) -> std::io::Result<()> {
        writeln!(self.out, "{frame},{t_ms:.3},{frametime_ms:.3}")
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_args_with_defaults() {
        let a = Args::parse_from(["fakegame", "--mode", "exclusive", "--cpu-threads", "4"]);
        assert_eq!(a.mode, Mode::Exclusive);
        assert_eq!(a.cpu_threads, 4);
        assert_eq!(a.gpu_load, 70);
        assert_eq!(a.duration_secs, 0);
        assert!(!a.vsync);
        assert!(a.frametimes.is_none());
    }

    #[test]
    fn rejects_gpu_load_over_100() {
        assert!(Args::try_parse_from(["fakegame", "--mode", "windowed", "--gpu-load", "101"]).is_err());
    }

    #[test]
    fn frametime_writer_header_and_rows() {
        let mut out = Vec::new();
        {
            let mut w = FrametimeWriter::new(&mut out).unwrap();
            w.record(0, 0.0, 0.0).unwrap();
            w.record(1, 16.7, 16.7).unwrap();
            w.record(2, 33.3, 16.6).unwrap();
        }
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "frame,t_ms,frametime_ms");
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[2], "1,16.700,16.700");
    }

    #[test]
    fn gpu_load_maps_to_shader_iterations() {
        assert_eq!(iterations_for(0), 0);
        assert!(iterations_for(50) > 0);
        assert!(iterations_for(100) > iterations_for(50));
    }
}
