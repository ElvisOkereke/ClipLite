# Phase 1 · WS2: Replay Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the stop-and-restart replay buffer with a capture engine. It has one supervised FFmpeg capture, a bounded in-memory ring of keyframe-aligned chunks, saves taken without interrupting capture, and recording as a second consumer of the same stream. Capture also survives exclusive fullscreen.

**Architecture:**
- The recorder process runs one FFmpeg pipeline: capture (Windows Graphics Capture or Desktop Duplication, chosen by the Task 1 spike) → GPU encode → MPEG-TS on stdout.
- A reader thread splits the TS into keyframe-aligned chunks (`cliplite_core::ts`). The chunks feed a `ReplayRing` capped by seconds and bytes, plus an optional recording sink.
- A save snapshots the ring and remuxes it to MP4 by stream copy in the background.
- A supervisor restarts FFmpeg when it dies (`DXGI_ERROR_ACCESS_LOST`), bumping an *epoch*. Outputs that span a restart are joined with FFmpeg's concat demuxer.
- The UI process talks to this single engine through cid-tagged request/response IPC.

**Tech Stack:** Rust (`cliplite-core`, `cliplite-recorder`, Tauri app), FFmpeg 8.1.1 (`gfxcapture`/`ddagrab`, NVENC/x264, `mpegts`, `concat`), React/TS, the `hil` test kit.

**Spec:** `docs/superpowers/specs/2026-11-11-phase1-foundation-design.md` (WS2, plus the WS2 capture-survival addendum written in Task 1). The evidence is in `docs/benchmarks/raw/baseline/README.md` (findings 1, 3, 7).

## Global Constraints

- Windows 10/11 only. Normal use needs no admin rights. The two-process isolation stays: no FFmpeg or WASAPI in the Tauri process.
- Recorder stdout stays IPC-only (newline JSON). FFmpeg's media stream must not use the recorder's stdout. FFmpeg writes TS to *its own* stdout, which the recorder reads through a pipe.
- Memory and disk are bounded by `buffer_seconds × bitrate` (+ small constant), independent of uptime. The byte cap is `max_replay_buffer_size_mb` (default **512**).
- A save never interrupts capture. Saving 60 s takes **< 2 s** on the dev machine. Output is a valid MP4 of the requested duration **± 1 keyframe interval (1 s)**, with A/V in sync and playable in the trim dialog.
- Survives audio device failure by falling back to video only, as today.
- The buffer is **on by default** (settings migration: duration `0` → `60`). The tray reflects the buffer state.
- **One capture pipeline:** recording reuses the buffer's encoded stream (MP4, stream copy, starts at the most recent keyframe). Pause and resume pause the consumer. WebM/GIF move to **export** (re-encode) in the trim dialog. Settings migration maps `webm`/`gif` → `mp4`.
- The ring must expose recent media to a future consumer (Phase 3 highlight detection): `ReplayRing::snapshot` is that API.
- No force-push. Push feature branches freely. Merging to `main` needs the owner. Conventional commits ending `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Every behaviour change ships with a test that failed first. HIL runs that take over the screen need the owner away.

## Review Focus

1. **33-bit PTS wraparound** (about every 26.5 h of uptime): chunk durations and the ring's seconds math must not break at the wrap. Test: `ring_handles_pts_wraparound` (Task 3).
2. **Save before the buffer holds a complete chunk** (seconds after start, or right after a restart): return an `Error` event "replay buffer is empty", never a crash or a zero-byte MP4. Test: `save_with_empty_ring_reports_error` (Task 5).
3. **Overlapping operations:** two saves within a second, or a save during a recording. Both must produce valid files, with no shared temp paths. Test: `two_quick_saves_and_save_during_recording` (Task 5).
4. **FFmpeg dies mid-buffer or mid-recording** (the exclusive-fullscreen case): the engine restarts within 5 s, and saves or recordings that span the restart are still valid MP4s. Test: `capture_restart_keeps_saves_and_recordings_valid` (Task 5).
5. **Output paths with spaces, or folders that don't exist yet:** create the folders, and report a failure as an `Error` event. Test: inside `save_produces_requested_duration` (Task 5), which uses `"out dir with spaces/new sub"`.

---

## File Structure

```
crates/cliplite-core/src/
  ts.rs            NEW  MPEG-TS packet parser + keyframe chunker (pure)
  ring.rs          NEW  ReplayRing: bounded store of chunks, snapshot by seconds, per-epoch PSI (pure)
  ffmpeg.rs        MOD  CaptureSource; build_engine_command; build_concat_remux_command; build_export_command
  ipc.rs           MOD  engine protocol v2 (commands/events below)
  tests/fixtures/sample_2s.ts   NEW  committed fixture (testsrc2 + sine, libx264 -g 30, 320x240, ~150 KB)
crates/cliplite-recorder/src/
  engine.rs        NEW  Engine: FFmpeg supervisor, stdout reader → chunker → ring/recording, save/remux workers
  recording.rs     NEW  RecordingSink: segment files per pause/epoch, concat on stop
  main.rs          MOD  IPC loop drives one Engine (old Start/StartReplay/... removed)
crates/cliplite-recorder/tests/
  engine_it.rs     NEW  integration tests through the binary with CaptureSource::TestPattern
apps/desktop/src-tauri/src/
  capture_config.rs NEW (from test_mode::replay_args) Settings → EngineArgs
  recorder.rs      MOD  EngineClient: one child, reader thread, cid-matched request/response
  commands.rs      MOD  replay/recording/status commands on the engine; legacy restart path removed
  settings.rs      MOD  settings_version 2 + migrate()
  tray.rs          MOD  set_replay_state
  lib.rs           MOD  start engine at launch when buffer enabled
apps/desktop/src/  MOD  RecorderView (no format select; buffer status), TrimDialog (export MP4/WebM/GIF), types/api
tools/testkit/     MOD  hil capture-spike; matrix --gpu-load; settings_version in test settings
docs/adr/0001-capture-source.md, docs/adr/0002-replay-engine.md   NEW
docs/benchmarks/raw/phase1-ws2/   NEW  after-data
```

---

### Task 1: Capture-source spike and spec addendum

Decide `gfxcapture` (Windows Graphics Capture) or `ddagrab` (Desktop Duplication) as the default, using evidence. The supervisor (Task 5) ships either way.

**Files:**
- Create: `tools/testkit/src/spike.rs`; modify `tools/testkit/src/bin/hil.rs` (subcommand `capture-spike`), `tools/testkit/src/lib.rs`
- Create: `docs/adr/0001-capture-source.md`
- Modify: the spec's WS2 section: add a "Capture survival" bullet list (supervise and restart on FFmpeg exit; source per this ADR; findings 1/3/7 from the baseline README)

**Interfaces:**
- Produces:
  - `hil capture-spike --source <ddagrab|gfxcapture> [--trials 10] [--gpu-load 100] --out <dir>`
  - `spike.json`: `{source, trials:[{alive_at_end: bool, exit_code: Option<i32>, frames: u64, expected_frames: u64, stderr_tail: String}]}`
- Each trial runs FFmpeg directly for 15 s at 30 fps (`-f lavfi -i <source>` → `h264_nvenc` → `-f null -`, with `-progress pipe:2` for the frame count). It launches `fakegame --mode exclusive --gpu-load <n>` at t = 4 s. Pass = alive at 15 s AND frames ≥ 0.9 × 450.
- Source strings:
  - `ddagrab=output_idx=0:framerate=30:draw_mouse=1`
  - `gfxcapture=monitor_idx=0:max_framerate=30:capture_cursor=1`

- [ ] **Step 1: Write failing unit tests** in `spike.rs`

```rust
#[test] fn parses_ffmpeg_progress_frames() {
    assert_eq!(last_progress_frame("frame=12\nfps=30\nframe=431\nprogress=end\n"), Some(431));
    assert_eq!(last_progress_frame("nothing"), None);
}
#[test] fn trial_verdict() {
    assert!(trial_passed(true, 440, 450));
    assert!(!trial_passed(true, 300, 450));   // starved
    assert!(!trial_passed(false, 450, 450));  // died
}
#[test] fn source_filter_strings() {
    assert_eq!(source_filter("ddagrab", 0, 30), "ddagrab=output_idx=0:framerate=30:draw_mouse=1");
    assert_eq!(source_filter("gfxcapture", 0, 30), "gfxcapture=monitor_idx=0:max_framerate=30:capture_cursor=1");
}
```

- [ ] **Step 2: Run** `cargo test -p cliplite-testkit --lib spike`. Expected: FAIL (undefined).
- [ ] **Step 3: Implement** `last_progress_frame`, `trial_passed`, `source_filter` and the `capture-spike` subcommand. It uses `Job`, `launch_game` and `prepare_out` from the existing harness, and must refuse to run while any ClipLite capture is running (`procmon::foreign_capture`).
- [ ] **Step 4: Run tests.** Expected: PASS.
- [ ] **Step 5: Run the spike** (owner away, about 10 min). Run both sources with `--trials 10 --gpu-load 100`, writing to `docs/benchmarks/raw/phase1-ws2/spike-<source>/`.
- [ ] **Step 6: Write ADR 0001.**
  - Default = `gfxcapture` if it passes ≥ 9/10 trials, otherwise `ddagrab`.
  - Record both pass rates and the reasoning. `gfxcapture` needs Windows 10 2004+, and the yellow border must be off: it defaults to `display_border=false`, so assert that in the snapshot.
  - Also record whether `ddagrab` + restart (Task 5) would lose more than 2 s per transition: time from the death to the next frame, from `stderr_tail`/progress.
- [ ] **Step 7: Commit**: `feat(testkit): capture-source spike; ADR 0001 capture source` (data plus spec addendum).

---

### Task 2: MPEG-TS keyframe chunker (`cliplite_core::ts`)

**Files:** create `crates/cliplite-core/src/ts.rs` and `crates/cliplite-core/tests/fixtures/sample_2s.ts`; modify `lib.rs` (`pub mod ts;`).

Generate the fixture once:

```
ffmpeg -f lavfi -i testsrc2=s=320x240:r=30 -f lavfi -i sine=f=440 -t 2 -c:v libx264 -g 30 -pix_fmt yuv420p -c:a aac -f mpegts sample_2s.ts
```

**Interfaces:**
- Produces:
  ```rust
  pub const TS_PACKET: usize = 188;
  #[derive(Clone, Debug)]
  pub struct Chunk { pub epoch: u32, pub start_pts_90k: u64, pub end_pts_90k: u64, pub data: std::sync::Arc<[u8]> }
  impl Chunk { pub fn duration_s(&self) -> f64 }           // wrap-aware (33-bit)
  pub struct TsChunker { /* parser state */ }
  impl TsChunker {
      pub fn new(epoch: u32) -> Self;
      /// Feed raw bytes (any split); returns chunks completed by this call.
      pub fn push(&mut self, bytes: &[u8]) -> Vec<Chunk>;
      /// Latest PAT+PMT packets (376 bytes), once seen.
      pub fn psi(&self) -> Option<std::sync::Arc<[u8]>>;
  }
  ```
- A chunk starts at a video PES whose adaptation field has `random_access_indicator` set (FFmpeg's mpegts muxer sets it on keyframes). It ends right before the next such packet. The video PID and stream type (0x1B H.264 / 0x24 HEVC) come from the PMT. `start_pts_90k` is the PTS of that keyframe PES, and `end_pts_90k` is the next keyframe's PTS. Packets before the first keyframe are dropped. Resync on lost sync bytes (0x47) by scanning forward.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn fixture_splits_into_one_second_keyframe_chunks() {
    let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample_2s.ts")).unwrap();
    let mut c = TsChunker::new(0);
    let chunks: Vec<Chunk> = bytes.chunks(1000).flat_map(|b| c.push(b)).collect(); // arbitrary splits
    assert_eq!(chunks.len(), 1);                       // 2 keyframes → 1 closed chunk (second still open)
    assert!((chunks[0].duration_s() - 1.0).abs() < 0.05);
    assert_eq!(chunks[0].data.len() % TS_PACKET, 0);
    assert_eq!(chunks[0].data[0], 0x47);
    assert_eq!(c.psi().unwrap().len(), 2 * TS_PACKET);
}
#[test] fn split_points_do_not_matter() { /* push whole file vs 1-byte pieces → identical chunk bytes */ }
#[test] fn resyncs_after_garbage() { /* 37 junk bytes prepended → same chunks */ }
#[test] fn duration_wraps_at_33_bits() {
    let c = Chunk { epoch: 0, start_pts_90k: (1u64 << 33) - 45_000, end_pts_90k: 45_000, data: vec![].into() };
    assert!((c.duration_s() - 1.0).abs() < 1e-9);
}
```

- [ ] **Step 2: Run** `cargo test -p cliplite-core ts`. Expected: FAIL.
- [ ] **Step 3: Implement** `ts.rs` (PAT → PMT PID → video PID; PES header PTS; adaptation-field RAI).
- [ ] **Step 4: Run tests.** Expected: PASS.
- [ ] **Step 5: Commit**: `feat(core): MPEG-TS keyframe chunker`.

---

### Task 3: `ReplayRing` (`cliplite_core::ring`)

**Files:** create `crates/cliplite-core/src/ring.rs`; modify `lib.rs`.

**Interfaces:**
- Consumes: `ts::Chunk`.
- Produces:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq)] pub struct RingLimits { pub max_secs: f64, pub max_bytes: u64 }
  #[derive(Clone, Copy, Debug, PartialEq)] pub struct RingStats { pub secs: f64, pub bytes: u64, pub chunks: usize }
  /// Chunks grouped by epoch, each group prefixed by that epoch's PSI, oldest first.
  pub struct Snapshot { pub segments: Vec<(std::sync::Arc<[u8]>, Vec<Chunk>)>, pub secs: f64 }
  pub struct ReplayRing { /* VecDeque<Chunk>, HashMap<u32, Arc<[u8]>> psi */ }
  impl ReplayRing {
      pub fn new(limits: RingLimits) -> Self;
      pub fn set_limits(&mut self, limits: RingLimits);   // evicts immediately if smaller
      pub fn set_psi(&mut self, epoch: u32, psi: std::sync::Arc<[u8]>);
      pub fn push(&mut self, chunk: Chunk);               // then evict oldest while secs > max_secs or bytes > max_bytes (always keep ≥ 1)
      pub fn snapshot(&self, secs: f64) -> Option<Snapshot>; // newest chunks whose total ≥ secs (or all); None if empty or PSI missing
      pub fn stats(&self) -> RingStats;
  }
  ```
- Snapshotting is cheap: it clones `Arc`s under the caller's lock and copies no bytes.

- [ ] **Step 1: Write failing tests**

```rust
fn ch(epoch: u32, start_s: f64, len_s: f64, bytes: usize) -> Chunk { /* 90 kHz pts, data = vec![0x47; bytes] */ }
#[test] fn evicts_by_seconds() { /* limits 60 s; push 70 × 1 s → stats.secs ≈ 60, chunks 60 */ }
#[test] fn evicts_by_bytes() { /* max_bytes 10_000; push 1 s × 3_000 B chunks → bytes ≤ 10_000, ≥ 1 chunk kept */ }
#[test] fn snapshot_covers_requested_seconds_from_a_keyframe() { /* 30 × 1 s; snapshot(10.0).secs in [10, 11) */ }
#[test] fn snapshot_groups_by_epoch_with_psi() { /* epochs 0,0,1,1 → 2 segments, each psi of its epoch */ }
#[test] fn snapshot_empty_is_none() { assert!(ReplayRing::new(L).snapshot(5.0).is_none()); }
#[test] fn ring_handles_pts_wraparound() { /* chunks straddling 2^33 → secs ≈ count */ }
#[test] fn shrinking_limits_evicts_now() { /* set_limits(30 s) after 60 → secs ≈ 30 */ }
```

- [ ] **Step 2: Run** `cargo test -p cliplite-core ring`. Expected: FAIL.
- [ ] **Step 3: Implement** `ring.rs`.
- [ ] **Step 4: Run tests.** Expected: PASS.
- [ ] **Step 5: Commit**: `feat(core): bounded replay ring with epoch-aware snapshots`.

---

### Task 4: FFmpeg command builders and IPC v2 (`cliplite-core`)

**Files:** modify `crates/cliplite-core/src/ffmpeg.rs`, `ffmpeg_tests.rs`, `ipc.rs` and `ipc_tests.rs`.

**Interfaces:**
- Produces (ffmpeg):
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum CaptureSource { Gfxcapture, Ddagrab, TestPattern }
  /// Capture + encode → MPEG-TS on FFmpeg's stdout (`-f mpegts pipe:1`), GOP = fps (1 s keyframes),
  /// `-mpegts_flags +resend_headers`, audio from `pipe_name` when `sys_audio_format` is Some.
  pub fn build_engine_command(ffmpeg: &Path, source: CaptureSource, region: &Region, enc: &EncConfig,
                              sys_audio_format: Option<&AudioFormat>, pipe_name: &str) -> Vec<String>;
  /// `-f concat -safe 0 -i <list> -c copy -movflags +faststart <out>`
  pub fn build_concat_remux_command(ffmpeg: &Path, list_file: &Path, out: &Path) -> Vec<String>;
  #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum ExportFormat { Mp4Copy, Webm, Gif }
  /// Trim/export: Mp4Copy = stream copy; Webm = libvpx-vp9 + libopus; Gif = fps=15,scale=640:-1 palette (split/palettegen/paletteuse).
  pub fn build_export_command(ffmpeg: &Path, input: &Path, start_s: f64, end_s: f64, fmt: ExportFormat, out: &Path) -> Vec<String>;
  ```
  `TestPattern` = `-f lavfi -i testsrc2=s=<w>x<h>:r=<fps>` (and `-f lavfi -i sine=f=440` instead of the pipe when audio is requested). It's encoded with `libx264` whatever `enc.hw_encoder` says, so it runs on CI without a GPU.
- Produces (ipc), replacing `Start`/`Stop`/`StartReplay`/`StopReplay`/`SaveReplay`:
  ```rust
  pub struct EngineArgs { pub ffmpeg_path: String, pub source: CaptureSource, pub region: Region, pub audio_cfg: AudioConfig,
                          pub enc_cfg: EncConfig, pub audio_pipe: Option<String> }
  pub enum Command { StartEngine { args: EngineArgs, max_secs: u32, max_mb: u32 }, StopEngine,
                     SetLimits { max_secs: u32, max_mb: u32 }, SaveReplay { secs: u32, output_path: String },
                     StartRecording { output_path: String }, StopRecording, Pause, Resume, Status }
  pub enum Event { EngineStarted, EngineStopped, EngineRestarted { reason: String, restarts: u32 },
                   ReplaySaved { path: String, duration_s: f64 }, RecordingStarted { path: String },
                   RecordingStopped { path: String }, Status { engine_running: bool, recording: bool, paused: bool,
                   elapsed: f64, buffered_secs: f64, buffered_bytes: u64, restarts: u32 }, Error { message: String } }
  ```
  These are serde-tagged exactly as today (`cmd` / `event`, snake_case), with `Envelope<T>` unchanged. Every request gets exactly one response event carrying the same `cid`: `StartEngine` → `EngineStarted`|`Error`, `SaveReplay` → `ReplaySaved`|`Error`, and so on. `EngineRestarted` is unsolicited and has a fresh cid.

- [ ] **Step 1: Write failing tests.**
  - Insta snapshots `engine_cmd__{gfxcapture,ddagrab,test_pattern}_{audio,noaudio}`, plus a `region 2560x1440` case.
  - Asserts in the same test: args end with `["-f","mpegts","pipe:1"]`; contain `"-g","30"` at 30 fps; gfxcapture contains no `display_border=1`.
  - `concat_remux_cmd` snapshot.
  - `export_cmd__{mp4copy,webm,gif}` snapshots.
  - IPC goldens:
  ```rust
  #[test] fn start_engine_round_trips() { /* serialize → {"cmd":"start_engine","args":{...,"source":"gfxcapture",...},"max_secs":60,"max_mb":512} → parse back */ }
  #[test] fn status_event_shape() {
      assert_eq!(serde_json::to_string(&Event::Status { engine_running: true, recording: false, paused: false, elapsed: 0.0,
          buffered_secs: 59.5, buffered_bytes: 12_000_000, restarts: 1 }).unwrap(),
          r#"{"event":"status","engine_running":true,"recording":false,"paused":false,"elapsed":0.0,"buffered_secs":59.5,"buffered_bytes":12000000,"restarts":1}"#);
  }
  ```
  Delete the old golden tests for the removed variants, and the old `build_record_command_with_pipe` snapshots. Keep the builder only if a caller remains; it doesn't.
- [ ] **Step 2: Run** `cargo test -p cliplite-core`. Expected: FAIL / new snapshots pending.
- [ ] **Step 3: Implement.** Reuse the existing per-encoder argument logic, moved into `build_engine_command`.
- [ ] **Step 4: Run**, review every `.snap` by eye (NVENC args unchanged from the old builder apart from the output side), then accept.
- [ ] **Step 5: Commit**: `feat(core): engine/remux/export command builders; IPC v2`.

---

### Task 5: Engine and recording sink (`cliplite-recorder`)

**Files:** create `crates/cliplite-recorder/src/engine.rs`, `recording.rs`, `tests/engine_it.rs`; rewrite the dispatch in `main.rs`; slim `recorder.rs` (keep `spawn_ffmpeg`, `elevate_priority`, and the stderr drain/`StderrTail`).

**Interfaces:**
- Consumes: Tasks 2–4.
- Produces (internal to the recorder):
  ```rust
  pub type Emit = std::sync::Arc<dyn Fn(Event, &str) + Send + Sync>;   // (event, cid)
  pub struct Engine { /* Arc<Mutex<ReplayRing>>, Arc<Mutex<Option<RecordingSink>>>, supervisor thread, ... */ }
  impl Engine {
      pub fn start(args: EngineArgs, limits: RingLimits, emit: Emit) -> anyhow::Result<Engine>;
      pub fn save(&self, secs: u32, out: PathBuf, cid: String);          // async: emits ReplaySaved|Error with cid
      pub fn start_recording(&self, out: PathBuf) -> anyhow::Result<()>;
      pub fn stop_recording(&self, cid: String);                          // async: emits RecordingStopped|Error
      pub fn pause(&self) -> anyhow::Result<()>;  pub fn resume(&self) -> anyhow::Result<()>;
      pub fn set_limits(&self, limits: RingLimits);
      pub fn status(&self) -> Event;                                      // Event::Status
      pub fn stop(self);                                                  // kills FFmpeg + audio, joins threads
  }
  pub struct RecordingSink { /* dir, segments: Vec<PathBuf>, current: Option<BufWriter<File>>, paused, epoch */ }
  impl RecordingSink {
      pub fn new(temp_dir: PathBuf, out: PathBuf) -> anyhow::Result<Self>;
      pub fn on_chunk(&mut self, chunk: &Chunk, psi: &[u8]) -> std::io::Result<()>; // new segment file on first chunk / after resume / on epoch change (writes psi first)
      pub fn pause(&mut self);  pub fn resume(&mut self);
      pub fn finish(self) -> anyhow::Result<(Vec<PathBuf>, PathBuf)>;   // (segment files, out) for concat-remux
  }
  ```
- **Supervisor.** It spawns FFmpeg (`build_engine_command`) and drains stderr through the existing drain, keeping its tail. A reader thread feeds stdout to `TsChunker` → `ring.push` + `ring.set_psi` + `recording.on_chunk`. On an unexpected exit it emits `EngineRestarted { reason: <last 3 stderr lines>, restarts }`, bumps the epoch, and respawns: backoff 0.5 s, doubling to a 5 s cap, reset after 30 s of healthy running. Audio (`SysAudioCapture`) is restarted with it. If audio fails to open, FFmpeg is built without audio (video-only fallback).
- **Save.** Snapshot under the ring lock. Write `temp/<cid>/seg<i>.ts` (PSI + chunk bytes) and `list.txt`, run `build_concat_remux_command`, emit `ReplaySaved { duration_s }` (ffprobe-free: `snapshot.secs`), then delete `temp/<cid>/`. Create the output's parent directories. An empty snapshot gives `Error { "replay buffer is empty" }`.
- **Temp root.** `cliplite_core::paths::app_dir()/temp/engine/`. Clear it at engine start: it is only engine scratch.

- [ ] **Step 1: Write failing integration tests** in `tests/engine_it.rs`.
  - They drive the recorder binary over IPC with `CaptureSource::TestPattern`, the ffmpeg path from `CLIPLITE_FFMPEG` (else `apps/desktop/src-tauri/bin/ffmpeg.exe`), and `CLIPLITE_HOME` set to a temp dir with a space in it.
  - Helper `fn engine() -> (Child, Lines)` sends `start_engine` (`max_secs` 30, `max_mb` 64) and asserts `engine_started`.
  - Clip assertions use ffprobe through a helper `probe(path) -> (duration_s, has_video, has_audio, v_start, a_start)`.
  ```rust
  #[test] fn save_produces_requested_duration() {
      // wait 8 s; save_replay secs=5 → output "<tmp>/out dir with spaces/new sub/clip.mp4"
      // → replay_saved within 5 s; duration in [5.0, 6.1]; video+audio; |v_start − a_start| < 0.1
  }
  #[test] fn save_with_empty_ring_reports_error() { /* save immediately after engine_started → error "replay buffer is empty" */ }
  #[test] fn two_quick_saves_and_save_during_recording() {
      // wait 6 s; start_recording; two save_replay 2 s apart → both replay_saved, both valid;
      // wait 3 s; stop_recording → recording_stopped; recording duration ≥ 4.5 s; all three files distinct and valid
  }
  #[test] fn pause_excludes_paused_time() { /* record 3 s, pause 3 s, resume 3 s, stop → duration in [5.0, 7.5] */ }
  #[test] fn capture_restart_keeps_saves_and_recordings_valid() {
      // start_recording; wait 4 s; kill the engine's ffmpeg child (find by parent pid); expect engine_restarted within 5 s;
      // wait 4 s; save_replay 6 → valid mp4 spanning the restart; stop_recording → valid mp4 ≥ 6 s
  }
  #[test] fn memory_stays_bounded() { /* set_limits max_secs=3; wait 10 s; status.buffered_secs ≤ 4.1 and buffered_bytes ≤ 64 MiB */ }
  #[test] fn video_only_when_audio_pipe_unavailable() { /* EngineArgs with no audio → save → has_video && !has_audio */ }
  ```
  These tests need FFmpeg: CI fetches it (`fetch-ffmpeg.ps1`), so they run in CI too.
- [ ] **Step 2: Run** `cargo test -p cliplite-recorder --test engine_it`. Expected: FAIL (old protocol rejects `start_engine`).
- [ ] **Step 3: Implement** `recording.rs`, then `engine.rs`, then the `main.rs` dispatch: one `Option<Engine>`, each command → engine call → exactly one response event with the request's cid. Unit-test `RecordingSink` segment rotation in `recording.rs` (`new segment on resume`, `new segment on epoch change`) before using it.
- [ ] **Step 4: Run** `RUSTFLAGS=-Dwarnings cargo test -p cliplite-recorder`. Expected: all PASS, including `stdout_is_ipc` (updated to `status` + `bogus`).
- [ ] **Step 5: Commit**: `feat(recorder): supervised capture engine with in-memory replay ring`.

---

### Task 6: UI process on the engine (Tauri side)

**Files:** create `apps/desktop/src-tauri/src/capture_config.rs`; modify `recorder.rs`, `commands.rs`, `settings.rs`, `tray.rs`, `lib.rs`, `test_mode.rs`, `commands_ipc_tests.rs`.

**Interfaces:**
- Consumes: IPC v2 (Task 4).
- Produces:
  ```rust
  // capture_config.rs (moved from test_mode::replay_args, extended)
  pub fn engine_args(s: &Settings, monitors: &[MonitorInfo], ffmpeg: &Path, encoder: &str, source: CaptureSource) -> EngineArgs;
  pub fn ring_limits(s: &Settings) -> (u32 /*max_secs*/, u32 /*max_mb*/);   // (duration + 2, max_replay_buffer_size_mb)
  // recorder.rs
  pub struct EngineClient { /* RecorderChild, reader thread, Mutex<HashMap<String, Sender<Event>>> pending */ }
  impl EngineClient {
      pub fn spawn(recorder_exe: &Path, on_unsolicited: impl Fn(Event) + Send + 'static) -> anyhow::Result<Self>;
      /// Send with cid; wait for the event carrying the same cid.
      pub fn request(&self, cmd: Command, cid: &str, timeout: Duration) -> anyhow::Result<Event>;
  }
  // settings.rs
  pub const SETTINGS_VERSION: u32 = 2;
  impl Settings { pub fn migrate(&mut self) -> bool; }   // true if changed (then saved)
  // tray.rs
  pub fn set_replay_state<R: Runtime>(app: &AppHandle<R>, buffer_secs: Option<u32>);  // None = off
  ```
- **Migration v1 → v2:**
  - `replay_buffer_duration_secs == 0` → `60`
  - `default_format` `webm`/`gif` → `mp4`
  - `max_replay_buffer_size_mb == 500` → `512`
  - set `settings_version = 2`
- **Commands:**
  - `start_replay` → ensure the engine (`StartEngine` with `capture_config`), else `SetLimits`.
  - `stop_replay` → `StopEngine`, unless recording; then only mark the buffer off (saves refused).
  - `start_recording` → ensure the engine, then `StartRecording` (path from Settings, `.mp4`).
  - `stop_recording` → `StopRecording`, then add the clip to the library.
  - `pause_recording`/`resume_recording` → `Pause`/`Resume`.
  - `save_replay` → `SaveReplay` with timeout `secs + 30 s`, then add the clip to the library.
  - `get_recording_status` → `Status`.
  - Delete the old restart-after-save code and `replay_child`.
  - `EngineRestarted` → log at `warn` and emit `"engine-restarted"` to the UI.
- **Launch:** after encoder detection, if `replay_buffer_duration_secs > 0`, start the engine and set the tray to "Replay buffer: on (N s)". `test_mode` just calls the same path. Once per launch, also delete legacy `app_dir()/temp/cliplite_replay_*.mkv` files left by the old buffer. That's `pub fn remove_legacy_replay_files(dir: &Path) -> usize`, tested by `removes_only_legacy_replay_files`: it deletes `cliplite_replay_123.mkv`, keeps `other.mkv` and keeps the `engine/` folder.
- **Source:** the ADR 0001 default, overridable with env `CLIPLITE_CAPTURE_SOURCE=ddagrab|gfxcapture` for the HIL A/B runs.

- [ ] **Step 1: Write failing tests**
  ```rust
  #[test] fn migrate_v1_defaults() { /* v0 settings with duration 0, format "gif", cap 500 → 60, "mp4", 512, version 2; migrate() true */ }
  #[test] fn migrate_keeps_user_choices() { /* duration 180, cap 2048, format "mp4" → unchanged except version; true */ }
  #[test] fn migrate_is_idempotent() { /* second call → false */ }
  #[test] fn ring_limits_add_headroom() { /* duration 60, cap 512 → (62, 512) */ }
  #[test] fn engine_args_from_settings() { /* monitor selection, audio ids, encoder, source carried through (port the test_mode tests) */ }
  #[test] fn engine_client_matches_responses_by_cid() {
      // spawn the real recorder binary (env!("CARGO_BIN_EXE_...") is unavailable across crates: locate target/<profile>/cliplite-recorder.exe
      // next to the test exe's parent); two concurrent Status requests with different cids → each gets its own Status
  }
  ```
- [ ] **Step 2: Run** `cargo test -p cliplite --lib`. Expected: FAIL.
- [ ] **Step 3: Implement**, removing dead code (old IPC helpers, `to_start_args` callers, the restart block) and keeping `-Dwarnings` clean.
- [ ] **Step 4: Run** `RUSTFLAGS=-Dwarnings cargo test --workspace`. Expected: PASS. Then a manual check: launch the release build with `CLIPLITE_HOME` set; the tray tooltip shows the buffer on, and `Status.buffered_secs` grows to about 60.
- [ ] **Step 5: Commit**: `feat(desktop): drive the capture engine; settings v2; buffer on at launch`.

---

### Task 7: Frontend: recording, buffer status, export

**Files:** modify `apps/desktop/src/components/RecorderView.tsx`, `TrimDialog.tsx`, `api.ts`, `types.ts`; `commands.rs` (`trim_clip` → `export_clip`).

**Interfaces:**
- Consumes: `build_export_command`/`ExportFormat` (Task 4); `get_recording_status` returning `{ is_recording, is_paused, elapsed_seconds, replay_active, buffered_secs, restarts }`.
- Produces:
  - Tauri command `export_clip(input_path, output_path, start_time, end_time, format: ExportFormat) -> Result<(), String>`, replacing `trim_clip`.
  - `api.exportClip(...)`.
- UI changes:
  - The recording format `<select>` is removed (recording is always MP4; the `FORMAT_OPTIONS` constant goes).
  - The replay card shows "Buffer: 47 s / 60 s" from `buffered_secs`, and "capture restarted N×" when `restarts > 0`.
  - The trim dialog gets a format select (MP4 · lossless / WebM / GIF) and calls `exportClip`; the output extension follows the format.

- [ ] **Step 1: Write failing tests.** Rust: `export_clip` uses `build_export_command` (unit-test the command selection through a pure helper `export_args(fmt) -> ExportFormat` mapping from the UI strings `"mp4"|"webm"|"gif"`, with an error on anything else). Frontend: `npm run typecheck` fails until the types are updated.
- [ ] **Step 2: Run** `cargo test -p cliplite --lib export && (cd apps/desktop && npm run typecheck)`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** both. Expected: PASS. Then a manual check in `npx tauri dev`: record 5 s → MP4 in the library; trim → export WebM and GIF, both play.
- [ ] **Step 5: Commit**: `feat(ui): MP4-only recording, buffer status, WebM/GIF export`.

---

### Task 8: HIL verification and after-data

**Files:**
- Modify: `tools/testkit/src/harness.rs` (test settings add `"settings_version": 2`), `bin/hil.rs` (`matrix --gpu-load <n>`, default 50).
- Create: `docs/benchmarks/raw/phase1-ws2/` and `docs/adr/0002-replay-engine.md`; update `ARCHITECTURE.md` (replay engine section), the README (status), and `docs/ROADMAP.md` (WS2 ✅).

**Interfaces:** the existing `hil` CLI; `capture-spike` from Task 1.

- [ ] **Step 1: Write failing test.** `settings_carry_version_2` in `harness.rs`: `test_settings(...)["settings_version"] == 2`. Run it (FAIL), implement, run it (PASS).
- [ ] **Step 2: Build release** (`npm run bundle` in `apps/desktop`, or `cargo build --release --workspace`). Then run, with the owner away (about 3.5 h; overnight is fine), into `docs/benchmarks/raw/phase1-ws2/`:
  - `matrix --trials 5`
  - `matrix --trials 5 --gpu-load 100`
  - `bench --runs 5 --duration-secs 60`
  - `soak --hours 2`

  (`scripts/run-hil-suite.ps1` covers matrix, bench and soak; run the `--gpu-load 100` matrix separately.)
- [ ] **Step 3: Check the done-when criteria against the data**, and record each in a results README with pass/fail:
  - **Matrix:** non-elevated 60/60 at both GPU loads. Exclusive trials have `capture_alive` true, and clips cover the in-game time.
  - **Soak:** memory slope < 5 MB/h and band < 100 MB; temp dir ≈ 0 (in-memory ring); 11/11 saves ok; save latency < 2 s; clip duration 60 ± 1 s.
  - **Bench:** `replay_record` RAM within 150 MB of `replay` (one capture), compared against the baseline's 1,208 vs 653 MB.
  - **Restarts:** count from the logs (`EngineRestarted`) and report them.
- [ ] **Step 4: Write ADR 0002** covering the ring and engine design, the measured trade-offs, and the rejected on-disk segment ring. Update the docs listed above with the new numbers. Keep README "Built and verified" vs "Planned" accurate: move the WS2 fixes to built only if Step 3 passed.
- [ ] **Step 5: Commit and push** the branch: `docs: WS2 after-data, ADR 0002, docs refresh`. Open a PR to `main` with before/after tables.

---

## Not in this plan

- WS1 (hotkeys for elevated games, Rust-side hotkey actions), WS3 (encoder fallback, diagnostics), WS4 charts, WS6 post.
- The mic-only recording bug (baseline finding 6) is fixed only if the engine's audio path naturally removes it. Task 5's tests cover sys+mic and no-audio, not mic-only. Add a mic-only integration test if a test machine has a mic (`TestAudio` is available in the harness), otherwise leave it for WS3.
