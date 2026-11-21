//! The capture engine: one supervised FFmpeg writing MPEG-TS to its stdout,
//! cut into keyframe chunks that feed a bounded in-memory replay ring and, while
//! recording, a `RecordingSink`. Saves and recordings never interrupt capture.
//!
//! Threads: the supervisor reads FFmpeg's stdout inline (EOF = FFmpeg died)
//! and restarts it with backoff; a stderr drain per FFmpeg; one worker per
//! save / stop-recording (remux). Locks (`ring`, `rec`, `child`, `phase`,
//! `workers`) are never held together, and never across a remux.
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use cliplite_core::audio::AudioFormat;
use cliplite_core::ffmpeg::{self, CaptureSource};
use cliplite_core::ipc::{EngineArgs, Event};
use cliplite_core::ring::{ReplayRing, RingLimits, Snapshot};
use cliplite_core::telemetry::{new_cid, Stage};
use cliplite_core::ts::TsChunker;

use crate::recorder::{spawn_ffmpeg, spawn_stderr_drain, StderrTail};
use crate::recording::RecordingSink;

/// Writes one event to the IPC channel, tagged with a cid.
pub type Emit = Arc<dyn Fn(Event, &str) + Send + Sync>;

const DEFAULT_AUDIO_PIPE: &str = r"\\.\pipe\cliplite_engine_audio";
/// FFmpeg exiting this soon after `start` is a start failure, not a restart.
const STARTUP_WINDOW: Duration = Duration::from_millis(500);
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// A pipeline that ran this long resets the backoff.
const HEALTHY_RUN: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);

/// Engine scratch space, cleared at every engine start.
pub fn temp_root() -> PathBuf {
    cliplite_core::paths::app_dir().join("temp").join("engine")
}

struct Recording {
    sink: RecordingSink,
    started: Instant,
    paused_total: Duration,
    paused_at: Option<Instant>,
    write_failed: bool,
}

impl Recording {
    fn elapsed(&self) -> f64 {
        let paused_now = self.paused_at.map(|t| t.elapsed()).unwrap_or_default();
        self.started.elapsed().saturating_sub(self.paused_total + paused_now).as_secs_f64()
    }
}

enum Phase {
    Starting,
    Running,
    StartFailed(String),
}

struct Shared {
    args: EngineArgs,
    emit: Emit,
    temp: PathBuf,
    ring: Mutex<ReplayRing>,
    rec: Mutex<Option<Recording>>,
    /// The live FFmpeg (None while restarting).
    child: Mutex<Option<Child>>,
    phase: Mutex<Phase>,
    stopping: AtomicBool,
    restarts: AtomicU32,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

pub struct Engine {
    shared: Arc<Shared>,
    supervisor: Option<JoinHandle<()>>,
}

impl Engine {
    /// Spawn FFmpeg and the supervisor. Fails if FFmpeg cannot be spawned or
    /// exits within the startup window (bad source, bad arguments).
    pub fn start(args: EngineArgs, limits: RingLimits, emit: Emit, cid: &str) -> Result<Engine> {
        let temp = temp_root();
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).with_context(|| format!("create {}", temp.display()))?;
        let shared = Arc::new(Shared {
            args,
            emit,
            temp,
            ring: Mutex::new(ReplayRing::new(limits)),
            rec: Mutex::new(None),
            child: Mutex::new(None),
            phase: Mutex::new(Phase::Starting),
            stopping: AtomicBool::new(false),
            restarts: AtomicU32::new(0),
            workers: Mutex::new(Vec::new()),
        });
        let first = spawn_pipeline(&shared, 0, cid)?;
        let s = shared.clone();
        let sup = std::thread::Builder::new()
            .name("engine-supervisor".into())
            .spawn(move || supervise(s, first))?;
        let t0 = Instant::now();
        while t0.elapsed() < STARTUP_WINDOW && !sup.is_finished() {
            std::thread::sleep(POLL);
        }
        let mut phase = shared.phase.lock().unwrap();
        if let Phase::StartFailed(reason) = &*phase {
            let reason = reason.clone();
            drop(phase);
            let _ = sup.join();
            bail!("capture failed to start: {reason}");
        }
        *phase = Phase::Running;
        drop(phase);
        Ok(Engine { shared, supervisor: Some(sup) })
    }

    /// Async: emits `ReplaySaved` or `Error` with `cid`. The clip is the ring's
    /// newest `secs` (rounded up to whole chunks) at the moment of the call.
    pub fn save(&self, secs: u32, out: PathBuf, cid: String) {
        let sh = &self.shared;
        if secs == 0 {
            return (sh.emit)(Event::Error { message: "save duration must be at least 1 second".into() }, &cid);
        }
        let Some(snap) = sh.ring.lock().unwrap().snapshot(f64::from(secs)) else {
            return (sh.emit)(Event::Error { message: "replay buffer is empty".into() }, &cid);
        };
        let s = sh.clone();
        self.worker(move || {
            let event = match save_snapshot(&s, &snap, &out, &cid) {
                Ok(()) => {
                    tracing::info!(stage = Stage::ReplaySaved.as_str(), cid = %cid, path = %out.display(),
                                   duration_s = snap.secs, "replay saved");
                    Event::ReplaySaved { path: out.to_string_lossy().into_owned(), duration_s: Some(snap.secs) }
                }
                Err(e) => Event::Error { message: format!("save failed: {e:#}") },
            };
            (s.emit)(event, &cid);
        });
    }

    pub fn start_recording(&self, out: PathBuf) -> Result<()> {
        let mut rec = self.shared.rec.lock().unwrap();
        if rec.is_some() {
            bail!("already recording");
        }
        let dir = self.shared.temp.join(format!("rec-{}", new_cid()));
        *rec = Some(Recording {
            sink: RecordingSink::new(dir, out)?,
            started: Instant::now(),
            paused_total: Duration::ZERO,
            paused_at: None,
            write_failed: false,
        });
        Ok(())
    }

    /// Async: the recording ends with the last complete chunk (the open one,
    /// at most one GOP, is not waited for); the remux runs on a worker, which
    /// emits `RecordingStopped` or `Error`.
    pub fn stop_recording(&self, cid: String) {
        let sh = &self.shared;
        let Some(r) = sh.rec.lock().unwrap().take() else {
            return (sh.emit)(Event::Error { message: "not recording".into() }, &cid);
        };
        let s = sh.clone();
        self.worker(move || {
            let event = match finish_recording(&s, r, &cid) {
                Ok(path) => Event::RecordingStopped { path },
                Err(e) => Event::Error { message: format!("recording failed: {e:#}") },
            };
            (s.emit)(event, &cid);
        });
    }

    pub fn pause(&self) -> Result<()> {
        let mut rec = self.shared.rec.lock().unwrap();
        let r = rec.as_mut().ok_or_else(|| anyhow!("not recording"))?;
        if r.paused_at.is_none() {
            r.sink.pause();
            r.paused_at = Some(Instant::now());
        }
        Ok(())
    }

    pub fn resume(&self) -> Result<()> {
        let mut rec = self.shared.rec.lock().unwrap();
        let r = rec.as_mut().ok_or_else(|| anyhow!("not recording"))?;
        if let Some(t) = r.paused_at.take() {
            r.paused_total += t.elapsed();
            r.sink.resume();
        }
        Ok(())
    }

    pub fn set_limits(&self, limits: RingLimits) {
        self.shared.ring.lock().unwrap().set_limits(limits);
    }

    pub fn status(&self) -> Event {
        let stats = self.shared.ring.lock().unwrap().stats();
        let rec = self.shared.rec.lock().unwrap();
        Event::Status {
            engine_running: true,
            recording: rec.is_some(),
            paused: rec.as_ref().is_some_and(|r| r.paused_at.is_some()),
            elapsed: rec.as_ref().map(Recording::elapsed).unwrap_or(0.0),
            buffered_secs: stats.secs,
            buffered_bytes: stats.bytes,
            restarts: self.shared.restarts.load(SeqCst),
        }
    }

    /// Kill FFmpeg and audio, join all threads. Pending saves and stops finish
    /// first; a recording still running is finalized and reported with an
    /// unsolicited `RecordingStopped` (fresh cid), so no footage is dropped.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(sup) = self.supervisor.take() else { return };
        let sh = &self.shared;
        sh.stopping.store(true, SeqCst);
        if let Some(c) = sh.child.lock().unwrap().as_mut() {
            let _ = c.kill();
        }
        let _ = sup.join();
        let workers = std::mem::take(&mut *sh.workers.lock().unwrap());
        for w in workers {
            let _ = w.join();
        }
        let leftover = sh.rec.lock().unwrap().take();
        if let Some(r) = leftover {
            let cid = new_cid();
            let event = match finish_recording(sh, r, &cid) {
                Ok(path) => Event::RecordingStopped { path },
                Err(e) => Event::Error { message: format!("recording failed: {e:#}") },
            };
            (sh.emit)(event, &cid);
        }
        let _ = std::fs::remove_dir_all(&sh.temp);
    }

    fn worker(&self, f: impl FnOnce() + Send + 'static) {
        let mut workers = self.shared.workers.lock().unwrap();
        workers.retain(|w| !w.is_finished());
        workers.push(std::thread::spawn(f));
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// -- capture pipeline ---------------------------------------------------------

struct Pipeline {
    stdout: std::process::ChildStdout,
    audio: Option<Audio>,
    tail: StderrTail,
    drain: JoinHandle<()>,
    started: Instant,
    pid: u32,
    /// The request (or restart) that spawned it, for the `ffmpeg.exited` log.
    cid: String,
}

/// Spawn audio capture (if any) and the engine FFmpeg for `epoch`.
fn spawn_pipeline(sh: &Shared, epoch: u32, cid: &str) -> Result<Pipeline> {
    let a = &sh.args;
    let pipe = a.audio_pipe.clone().unwrap_or_else(|| DEFAULT_AUDIO_PIPE.to_string());
    let (audio, format) = open_audio(a, &pipe);
    let cmd = ffmpeg::build_engine_command(
        Path::new(&a.ffmpeg_path), a.source, &a.region, &a.enc_cfg, format.as_ref(), &pipe);
    let mut child = match spawn_ffmpeg(&cmd, Stdio::piped(), true) {
        Ok(c) => c,
        Err(e) => {
            stop_audio(audio);
            return Err(e);
        }
    };
    let pid = child.id();
    tracing::info!(stage = Stage::FfmpegSpawned.as_str(), cid = %cid, pid, purpose = "engine", epoch,
                   audio = format.is_some(), args = %cmd.join(" "), "ffmpeg spawned");
    let stdout = child.stdout.take().expect("stdout piped");
    let tail = StderrTail::default();
    let drain = spawn_stderr_drain(child.stderr.take().expect("stderr piped"), tail.clone(), "engine");
    let mut slot = sh.child.lock().unwrap();
    if sh.stopping.load(SeqCst) {
        let _ = child.kill(); // stop() ran while we were spawning
    }
    *slot = Some(child);
    Ok(Pipeline { stdout, audio, tail, drain, started: Instant::now(), pid, cid: cid.to_string() })
}

/// Read FFmpeg until it dies; restart it unless the engine is stopping.
fn supervise(sh: Arc<Shared>, first: Pipeline) {
    let mut current = Ok(first);
    let mut epoch = 0u32;
    let mut backoff = BACKOFF_MIN;
    loop {
        let (reason, ran) = match current {
            Ok(p) => run_pipeline(&sh, p, epoch),
            Err(e) => (format!("spawn failed: {e:#}"), Duration::ZERO),
        };
        if sh.stopping.load(SeqCst) {
            return;
        }
        {
            let mut phase = sh.phase.lock().unwrap();
            if matches!(*phase, Phase::Starting) {
                *phase = Phase::StartFailed(reason);
                return;
            }
        }
        if ran >= HEALTHY_RUN {
            backoff = BACKOFF_MIN;
        }
        let restarts = sh.restarts.fetch_add(1, SeqCst) + 1;
        let cid = new_cid();
        tracing::warn!(cid = %cid, restarts, %reason, backoff_ms = backoff.as_millis() as u64,
                       "capture lost; restarting ffmpeg");
        (sh.emit)(Event::EngineRestarted { reason, restarts }, &cid);
        if !sleep_unless_stopping(&sh, backoff) {
            return;
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
        epoch += 1;
        current = spawn_pipeline(&sh, epoch, &cid);
    }
}

/// Pump stdout into the ring/recording until EOF, then reap FFmpeg and stop
/// audio. Returns (restart reason, how long the pipeline ran).
fn run_pipeline(sh: &Shared, mut p: Pipeline, epoch: u32) -> (String, Duration) {
    pump(sh, &mut p.stdout, epoch);
    let status = reap(sh);
    stop_audio(p.audio.take());
    let _ = p.drain.join();
    let ran = p.started.elapsed();
    tracing::info!(stage = Stage::FfmpegExited.as_str(), cid = %p.cid, pid = p.pid, purpose = "engine", epoch,
                   status = ?status, ran_s = ran.as_secs_f64(), "ffmpeg exited");
    let code = status.map(|s| s.to_string()).unwrap_or_else(|| "unknown exit status".into());
    let lines = p.tail.last_messages(3);
    let reason = if lines.is_empty() { code } else { format!("{code}: {}", lines.join(" | ")) };
    (reason, ran)
}

fn pump(sh: &Shared, stdout: &mut impl Read, epoch: u32) {
    let mut chunker = TsChunker::new(epoch);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match stdout.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let chunks = chunker.push(&buf[..n]);
        if chunks.is_empty() {
            continue;
        }
        // A chunk only starts at a keyframe, which follows PAT/PMT, so psi is known.
        let Some(psi) = chunker.psi() else { continue };
        {
            let mut ring = sh.ring.lock().unwrap();
            ring.set_psi(epoch, psi.clone());
            for c in &chunks {
                ring.push(c.clone());
            }
        }
        if let Some(r) = sh.rec.lock().unwrap().as_mut() {
            for c in &chunks {
                if let Err(e) = r.sink.on_chunk(c, &psi) {
                    if !std::mem::replace(&mut r.write_failed, true) {
                        tracing::error!(stage = Stage::Error.as_str(), error = %e, "recording write failed");
                    }
                }
            }
        }
    }
}

/// Take the child after stdout EOF and wait for it (killing it after 2 s).
fn reap(sh: &Shared) -> Option<ExitStatus> {
    let mut child = sh.child.lock().unwrap().take()?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) if t0.elapsed() < Duration::from_secs(2) => std::thread::sleep(POLL),
            _ => {
                let _ = child.kill();
                return child.wait().ok();
            }
        }
    }
}

fn sleep_unless_stopping(sh: &Shared, d: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < d {
        if sh.stopping.load(SeqCst) {
            return false;
        }
        std::thread::sleep(POLL);
    }
    !sh.stopping.load(SeqCst)
}

// -- audio --------------------------------------------------------------------

#[cfg(windows)]
type Audio = (cliplite_core::audio::SysAudioCapture, String);
#[cfg(not(windows))]
type Audio = std::convert::Infallible;

fn audio_requested(a: &EngineArgs) -> bool {
    let set = |id: &Option<String>| id.as_deref().is_some_and(|s| !s.is_empty());
    set(&a.audio_cfg.sys_device_id) || set(&a.audio_cfg.mic_device_id)
}

/// Audio input for FFmpeg: the test pattern uses a generated tone; real
/// sources get WASAPI through the named pipe. Devices that fail to open give
/// `None` and FFmpeg runs video-only.
fn open_audio(a: &EngineArgs, pipe: &str) -> (Option<Audio>, Option<AudioFormat>) {
    if !audio_requested(a) {
        return (None, None);
    }
    if a.source == CaptureSource::TestPattern {
        return (None, Some(AudioFormat { sample_rate: 48_000, channels: 2 }));
    }
    #[cfg(windows)]
    {
        use cliplite_core::audio::SysAudioCapture;
        let (sys, mic) = (a.audio_cfg.sys_device_id.clone(), a.audio_cfg.mic_device_id.clone());
        match SysAudioCapture::start_with_pipe(sys, mic, pipe.to_string()) {
            Ok(Some(cap)) => {
                let format = cap.format.clone();
                (Some((cap, pipe.to_string())), Some(format))
            }
            Ok(None) => {
                tracing::warn!("audio devices failed to open; capturing video only");
                (None, None)
            }
            Err(e) => {
                tracing::warn!("audio capture failed to start ({e}); capturing video only");
                (None, None)
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pipe;
        (None, None)
    }
}

/// Stop audio capture without hanging: its thread may still be blocked in
/// ConnectNamedPipe if FFmpeg died before opening the pipe, so connect to the
/// pipe ourselves to release it.
fn stop_audio(audio: Option<Audio>) {
    #[cfg(windows)]
    if let Some((mut cap, pipe)) = audio {
        let h = std::thread::spawn(move || cap.stop());
        let t0 = Instant::now();
        while !h.is_finished() && t0.elapsed() < Duration::from_secs(3) {
            let _ = std::fs::OpenOptions::new().read(true).open(&pipe);
            std::thread::sleep(Duration::from_millis(50));
        }
        if h.is_finished() {
            let _ = h.join();
        } else {
            tracing::warn!("audio capture thread did not stop; detached");
        }
    }
    #[cfg(not(windows))]
    let _ = audio;
}

// -- saving -------------------------------------------------------------------

/// A filesystem-safe directory name for a request's scratch files.
fn scratch_name(cid: &str) -> String {
    let ok = !cid.is_empty() && cid.len() <= 64 && cid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok { cid.to_string() } else { new_cid() }
}

fn save_snapshot(sh: &Shared, snap: &Snapshot, out: &Path, cid: &str) -> Result<()> {
    let dir = sh.temp.join(format!("save-{}", scratch_name(cid)));
    let res = (|| {
        std::fs::create_dir_all(&dir)?;
        let mut names = Vec::new();
        for (i, (psi, chunks)) in snap.segments.iter().enumerate() {
            let name = format!("seg{i}.ts");
            let mut w = BufWriter::new(std::fs::File::create(dir.join(&name))?);
            w.write_all(psi)?;
            for c in chunks {
                w.write_all(&c.data)?;
            }
            w.flush()?;
            names.push(name);
        }
        remux(sh, &dir, &names, out, cid)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    res
}

fn finish_recording(sh: &Shared, r: Recording, cid: &str) -> Result<String> {
    let dir = r.sink.dir().to_path_buf();
    let res = (|| {
        let (segments, out) = r.sink.finish()?;
        let names: Vec<String> = segments
            .iter()
            .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
            .collect();
        remux(sh, &dir, &names, &out, cid)?;
        Ok(out.to_string_lossy().into_owned())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    res
}

/// Concat-remux `dir/<names>` (in order) into `out`, creating its parent dirs.
fn remux(sh: &Shared, dir: &Path, names: &[String], out: &Path, cid: &str) -> Result<()> {
    // Names are relative to the list file, so no path quoting is needed.
    let list: String = names.iter().map(|n| format!("file '{n}'\n")).collect();
    let list_file = dir.join("list.txt");
    std::fs::write(&list_file, list)?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let cmd = ffmpeg::build_concat_remux_command(Path::new(&sh.args.ffmpeg_path), &list_file, out);
    let mut child = spawn_ffmpeg(&cmd, Stdio::null(), false)?;
    let pid = child.id();
    tracing::info!(stage = Stage::FfmpegSpawned.as_str(), cid = %cid, pid, purpose = "remux",
                   args = %cmd.join(" "), "ffmpeg spawned");
    let tail = StderrTail::default();
    let drain = spawn_stderr_drain(child.stderr.take().expect("stderr piped"), tail.clone(), "remux");
    let status = child.wait()?;
    let _ = drain.join();
    tracing::info!(stage = Stage::FfmpegExited.as_str(), cid = %cid, pid, purpose = "remux",
                   ok = status.success(), "ffmpeg exited");
    if !status.success() {
        bail!("remux failed ({status}): {}", tail.last_messages(3).join(" | "));
    }
    Ok(())
}
