//! `hil` — hardware-in-the-loop runner for ClipLite.
//!
//!   hil matrix   --app <cliplite.exe> [--trials 5] [--elevated] --out <dir>
//!   hil soak     --app <cliplite.exe> [--hours 2] [--save-every-mins 10] --out <dir>
//!   hil bench    --app <cliplite.exe> [--runs 5] [--duration-secs 60] --out <dir>
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
    /// Long run: replay buffer + fake game; sample memory/disk, save periodically.
    Soak {
        #[arg(long)]
        app: PathBuf,
        #[arg(long, default_value_t = 2.0)]
        hours: f64,
        #[arg(long, default_value_t = 10.0)]
        save_every_mins: f64,
        #[arg(long, default_value_t = 5)]
        sample_every_secs: u64,
        #[arg(long, default_value_t = 10.0)]
        warmup_mins: f64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Frame times with ClipLite off / replay buffer / replay + recording.
    Bench {
        #[arg(long)]
        app: PathBuf,
        #[arg(long, default_value_t = 5)]
        runs: usize,
        #[arg(long, default_value_t = 60)]
        duration_secs: u64,
        #[arg(long, default_value = "exclusive")]
        mode: String,
        #[arg(long, default_value_t = 100)]
        gpu_load: u8,
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
    use cliplite_testkit::harness::{
        ensure_foreground, launch_game, list_mp4, tools_in, wait_new_clips, Game, Harness, DEFAULT_CHORD, RECORD_START_CHORD,
        RECORD_STOP_CHORD,
    };
    use cliplite_testkit::{bench, procmon, soak};
    use std::collections::HashMap;
    use std::io::Write;
    use std::time::Instant;
    use cliplite_testkit::job::Job;
    use cliplite_testkit::matrix::{cells, evaluate_trial, summary_markdown, CpuLoad, TrialResult};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    pub fn run(cmd: Cmd) -> Result<()> {
        match cmd {
            Cmd::Matrix { app, trials, elevated, hotkey, buffer_secs, out } => matrix(&app, trials, elevated, &hotkey, buffer_secs, &out),
            Cmd::Soak { app, hours, save_every_mins, sample_every_secs, warmup_mins, out } => {
                soak(&app, hours, save_every_mins, sample_every_secs, warmup_mins, &out)
            }
            Cmd::Bench { app, runs, duration_secs, mode, gpu_load, out } => bench(&app, runs, duration_secs, &mode, gpu_load, &out),
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

    const APP_NAMES: [&str; 3] = ["cliplite.exe", "cliplite-recorder.exe", "ffmpeg.exe"];
    /// Matches `max_replay_buffer_size_mb` written by `harness::test_settings`.
    const CAP_MB: u32 = 500;

    fn app_dir_of(app: &Path) -> Result<PathBuf> {
        Ok(app.canonicalize().with_context(|| format!("app not found: {}", app.display()))?.parent().unwrap().to_path_buf())
    }

    fn refuse_if_foreign_capture(app: &Path) -> Result<()> {
        let foreign = procmon::foreign_capture(&app_dir_of(app)?);
        if !foreign.is_empty() {
            anyhow::bail!(
                "another ClipLite is capturing and would skew the numbers — stop its replay buffer/recording first:\n  {}",
                foreign.join("\n  ")
            );
        }
        Ok(())
    }

    fn fresh_dir(dir: &Path) -> Result<()> {
        if dir.exists() {
            std::fs::remove_dir_all(dir).with_context(|| format!("clear {}", dir.display()))?;
        }
        std::fs::create_dir_all(dir)?;
        Ok(())
    }

    fn csv(path: &Path, header: &str) -> Result<std::io::BufWriter<std::fs::File>> {
        let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
        writeln!(w, "{header}")?;
        Ok(w)
    }

    fn soak(app: &Path, hours: f64, save_every_mins: f64, sample_every_secs: u64, warmup_mins: f64, out: &Path) -> Result<()> {
        refuse_if_foreign_capture(app)?;
        fresh_dir(out)?;
        let fakegame = sibling_exe("fakegame.exe")?;
        let started_at = chrono::Local::now().to_rfc3339();
        let harness = Harness::start(app, out, 60, DEFAULT_CHORD)?;
        let (ffmpeg, ffprobe) = harness.ffmpeg_tools()?;
        let job = Job::new_kill_on_close(false)?;
        let game_args: Vec<String> = ["--mode", "borderless", "--gpu-load", "50"].iter().map(|s| s.to_string()).collect();
        let mut game = launch_game(&job, &fakegame, &game_args)?;

        let mut procs = csv(&out.join("procs.csv"), "t_s,name,pid,private_bytes,working_set,handles,cpu_time_ms,write_bytes")?;
        let mut disk = csv(&out.join("disk.csv"), "t_s,temp_dir_bytes")?;
        let mut saves_csv = csv(&out.join("saves.csv"), "t_s,ok,failure,latency_ms,duration_s,file")?;
        let (total_s, save_s, sample_s) = (hours * 3600.0, save_every_mins * 60.0, sample_every_secs as f64);
        let (mut next_sample, mut next_save) = (0.0, save_s);
        let mut totals: Vec<(f64, u64)> = Vec::new();
        let mut temp_max = 0u64;
        let mut saves = Vec::new();
        let t0 = Instant::now();
        println!("soak: {hours} h, save every {save_every_mins} min, sample every {sample_every_secs} s");

        while t0.elapsed().as_secs_f64() < total_s {
            let t = t0.elapsed().as_secs_f64();
            if let Ok(Some(_)) = game.child.try_wait() {
                println!("{t:>7.0}s fake game exited unexpectedly; relaunching");
                game = launch_game(&job, &fakegame, &game_args)?;
            }
            if t >= next_sample {
                let samples = procmon::sample(&APP_NAMES, &harness.app_dir);
                let mut total = 0u64;
                for p in &samples {
                    writeln!(procs, "{t:.1},{},{},{},{},{},{},{}", p.name, p.pid, p.private_bytes, p.working_set, p.handles, p.cpu_time_ms, p.write_bytes)?;
                    total += p.private_bytes;
                }
                totals.push((t, total));
                let temp = procmon::dir_size(&harness.home.join("temp"));
                temp_max = temp_max.max(temp);
                writeln!(disk, "{t:.1},{temp}")?;
                next_sample += sample_s;
            }
            if t >= next_save {
                ensure_foreground(game.hwnd);
                let before = harness.list_clips();
                let r = match cliplite_testkit::input::send_chord(&harness.chord) {
                    Err(_) => TrialResult::failed("input_blocked"),
                    Ok(()) => {
                        let (new, latency) = wait_new_clips(&harness.clips, &before, Duration::from_secs(20));
                        let report = if new.len() == 1 { clip::validate(&ffprobe, &ffmpeg, &new[0]).ok() } else { None };
                        let mut r = evaluate_trial(&new, report.as_ref(), 3.0);
                        r.latency_ms = latency;
                        r
                    }
                };
                writeln!(
                    saves_csv,
                    "{t:.1},{},{},{},{},{}",
                    r.ok,
                    r.failure.unwrap_or(""),
                    r.latency_ms.map(|v| v.to_string()).unwrap_or_default(),
                    r.duration_s.map(|v| format!("{v:.2}")).unwrap_or_default(),
                    r.file.as_ref().map(|f| f.display().to_string()).unwrap_or_default()
                )?;
                println!(
                    "{t:>7.0}s save: {} | private {:.0} MB | temp {:.0} MB",
                    if r.ok { format!("ok {:.1}s clip", r.duration_s.unwrap_or(0.0)) } else { r.failure.unwrap_or("?").into() },
                    totals.last().map(|x| x.1 as f64 / 1048576.0).unwrap_or(0.0),
                    temp_max as f64 / 1048576.0
                );
                saves.push(r);
                procs.flush()?;
                disk.flush()?;
                saves_csv.flush()?;
                next_save += save_s;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        game.close();
        drop(harness);
        procs.flush()?;
        disk.flush()?;
        saves_csv.flush()?;

        let v = soak::verdict(&totals, temp_max, CAP_MB, &saves, warmup_mins * 60.0);
        let report = serde_json::json!({
            "build": { "app_path": app, "git_sha": std::env::var("GIT_SHA").unwrap_or_else(|_| "unknown".into()) },
            "machine": machine_info(),
            "started_at": started_at,
            "params": { "hours": hours, "save_every_mins": save_every_mins, "sample_every_secs": sample_every_secs, "warmup_mins": warmup_mins, "cap_mb": CAP_MB },
            "verdict": v,
        });
        std::fs::write(out.join("soak.json"), serde_json::to_vec_pretty(&report)?)?;
        println!("\nsoak verdict: {}", serde_json::to_string_pretty(&v)?);
        Ok(())
    }

    fn read_frametimes(path: &Path, skip_ms: f64) -> Result<Vec<f64>> {
        let text = std::fs::read_to_string(path)?;
        Ok(text
            .lines()
            .skip(1)
            .filter_map(|l| {
                let mut it = l.split(',');
                let _frame = it.next()?;
                let t: f64 = it.next()?.parse().ok()?;
                let ft: f64 = it.next()?.parse().ok()?;
                (t >= skip_ms).then_some(ft)
            })
            .collect())
    }

    fn gzip_and_remove(path: &Path) -> Result<()> {
        let gz = path.with_extension("csv.gz");
        let mut enc = flate2::write::GzEncoder::new(std::fs::File::create(&gz)?, flate2::Compression::default());
        enc.write_all(&std::fs::read(path)?)?;
        enc.finish()?;
        std::fs::remove_file(path)?;
        Ok(())
    }

    /// Wait (≤ 10 s) for a new .mp4 in `dir` that is growing — i.e. a recording started.
    fn recording_started(dir: &Path, before: &[PathBuf]) -> bool {
        let t0 = Instant::now();
        let mut last: Option<(PathBuf, u64)> = None;
        while t0.elapsed() < Duration::from_secs(10) {
            if let Some(f) = list_mp4(dir).into_iter().find(|p| !before.contains(p)) {
                let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
                if let Some((lf, ls)) = &last {
                    if *lf == f && size > *ls {
                        return true;
                    }
                }
                last = Some((f, size));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        false
    }

    fn bench(app: &Path, runs: usize, duration_secs: u64, mode: &str, gpu_load: u8, out: &Path) -> Result<()> {
        refuse_if_foreign_capture(app)?;
        fresh_dir(out)?;
        let app_dir = app_dir_of(app)?;
        let fakegame = sibling_exe("fakegame.exe")?;
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
        let job = Job::new_kill_on_close(false)?;
        let started_at = chrono::Local::now().to_rfc3339();
        let mut scenarios = serde_json::Map::new();

        for scenario in ["baseline", "replay", "replay_record"] {
            let harness = if scenario == "baseline" { None } else { Some(Harness::start(app, &out.join(scenario), 60, DEFAULT_CHORD)?) };
            let mut results = Vec::new();
            for k in 1..=runs {
                let run_dir = out.join(scenario).join(format!("run{k}"));
                std::fs::create_dir_all(&run_dir)?;
                let mut note = None;
                if scenario == "replay_record" {
                    let h = harness.as_ref().unwrap();
                    let before = h.list_clips();
                    cliplite_testkit::input::send_chord(RECORD_START_CHORD)?;
                    if !recording_started(&h.clips, &before) {
                        note = Some("recording did not start");
                    }
                }
                let ft_path = run_dir.join("frametimes.csv");
                let args: Vec<String> = vec![
                    "--mode".into(), mode.into(), "--gpu-load".into(), gpu_load.to_string(),
                    "--duration-secs".into(), duration_secs.to_string(), "--frametimes".into(), ft_path.display().to_string(),
                ];
                let mut game = launch_game(&job, &fakegame, &args)?;
                let mut procs = csv(&run_dir.join("procs.csv"), "t_s,name,pid,private_bytes,working_set,handles,cpu_time_ms,write_bytes")?;
                let mut cpu: HashMap<u32, (u64, u64)> = HashMap::new();
                let mut private_sum = Vec::new();
                let t0 = Instant::now();
                loop {
                    if let Ok(Some(_)) = game.child.try_wait() {
                        break;
                    }
                    if t0.elapsed() > Duration::from_secs(duration_secs + 30) {
                        let _ = game.child.kill();
                        note = Some("fake game overran");
                        break;
                    }
                    let t = t0.elapsed().as_secs_f64();
                    let samples = procmon::sample(&APP_NAMES, &app_dir);
                    let mut sum = 0u64;
                    for p in &samples {
                        writeln!(procs, "{t:.1},{},{},{},{},{},{},{}", p.name, p.pid, p.private_bytes, p.working_set, p.handles, p.cpu_time_ms, p.write_bytes)?;
                        cpu.entry(p.pid).and_modify(|e| e.1 = p.cpu_time_ms).or_insert((p.cpu_time_ms, p.cpu_time_ms));
                        sum += p.private_bytes;
                    }
                    private_sum.push(sum);
                    std::thread::sleep(Duration::from_secs(1));
                }
                let wall = t0.elapsed().as_secs_f64();
                procs.flush()?;
                if scenario == "replay_record" {
                    cliplite_testkit::input::send_chord(RECORD_STOP_CHORD)?;
                    std::thread::sleep(Duration::from_secs(5));
                }
                let stats = bench::summarize(&read_frametimes(&ft_path, 2000.0)?);
                gzip_and_remove(&ft_path)?;
                let cpu_ms: u64 = cpu.values().map(|(a, b)| b.saturating_sub(*a)).sum();
                let private_mb = if private_sum.is_empty() { 0.0 } else { private_sum.iter().sum::<u64>() as f64 / private_sum.len() as f64 / 1048576.0 };
                println!(
                    "{scenario:<14} run {k}/{runs}: {:.1} fps avg, {:.1} fps 1% low, p99 {:.2} ms | ClipLite CPU {:.1}% RAM {:.0} MB{}",
                    stats.avg_fps, stats.p1_low_fps, stats.p99_frametime_ms, bench::cpu_pct(cpu_ms, wall, cores), private_mb,
                    note.map(|n| format!(" [{n}]")).unwrap_or_default()
                );
                results.push(serde_json::json!({
                    "run": k,
                    "stats": stats,
                    "cliplite_cpu_pct": bench::cpu_pct(cpu_ms, wall, cores),
                    "cliplite_private_mb": private_mb,
                    "note": note,
                }));
                std::thread::sleep(Duration::from_secs(3));
            }
            drop(harness);
            scenarios.insert(scenario.into(), serde_json::Value::Array(results));
        }
        let report = serde_json::json!({
            "build": { "app_path": app, "git_sha": std::env::var("GIT_SHA").unwrap_or_else(|_| "unknown".into()) },
            "machine": machine_info(),
            "started_at": started_at,
            "params": { "runs": runs, "duration_secs": duration_secs, "mode": mode, "gpu_load": gpu_load, "warmup_skip_ms": 2000 },
            "scenarios": scenarios,
        });
        std::fs::write(out.join("bench.json"), serde_json::to_vec_pretty(&report)?)?;
        Ok(())
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
