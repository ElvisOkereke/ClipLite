//! `hil` — hardware-in-the-loop runner for ClipLite.
//!
//!   hil matrix   --app <cliplite.exe> [--trials 5] [--elevated] --out <dir>
//!   hil self-test [--ffmpeg-dir <dir>]
//!   hil elevated-helper --pipe <name>        (launched by `matrix --elevated`)
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "hil", about = "Hardware-in-the-loop test runner for ClipLite")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Fullscreen hotkey matrix: 12 cells × N trials.
    Matrix {
        #[arg(long)]
        app: PathBuf,
        #[arg(long, default_value_t = 5)]
        trials: usize,
        /// Run the fake game elevated (one UAC prompt).
        #[arg(long)]
        elevated: bool,
        #[arg(long, default_value = cliplite_testkit::DEFAULT_CHORD)]
        hotkey: String,
        /// Replay buffer length; the UI only accepts 60/120/180/300.
        #[arg(long, default_value_t = 60)]
        buffer_secs: u32,
        #[arg(long)]
        out: PathBuf,
    },
    /// Check the clip validator against generated black / test-pattern clips.
    SelfTest {
        #[arg(long)]
        ffmpeg_dir: Option<PathBuf>,
    },
    /// Internal: elevated input/focus helper (started via UAC by `matrix --elevated`).
    ElevatedHelper {
        #[arg(long)]
        pipe: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    #[cfg(windows)]
    {
        win::run(cli.cmd)
    }
    #[cfg(not(windows))]
    {
        let _ = cli;
        anyhow::bail!("hil requires Windows")
    }
}

#[cfg(windows)]
mod win {
    use super::Cmd;
    use anyhow::{Context, Result};
    use cliplite_testkit::clip;
    use cliplite_testkit::elevated::{serve, sibling_exe, Helper};
    use cliplite_testkit::harness::{ensure_foreground, launch_game, tools_in, wait_new_clips, Game, Harness};
    use cliplite_testkit::job::Job;
    use cliplite_testkit::matrix::{cells, evaluate_trial, summary_markdown, CpuLoad, TrialResult};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    pub fn run(cmd: Cmd) -> Result<()> {
        match cmd {
            Cmd::Matrix { app, trials, elevated, hotkey, buffer_secs, out } => matrix(&app, trials, elevated, &hotkey, buffer_secs, &out),
            Cmd::SelfTest { ffmpeg_dir } => {
                let dir = match ffmpeg_dir {
                    Some(d) => d,
                    None => default_ffmpeg_dir()?,
                };
                clip::self_test(&dir)?;
                println!("self-test PASS (validator flags black clips, accepts test pattern)");
                Ok(())
            }
            Cmd::ElevatedHelper { pipe } => serve(&pipe, &sibling_exe("fakegame.exe")?),
        }
    }

    fn default_ffmpeg_dir() -> Result<PathBuf> {
        let me = std::env::current_exe()?;
        let bin = me.parent().unwrap().join("bin");
        tools_in(&bin).map(|_| bin)
    }

    enum Running {
        Local(Game),
        Elevated(u32),
    }

    pub fn machine_info() -> serde_json::Value {
        let q = |cls: &str| {
            std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &format!("(Get-CimInstance {cls}).Name -join '; '")])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        };
        serde_json::json!({ "gpu": q("Win32_VideoController"), "cpu": q("Win32_Processor") })
    }

    fn matrix(app: &Path, trials: usize, elevated: bool, chord: &str, buffer_secs: u32, out: &Path) -> Result<()> {
        std::fs::create_dir_all(out)?;
        let home = out.join("home");
        if home.exists() {
            std::fs::remove_dir_all(&home).context("clear previous harness home")?;
        }
        let fakegame = sibling_exe("fakegame.exe")?;
        let started_at = chrono::Local::now().to_rfc3339();
        println!("starting ClipLite in test mode: {}", app.display());
        let harness = Harness::start(app, out, buffer_secs, chord)?;
        let (ffmpeg, ffprobe) = harness.ffmpeg_tools()?;
        let game_job = Job::new_kill_on_close(false)?;
        let mut helper = if elevated {
            println!("approve the UAC prompt for the elevated helper…");
            Some(Helper::launch(&std::env::current_exe()?)?)
        } else {
            None
        };
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);

        let mut cells = cells(elevated);
        for cell in cells.iter_mut() {
            harness.set_window(cell.window)?;
            for t in 0..trials {
                let before = harness.list_clips();
                let cpu = match cell.cpu {
                    CpuLoad::Idle => 0,
                    CpuLoad::Saturated => threads,
                };
                let args: Vec<String> = ["--mode", cell.mode, "--gpu-load", "50", "--cpu-threads", &cpu.to_string()]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();

                let (running, fullscreen, focused) = match helper.as_mut() {
                    Some(h) => {
                        let (pid, _hwnd, fs, focused) = h.launch_fakegame(args)?;
                        (Running::Elevated(pid), fs, focused)
                    }
                    None => {
                        let g = launch_game(&game_job, &fakegame, &args)?;
                        let focused = ensure_foreground(g.hwnd);
                        let fs = g.fullscreen;
                        (Running::Local(g), fs, focused)
                    }
                };

                let mut result = if !focused {
                    TrialResult::failed("no_focus")
                } else {
                    std::thread::sleep(Duration::from_secs(5));
                    let sent = match helper.as_mut() {
                        Some(h) => h.send_chord(chord),
                        None => cliplite_testkit::input::send_chord(chord),
                    };
                    if sent.is_err() {
                        TrialResult::failed("input_blocked")
                    } else {
                        let (new, latency) = wait_new_clips(&harness.clips, &before, Duration::from_secs(15));
                        let report = if new.len() == 1 { clip::validate(&ffprobe, &ffmpeg, &new[0]).ok() } else { None };
                        let mut r = evaluate_trial(&new, report.as_ref(), 3.0);
                        r.latency_ms = latency;
                        r
                    }
                };
                result.fullscreen = Some(fullscreen);

                match running {
                    Running::Local(g) => g.close(),
                    Running::Elevated(pid) => {
                        let _ = helper.as_mut().map(|h| h.close(pid));
                    }
                }
                println!(
                    "{:<10} {:<8} {:<9} trial {}/{}: {}",
                    cell.mode,
                    format!("{:?}", cell.window),
                    format!("{:?}", cell.cpu),
                    t + 1,
                    trials,
                    if result.ok { format!("ok ({:?} ms, {:.1}s clip)", result.latency_ms.unwrap_or(0), result.duration_s.unwrap_or(0.0)) } else { result.failure.unwrap_or("?").to_string() }
                );
                cell.trials.push(result);
                std::thread::sleep(Duration::from_secs(2));
            }
        }
        drop(helper);
        drop(harness);

        let report = serde_json::json!({
            "build": { "app_path": app, "git_sha": std::env::var("GIT_SHA").unwrap_or_else(|_| "unknown".into()) },
            "machine": machine_info(),
            "started_at": started_at,
            "hotkey": chord,
            "trials_per_cell": trials,
            "cells": cells,
        });
        std::fs::write(out.join("matrix.json"), serde_json::to_vec_pretty(&report)?)?;
        let md = summary_markdown(&cells);
        std::fs::write(out.join("summary.md"), &md)?;
        println!("\n{md}");
        Ok(())
    }
}
