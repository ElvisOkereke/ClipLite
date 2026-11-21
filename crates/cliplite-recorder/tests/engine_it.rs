//! End-to-end tests of the capture engine: drive the recorder binary over IPC
//! with the FFmpeg test pattern and check the clips it writes with ffprobe.
//!
//! FFmpeg comes from `CLIPLITE_FFMPEG`, else the copy `scripts/fetch-ffmpeg.ps1`
//! puts in `apps/desktop/src-tauri/bin/`. A missing FFmpeg fails the tests.
#![cfg(windows)]

use cliplite_core::ffmpeg::CaptureSource;
use cliplite_core::ipc::{AudioConfig, Command, EncConfig, EngineArgs, Envelope, Region};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::sleep;
use std::time::{Duration, Instant};

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn ffmpeg() -> PathBuf {
    let p = match std::env::var_os("CLIPLITE_FFMPEG") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/src-tauri/bin/ffmpeg.exe"),
    };
    assert!(
        p.exists(),
        "FFmpeg not found at {}: run scripts/fetch-ffmpeg.ps1 or set CLIPLITE_FFMPEG",
        p.display()
    );
    p
}

fn ffprobe() -> PathBuf {
    let p = ffmpeg().with_file_name("ffprobe.exe");
    assert!(p.exists(), "ffprobe not found next to FFmpeg at {}", p.display());
    p
}

fn engine_args(audio: bool) -> EngineArgs {
    EngineArgs {
        ffmpeg_path: ffmpeg().to_string_lossy().into_owned(),
        source: CaptureSource::TestPattern,
        region: Region { x: 0, y: 0, width: 640, height: 360, monitor: 0 },
        audio_cfg: AudioConfig {
            mic_device: None,
            sys_device_id: audio.then(|| "test-tone".to_string()),
            mic_device_id: None,
        },
        enc_cfg: EncConfig { fps: 30, quality_crf: 23, format: "mp4".into(), hw_encoder: "libx264".into() },
        audio_pipe: None,
    }
}

/// A recorder child process plus the events it has written to stdout.
struct Rec {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<Value>,
    unclaimed: Vec<Value>,
    home: PathBuf,
    next_cid: u32,
}

impl Rec {
    fn spawn(name: &str) -> Rec {
        let home = std::env::temp_dir()
            .join("cliplite engine it")
            .join(format!("{name} {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cliplite-recorder"))
            .env("CLIPLITE_HOME", &home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn recorder");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let v: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("recorder stdout line is not JSON ({e}): {line}"));
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        Rec { child, stdin, events, unclaimed: Vec::new(), home, next_cid: 0 }
    }

    fn out(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    fn send(&mut self, cmd: Command) -> String {
        self.next_cid += 1;
        let cid = format!("it{}", self.next_cid);
        let env = Envelope { body: cmd, cid: Some(cid.clone()) };
        writeln!(self.stdin, "{}", serde_json::to_string(&env).unwrap()).unwrap();
        self.stdin.flush().unwrap();
        cid
    }

    fn wait(&mut self, timeout: Duration, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.unclaimed.iter().position(&pred) {
            return self.unclaimed.remove(i);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(v) if pred(&v) => return v,
                Ok(v) => self.unclaimed.push(v),
                Err(_) => panic!("no {what} within {timeout:?}; other events: {:?}", self.unclaimed),
            }
        }
    }

    fn reply(&mut self, cid: &str, timeout: Duration) -> Value {
        self.wait(timeout, &format!("reply to {cid}"), |v| v["cid"] == cid)
    }

    fn request(&mut self, cmd: Command, timeout: Duration) -> Value {
        let cid = self.send(cmd);
        self.reply(&cid, timeout)
    }

    fn save(&mut self, secs: u32, out: &Path) -> String {
        self.send(Command::SaveReplay { secs, output_path: out.to_string_lossy().into_owned() })
    }

    fn status(&mut self) -> Value {
        let v = self.request(Command::Status, secs(5));
        expect_event(&v, "status");
        v
    }
}

impl Drop for Rec {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.home); // kept on failure for diagnosis
        }
    }
}

fn expect_event(v: &Value, event: &str) {
    assert_eq!(v["event"], event, "expected {event}, got {v}");
}

/// A recorder with a running engine: test pattern + tone, 30 s / 64 MB ring.
fn engine(name: &str) -> Rec {
    let mut r = Rec::spawn(name);
    let v = r.request(
        Command::StartEngine { args: engine_args(true), max_secs: 30, max_mb: 64 },
        secs(10),
    );
    expect_event(&v, "engine_started");
    r
}

/// (duration_s, has_video, has_audio, v_start, a_start)
fn probe(path: &Path) -> (f64, bool, bool, f64, f64) {
    assert!(path.exists(), "missing output {}", path.display());
    let out = std::process::Command::new(ffprobe())
        .args(["-v", "error", "-show_entries", "format=duration:stream=codec_type,start_time", "-of", "json"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed on {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let num = |v: &Value| v.as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(f64::NAN);
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let start_of = |kind: &str| streams.iter().find(|s| s["codec_type"] == kind).map(|s| num(&s["start_time"]));
    let (vs, as_) = (start_of("video"), start_of("audio"));
    eprintln!("probe {}: duration {}, video start {vs:?}, audio start {as_:?}", path.display(), v["format"]["duration"]);
    (num(&v["format"]["duration"]), vs.is_some(), as_.is_some(), vs.unwrap_or(f64::NAN), as_.unwrap_or(f64::NAN))
}

/// Probe a clip that must have video and audio in sync; returns its duration.
fn valid_av(path: &Path) -> f64 {
    let (d, has_v, has_a, vs, as_) = probe(path);
    assert!(has_v && has_a, "{}: video {has_v}, audio {has_a}", path.display());
    assert!((vs - as_).abs() < 0.1, "{}: video starts at {vs}, audio at {as_}", path.display());
    d
}

/// Terminate every `exe` process whose parent is `parent` (ToolHelp32 snapshot).
fn kill_children_named(parent: u32, exe: &str) -> usize {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    let mut killed = 0;
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).expect("process snapshot");
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snap, &mut e).is_ok();
        while more {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..len]);
            if e.th32ParentProcessID == parent && name.eq_ignore_ascii_case(exe) {
                if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, e.th32ProcessID) {
                    if TerminateProcess(h, 1).is_ok() {
                        killed += 1;
                    }
                    let _ = CloseHandle(h);
                }
            }
            more = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    killed
}

#[test]
fn save_produces_requested_duration() {
    let mut r = engine("save");
    sleep(secs(8));
    let out = r.out("out dir with spaces/new sub/clip.mp4");
    let cid = r.save(5, &out);
    let v = r.reply(&cid, secs(5));
    expect_event(&v, "replay_saved");
    assert_eq!(v["path"], out.to_string_lossy().as_ref());
    let reported = v["duration_s"].as_f64().expect("duration_s");
    assert!((5.0..=6.1).contains(&reported), "reported duration {reported}");
    let d = valid_av(&out);
    assert!((5.0..=6.1).contains(&d), "clip duration {d}");
}

#[test]
fn save_with_empty_ring_reports_error() {
    let mut r = engine("empty");
    let out = r.out("clip.mp4");
    let cid = r.save(5, &out);
    let v = r.reply(&cid, secs(5));
    expect_event(&v, "error");
    assert_eq!(v["message"], "replay buffer is empty");
    let cid = r.save(0, &out);
    let v = r.reply(&cid, secs(5));
    expect_event(&v, "error");
    assert_eq!(v["message"], "save duration must be at least 1 second");
    assert!(!out.exists());
}

#[test]
fn two_quick_saves_and_save_during_recording() {
    let mut r = engine("quick saves");
    sleep(secs(6));
    let rec = r.out("recordings/rec.mp4");
    let v = r.request(Command::StartRecording { output_path: rec.to_string_lossy().into_owned() }, secs(5));
    expect_event(&v, "recording_started");
    assert_eq!(v["path"], rec.to_string_lossy().as_ref());

    let (a, b) = (r.out("clips/a.mp4"), r.out("clips/b.mp4"));
    let cid_a = r.save(3, &a);
    sleep(secs(2));
    let cid_b = r.save(3, &b);
    expect_event(&r.reply(&cid_a, secs(5)), "replay_saved");
    expect_event(&r.reply(&cid_b, secs(5)), "replay_saved");
    for clip in [&a, &b] {
        let d = valid_av(clip);
        assert!((3.0..=4.1).contains(&d), "{}: duration {d}", clip.display());
    }

    sleep(secs(3));
    let v = r.request(Command::StopRecording, secs(10));
    expect_event(&v, "recording_stopped");
    assert_eq!(v["path"], rec.to_string_lossy().as_ref());
    let d = valid_av(&rec);
    assert!(d >= 4.5, "recording duration {d}");
    assert_ne!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap(), "the two saves are identical");
}

#[test]
fn pause_excludes_paused_time() {
    let mut r = engine("pause");
    let rec = r.out("rec.mp4");
    let v = r.request(Command::StartRecording { output_path: rec.to_string_lossy().into_owned() }, secs(5));
    expect_event(&v, "recording_started");
    sleep(secs(3));
    let v = r.request(Command::Pause, secs(5));
    expect_event(&v, "status");
    assert_eq!(v["paused"], true, "{v}");
    sleep(secs(3));
    let v = r.request(Command::Resume, secs(5));
    expect_event(&v, "status");
    assert_eq!(v["paused"], false, "{v}");
    sleep(secs(3));
    let s = r.status();
    let elapsed = s["elapsed"].as_f64().unwrap();
    assert!((5.5..=7.0).contains(&elapsed), "elapsed excludes the pause: {s}");
    assert_eq!(s["recording"], true);
    let v = r.request(Command::StopRecording, secs(10));
    expect_event(&v, "recording_stopped");
    let d = valid_av(&rec);
    assert!((5.0..=7.5).contains(&d), "recording duration {d}");
}

#[test]
fn capture_restart_keeps_saves_and_recordings_valid() {
    let mut r = engine("restart");
    let rec = r.out("rec.mp4");
    let v = r.request(Command::StartRecording { output_path: rec.to_string_lossy().into_owned() }, secs(5));
    expect_event(&v, "recording_started");
    sleep(secs(4));

    assert_eq!(kill_children_named(r.child.id(), "ffmpeg.exe"), 1, "exactly one engine ffmpeg to kill");
    let v = r.wait(secs(5), "engine_restarted", |v| v["event"] == "engine_restarted");
    assert_eq!(v["restarts"], 1, "{v}");
    assert!(v["cid"].as_str().is_some_and(|c| !c.is_empty()), "unsolicited event carries a fresh cid: {v}");

    sleep(secs(4));
    let clip = r.out("clip.mp4");
    let cid = r.save(6, &clip);
    let v = r.reply(&cid, secs(5));
    expect_event(&v, "replay_saved");
    let d = valid_av(&clip);
    assert!((5.5..=7.5).contains(&d), "clip spanning the restart: duration {d}");

    let v = r.request(Command::StopRecording, secs(10));
    expect_event(&v, "recording_stopped");
    let d = valid_av(&rec);
    assert!(d >= 6.0, "recording spanning the restart: duration {d}");
    assert_eq!(r.status()["restarts"], 1);
}

#[test]
fn memory_stays_bounded() {
    let mut r = engine("bounded");
    let v = r.request(Command::SetLimits { max_secs: 3, max_mb: 64 }, secs(5));
    expect_event(&v, "status");
    sleep(secs(10));
    let s = r.status();
    let (buffered, bytes) = (s["buffered_secs"].as_f64().unwrap(), s["buffered_bytes"].as_u64().unwrap());
    assert!(buffered > 0.0 && buffered <= 4.1, "buffered_secs {buffered}");
    assert!(bytes > 0 && bytes <= 64 * 1024 * 1024, "buffered_bytes {bytes}");
    assert_eq!(s["engine_running"], true);

    expect_event(&r.request(Command::StopEngine, secs(10)), "engine_stopped");
    let s = r.status();
    assert_eq!(s["engine_running"], false, "{s}");
    assert_eq!(s["buffered_bytes"], 0, "{s}");
    let out = r.out("clip.mp4");
    let cid = r.save(2, &out);
    let v = r.reply(&cid, secs(5));
    expect_event(&v, "error");
    assert_eq!(v["message"], "engine not running");
}

#[test]
fn video_only_when_audio_pipe_unavailable() {
    let mut r = Rec::spawn("video only");
    let v = r.request(
        Command::StartEngine { args: engine_args(false), max_secs: 30, max_mb: 64 },
        secs(10),
    );
    expect_event(&v, "engine_started");
    sleep(secs(4));
    let out = r.out("clip.mp4");
    let cid = r.save(2, &out);
    expect_event(&r.reply(&cid, secs(5)), "replay_saved");
    let (d, has_v, has_a, _, _) = probe(&out);
    assert!(has_v && !has_a, "video {has_v}, audio {has_a}");
    assert!(d >= 2.0, "duration {d}");
}

#[test]
fn start_failure_reports_error_and_engine_stays_off() {
    let mut r = Rec::spawn("start failure");
    let mut bad = engine_args(true);
    bad.region.width = 0; // testsrc2 rejects a 0-wide frame at init
    let v = r.request(Command::StartEngine { args: bad, max_secs: 30, max_mb: 64 }, secs(10));
    expect_event(&v, "error");
    let msg = v["message"].as_str().unwrap();
    assert!(msg.starts_with("capture failed to start"), "{msg}");
    let s = r.status();
    assert_eq!(s["engine_running"], false, "{s}");
    assert_eq!(s["restarts"], 0, "{s}");
    let v = r.request(Command::StartEngine { args: engine_args(true), max_secs: 30, max_mb: 64 }, secs(10));
    expect_event(&v, "engine_started");
    let v = r.request(Command::StartEngine { args: engine_args(true), max_secs: 30, max_mb: 64 }, secs(10));
    expect_event(&v, "error");
    assert_eq!(v["message"], "engine already running");
    expect_event(&r.request(Command::StopEngine, secs(10)), "engine_stopped");
}

#[test]
fn legacy_and_engineless_commands_are_rejected() {
    let mut r = Rec::spawn("rejected");
    for cmd in [Command::Stop, Command::StopReplay] {
        let v = r.request(cmd, secs(5));
        expect_event(&v, "error");
        assert_eq!(v["message"], "legacy command not supported by this recorder; update ClipLite");
    }
    let rec = r.out("rec.mp4").to_string_lossy().into_owned();
    for cmd in [
        Command::StopEngine,
        Command::SetLimits { max_secs: 3, max_mb: 64 },
        Command::StartRecording { output_path: rec },
        Command::StopRecording,
        Command::Pause,
        Command::Resume,
    ] {
        let v = r.request(cmd, secs(5));
        expect_event(&v, "error");
        assert_eq!(v["message"], "engine not running");
    }
    assert!(r.unclaimed.is_empty(), "exactly one reply per request: {:?}", r.unclaimed);
}

#[test]
fn stop_engine_finalizes_an_active_recording() {
    let mut r = engine("stop with recording");
    let rec = r.out("rec.mp4");
    let v = r.request(Command::StartRecording { output_path: rec.to_string_lossy().into_owned() }, secs(5));
    expect_event(&v, "recording_started");
    sleep(secs(3));
    let cid = r.send(Command::StopEngine);
    expect_event(&r.reply(&cid, secs(10)), "engine_stopped");
    let v = r.wait(secs(1), "recording_stopped", |v| v["event"] == "recording_stopped");
    assert_ne!(v["cid"], cid.as_str(), "unsolicited: {v}");
    assert_eq!(v["path"], rec.to_string_lossy().as_ref());
    let d = valid_av(&rec);
    assert!(d >= 2.0, "recording duration {d}");
}
