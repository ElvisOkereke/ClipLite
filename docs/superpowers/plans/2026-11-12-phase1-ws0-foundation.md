# Phase 1 · WS0 — Baseline, Restructure, Observability & Test Kit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give ClipLite a clean Cargo workspace, structured cross-process logs, and a hardware-in-the-loop (HIL) test kit, then capture honest "before" numbers from the build the owner runs today.

**Architecture:** First, instrument the `baseline` build minimally (isolated app dir + auto-start replay) so the harness can drive it. Then pin current behaviour with characterization tests, restructure into a workspace (`apps/desktop`, `crates/cliplite-core`, `crates/cliplite-recorder`, `tools/testkit`) and remove dead code. Add `tracing` JSON logs with correlation ids. Build a D3D11 fake game and a Rust HIL runner (hotkey matrix, soak, bench) that drives any ClipLite build from the outside.

**Tech Stack:** Rust 1.95 (MSRV stays 1.77 for app crates), Tauri v2, React 18/TS, `windows` 0.58, `tracing` + `tracing-subscriber` (json) + `tracing-appender`, `insta`, `uuid`, FFmpeg (gyan.dev build).

**Spec:** `docs/superpowers/specs/2026-11-11-phase1-foundation-design.md` (WS0). WS1–WS6 get their own plans after this one lands, because they depend on WS0's diagnosis data.

## Global Constraints

- Windows 10/11 only. No admin required for normal use.
- Two-process isolation stays: no FFmpeg/WASAPI work in the Tauri process. The recorder crate must **not** depend on `tauri` (enforced by the dependency graph).
- Recorder **stdout is IPC only** (newline-delimited JSON). Logs never go to stdout.
- No behaviour changes to capture/replay/hotkey logic in WS0 (those are WS1/WS2). Refactors must be behaviour-preserving, proven by the characterization tests.
- Tests and harness runs must never read/write the owner's real `~/.cliplite` or real video folders: always set `CLIPLITE_HOME`.
- Never force-push or rewrite history; push feature branches freely; never push to `main`.
- Commit messages: conventional commits, ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Everything added must be used (no empty scaffolding).

## Review Focus

1. **Paths with spaces.** The owner's home is `C:\Users\Ah Eeee`, and the repo lives under `Y:\CODING PROJECTS`. Every path passed to FFmpeg, ffprobe, `CreateProcessW` or scripts must survive spaces. Test: Task 1's app-dir test and Task 7's runner tests use a `CLIPLITE_HOME` containing a space.
2. **Recorder stdout pollution.** A stray `println!`/log on stdout corrupts IPC. Test: Task 4 integration test spawns the recorder binary, sends `{"cmd":"status"}`, and asserts every stdout line parses as an `Event`.
3. **Fake game stuck in exclusive fullscreen** after a crash, panic or Ctrl+C leaves the owner's display unusable. Test: Task 7 spawns the fake game inside a kill-on-close Job Object, drops the job, and asserts the process exits within 2 s.
4. **Black or frozen clips counted as passes.** `ddagrab` may not see some exclusive-fullscreen surfaces. Test: Task 7's clip validator flags an all-black clip (ffmpeg `color=black` source) as `black: true` and `testsrc2` as `black: false`.
5. **Duplicate saves from one press** (auto-repeat, double handlers). Test: Task 7's trial evaluator marks a trial with more than one new file as `failure: "duplicate"`.

---

## File Structure (end state of WS0)

```
cliplite-tauri/                      (repo root; local folder name unchanged)
├── Cargo.toml                       workspace: members apps/desktop/src-tauri, crates/*, tools/*
├── .gitattributes                   * text=auto eol=lf; *.ps1 eol=crlf; binaries -text
├── .github/workflows/ci.yml         windows-latest: npm build, cargo build/test (zero warnings)
├── scripts/fetch-ffmpeg.ps1         pinned + sha256-verified FFmpeg/ffprobe into apps/desktop/src-tauri/bin
├── apps/desktop/                    moved from repo root: package.json, index.html, src/, vite.config.ts, tsconfig.json, src-tauri/
│   └── src-tauri/src/               Tauri-only code: commands.rs, library.rs, settings.rs, tray.rs, hotkey_listener.rs,
│                                    recorder.rs (spawn/IPC to recorder child only), lib.rs, main.rs, telemetry wiring
├── crates/cliplite-core/src/
│   ├── lib.rs
│   ├── paths.rs                     app_dir() honouring CLIPLITE_HOME
│   ├── ipc.rs                       Command, Event, Envelope<T>, StartArgs, Region, AudioConfig, EncConfig
│   ├── ffmpeg.rs                    find_ffmpeg/find_ffprobe, detect_hw_encoder, build_record_command_with_pipe
│   ├── audio.rs                     device enumeration + WASAPI capture (from rec_audio.rs + audio.rs enum)
│   └── telemetry.rs                 init_json_logging(), Stage, new_cid()
├── crates/cliplite-recorder/src/    main.rs (from recorder_bin.rs), recorder.rs (from rec_recorder.rs)
├── tools/testkit/src/
│   ├── bin/fakegame.rs              D3D11 load generator
│   ├── bin/hil.rs                   CLI: matrix | soak | bench
│   ├── job.rs  input.rs  procmon.rs  clip.rs  harness.rs  elevated.rs
└── docs/benchmarks/raw/baseline/    captured "before" data (Task 9)
```

Deleted: `src-recorder/` (unreferenced), `src-tauri/build_output.txt`, `src-tauri/src/rec_*.rs`, dead functions listed in Task 4.

---

### Task 1: Instrument the baseline build (isolated app dir + test-mode auto-start)

Runs on a branch **off the `baseline` tag**, in the old layout, so the "before" build differs from what the owner runs only by this disclosed instrumentation.

**Files:**
- Modify: `src-tauri/src/settings.rs` (add `pub fn app_dir() -> PathBuf`; use it at line ~78)
- Modify: `src-tauri/src/library.rs:40`, `src-tauri/src/lib.rs:30,64`, `src-tauri/src/commands.rs:960` (use `crate::settings::app_dir()`)
- Modify: `src-tauri/src/recorder_bin.rs` (`get_app_dir()` honours `CLIPLITE_HOME`)
- Modify: `src-tauri/src/lib.rs` (`setup`: test-mode auto-start)

**Interfaces:**
- Produces: env contract used by Tasks 7–9:
  - `CLIPLITE_HOME=<dir>`: replaces `~/.cliplite` for settings, library DB, logs, thumbnails and replay temp, in **both** processes. The recorder inherits the env through `CreateProcessW`. Verify `lpEnvironment` is null (inherit) in `spawn_recorder_isolated`.
  - `CLIPLITE_TEST_MODE=1`: after setup, auto-start the replay buffer from persisted Settings (`selected_monitor` → region via the same code `get_monitors` uses; audio ids from settings; buffer length `replay_buffer_duration_secs`, treat 0 as 30).

- [ ] **Step 1: Create the branch**

```bash
git checkout -b baseline-instrumented baseline
```

- [ ] **Step 2: Write the failing test** in `src-tauri/src/settings.rs` (`#[cfg(test)] mod tests`)

```rust
#[test]
fn app_dir_honours_cliplite_home_with_spaces() {
    let dir = std::env::temp_dir().join("clip lite home");
    std::env::set_var("CLIPLITE_HOME", &dir);
    assert_eq!(app_dir(), dir);
    std::env::remove_var("CLIPLITE_HOME");
    assert_eq!(app_dir(), dirs::home_dir().unwrap().join(".cliplite"));
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cd src-tauri && cargo test --lib app_dir_honours`
Expected: FAIL to compile, `cannot find function app_dir`.

- [ ] **Step 4: Implement `pub fn app_dir() -> PathBuf`** (returns `CLIPLITE_HOME` if set and non-empty, else `~/.cliplite`) and route every `.cliplite` reference listed above through it. The recorder's `get_app_dir()` gets the same logic (a separate binary, so the duplication is accepted until Task 4).

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --lib app_dir_honours`
Expected: PASS

- [ ] **Step 6: Implement test-mode auto-start in `lib.rs` `setup`.** When `CLIPLITE_TEST_MODE == "1"`, spawn an async task that waits for `EncoderCache` to be non-empty (max 10 s), then calls the existing `commands::start_replay(app, region, audio_cfg, enc_cfg, app.state(), app.state(), app.state())` with args built from Settings. Log `"[test-mode] replay auto-started"` or the error.

- [ ] **Step 7: Manual verification**

Run (PowerShell, from `src-tauri`):
```powershell
cargo build
$env:CLIPLITE_HOME="$env:TEMP\clip lite home"; $env:CLIPLITE_TEST_MODE="1"; .\target\debug\cliplite.exe
```
Expected: `"$env:TEMP\clip lite home\debug.log"` contains `[test-mode] replay auto-started`; `...\temp\cliplite_replay_*.mkv` grows; the real `~/.cliplite/debug.log` is untouched (mtime unchanged).

- [ ] **Step 8: Commit, then merge into the Phase 1 branch**

```bash
git add -A src-tauri/src && git commit -m "test: add CLIPLITE_HOME override and test-mode replay auto-start for harness"
git push -u origin baseline-instrumented
git checkout phase1/foundation && git merge --no-ff baseline-instrumented
```

---

### Task 2: Characterization tests (pin current behaviour before moving code)

**Files:**
- Modify: `src-tauri/Cargo.toml` (`[dev-dependencies] insta = "1"`)
- Create: `src-tauri/src/rec_ffmpeg_tests.rs` (included from `rec_ffmpeg.rs` via `#[cfg(test)] #[path = "rec_ffmpeg_tests.rs"] mod tests;`)
- Modify: `src-tauri/src/recorder_bin.rs` (`#[cfg(test)] mod ipc_golden`)
- Modify: `src-tauri/src/hotkey_listener.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: snapshot files under `src-tauri/src/snapshots/`. Task 4 moves them with the code, and they must pass **unchanged** there.

- [ ] **Step 1: Write the FFmpeg arg snapshot tests** (`#[cfg(windows)]`). Use a fixed ffmpeg path `C:\\ff mpeg\\ffmpeg.exe` and output `C:\\out dir\\clip.mp4`. Cases form a matrix:

```rust
// encoders × audio × region; plus format "webm" and "gif" with h264_nvenc + sys audio
for enc in ["h264_nvenc", "libx264", "h264_amf", "h264_qsv"] {
  for audio in [AudioCase::None, AudioCase::Sys, AudioCase::SysAndMic] {
    for (w, h) in [(1920, 1080), (2560, 1440)] {
      insta::assert_debug_snapshot!(format!("{enc}_{audio:?}_{w}x{h}"),
        build_record_command_with_pipe(/* ... */, r"\\.\pipe\cliplite_sysaudio_replay"));
    }
  }
}
```

- [ ] **Step 2: Write the IPC golden tests.** The JSON strings are copied from what `commands.rs` sends today. Find each `serde_json::json!` call for `start`, `start_replay`, `save_replay`, `stop`, `stop_replay`, `pause`, `resume` and `status`, and copy its exact shape. For each string, assert that it deserializes to the expected `Command` variant. Assert `serde_json::to_string(&Event::ReplaySaved { path: "C:\\a b\\c.mp4".into() })` equals `{"event":"replay_saved","path":"C:\\a b\\c.mp4"}`, and do the same for every other `Event` variant.

- [ ] **Step 3: Write the hotkey parse tests** against the existing `parse_hotkey_to_vk`:

```rust
assert_eq!(parse_hotkey_to_vk("CommandOrControl+Shift+F"), Some((0x46, true, true, false)));
assert_eq!(parse_hotkey_to_vk("Alt+F9"), Some((0x78, false, false, true)));
assert_eq!(parse_hotkey_to_vk(""), None);
assert_eq!(parse_hotkey_to_vk("Shift+"), None);
```
Return-tuple order is `(vk, ctrl, shift, alt)`. If the existing function disagrees with an assertion, **the test records current behaviour**: change the assertion, not the function, and note the discrepancy for WS1.

- [ ] **Step 4: Run and accept snapshots**

Run: `cd src-tauri && cargo insta test --accept --lib --bins` (install with `cargo install cargo-insta` if needed), then `cargo test`
Expected: all PASS; `src-tauri/src/snapshots/*.snap` created. Read three snapshots and confirm they match the live FFmpeg command shape (e.g. NVENC has `-cq`, `-preset p2`).

- [ ] **Step 5: Commit**

```bash
git add src-tauri && git commit -m "test: characterize ffmpeg args, IPC wire format and hotkey parsing"
```

---

### Task 3: Cargo workspace, app move, repo hygiene, FFmpeg fetch, minimal CI

Pure move plus hygiene. No code logic changes.

**Files:**
- Move (`git mv`): `package.json package-lock.json index.html src/ vite.config.ts tsconfig.json src-tauri/` → `apps/desktop/`
- Create: `Cargo.toml` (workspace, `resolver = "2"`, members `["apps/desktop/src-tauri"]`, growing in Task 4/6)
- Create: `.gitattributes`, `scripts/fetch-ffmpeg.ps1`, `.github/workflows/ci.yml`
- Modify: `.gitignore` (`/target/`, `apps/desktop/src-tauri/bin/`, `apps/desktop/src-tauri/gen/`, `node_modules/`, `dist/`)
- Delete: `src-recorder/`, `src-tauri/build_output.txt`
- Modify: `ARCHITECTURE.md`, `QUICKSTART.md` (paths and commands only; the full rewrite is WS5)

**Interfaces:**
- Produces: build outputs in `<root>/target/{debug,release}/` (`cliplite.exe`, `cliplite-recorder.exe`). Later tasks reference these paths.
- Produces: `scripts/fetch-ffmpeg.ps1` → `apps/desktop/src-tauri/bin/{ffmpeg.exe,ffprobe.exe}`.

- [ ] **Step 1: Move files and create the workspace manifest.** Move `[profile.*]` sections, if any, to the root manifest. `tauri.conf.json` relative paths (`../dist`) keep working because `src/` and `src-tauri/` move together. Confirm `src-recorder/` is unreferenced with `grep -r src-recorder` (outside `target/`) before deleting it.

- [ ] **Step 2: Write `scripts/fetch-ffmpeg.ps1`.** It downloads a **pinned** `ffmpeg-<ver>-essentials_build.zip` from `github.com/GyanD/codexffmpeg/releases`, verifies a hard-coded SHA-256, and extracts `ffmpeg.exe` and `ffprobe.exe` into `-Dest` (default `apps/desktop/src-tauri/bin/`, resolved relative to the script's repo root). It is idempotent (skips when the hashes match) and quotes every path. Pick the newest release that is at least 2 weeks old and record the version and hash at the top of the script. Then confirm the essentials build lists `h264_nvenc`, `h264_amf`, `h264_qsv` and `libx264` in `ffmpeg -hide_banner -encoders`. If any is missing, switch to the `full_build` asset and record why.

- [ ] **Step 3: Add `.gitattributes` and renormalize**

Run: `git add --renormalize . && git status --short | head`
Expected: line-ending-only changes staged; no CRLF warnings on later `git add`.

- [ ] **Step 4: Verify build and tests from the new layout**

Run: `powershell -File scripts/fetch-ffmpeg.ps1; cd apps/desktop; npm ci; npm run build; cd ../..; cargo test --workspace`
Expected: all Task 2 tests PASS with snapshots unchanged; `target/debug/cliplite.exe` and `target/debug/cliplite-recorder.exe` exist.

- [ ] **Step 5: Manual smoke test**

Run: `cd apps/desktop; npx tauri dev`
Expected: the window opens, a 5 s recording (click Record/Stop) produces a playable MP4, the replay buffer starts from its button, and saving produces a playable MP4.

- [ ] **Step 6: Add `.github/workflows/ci.yml`.** On `windows-latest`, triggered by push and pull_request:
  - checkout
  - `actions/setup-node` with Node 22 and an npm cache keyed on `apps/desktop/package-lock.json`
  - `dtolnay/rust-toolchain@stable`
  - `Swatinem/rust-cache`
  - `npm ci && npm run build` in `apps/desktop` (`generate_context!` needs `dist/`)
  - `cargo test --workspace`

  The zero-warnings build step is added in Task 4, once the dead code is gone.

  FFmpeg is not needed in CI: the snapshot tests don't execute it.

- [ ] **Step 7: Commit and push; verify CI**

```bash
git add -A && git commit -m "build: move app to apps/desktop under a Cargo workspace; add ffmpeg fetch script and CI"
git push -u origin phase1/foundation
gh run watch --exit-status
```
Expected: CI green.

---

### Task 4: Extract `cliplite-core` and `cliplite-recorder`; remove duplication and dead code

**Files:**
- Create: `crates/cliplite-core/{Cargo.toml,src/lib.rs,src/paths.rs,src/ipc.rs,src/ffmpeg.rs,src/audio.rs}`
- Create: `crates/cliplite-recorder/{Cargo.toml,src/main.rs,src/recorder.rs}` (binary name `cliplite-recorder`)
- Move: snapshot files and characterization tests into `crates/cliplite-core` (ffmpeg, ipc) and keep the hotkey tests in `apps/desktop`
- Modify: `apps/desktop/src-tauri/Cargo.toml` (drop `[[bin]]`; depend on `cliplite-core`), `commands.rs`, `recorder.rs`, `lib.rs`
- Modify: `apps/desktop/package.json` scripts (build the recorder before Tauri)
- Delete: `apps/desktop/src-tauri/src/{recorder_bin.rs,rec_recorder.rs,rec_audio.rs,rec_ffmpeg.rs}`

**Interfaces:**
- Produces (`cliplite_core`):
  - `paths::app_dir() -> PathBuf` (Task 1 semantics)
  - `ipc::{Command, Event, StartArgs, Region, AudioConfig, EncConfig}`, with the same wire format as the Task 2 golden tests
  - `ffmpeg::{find_ffmpeg() -> anyhow::Result<PathBuf>, find_ffprobe() -> anyhow::Result<PathBuf>, detect_hw_encoder() -> String, build_record_command_with_pipe(...) -> Vec<String>}`, with signatures as today in `rec_ffmpeg.rs`/`ffmpeg.rs`
  - `audio::{AudioDevice, list_input_devices(), list_output_devices(), AudioFormat, SysAudioCapture}`
- `cliplite-core` must not depend on `tauri`. The recorder depends only on `cliplite-core` plus what its own code needs.
- The Tauri side sends `ipc::Command` values via `serde_json::to_string` instead of `serde_json::json!`.

- [ ] **Step 1: Move code into `cliplite-core`.** Where the two copies differ (`audio.rs` vs `rec_audio.rs`, Tauri `ffmpeg.rs` vs `rec_ffmpeg.rs`), the **recorder's live version wins** for capture and arg building, and the Tauri version wins for discovery/enumeration. If a type differs between the Tauri-side command parameters (`commands::Region` etc., which come from the frontend) and the IPC types, keep the frontend-facing types in `commands.rs` and convert explicitly.

- [ ] **Step 2: Delete dead code reported by `cargo check`.** At minimum, remove these items flagged on 2026-11-12:
  - Tauri `ffmpeg::{build_record_command, add_video_input, add_mic_input, spawn_ffmpeg, elevate_priority}`
  - Tauri `audio::{SysAudioCapture, capture_loop}`
  - `RecorderInner::{process, sys_audio, recorder_proc}` and the fallback paths using them
  - `replay_is_active`, `start`, `duration_str`, `close_handles`, `handle_hotkey`
  - unused imports and variables

  Remove each item only after confirming it has no callers.

- [ ] **Step 3: Add the recorder stdout integration test** in `crates/cliplite-recorder/tests/stdout_is_ipc.rs`

```rust
#[test]
fn every_stdout_line_is_an_event() {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cliplite-recorder"))
        .env("CLIPLITE_HOME", std::env::temp_dir().join("clip lite ipc test"))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    writeln!(child.stdin.as_mut().unwrap(), r#"{{"cmd":"status"}}"#).unwrap();
    writeln!(child.stdin.as_mut().unwrap(), r#"{{"cmd":"bogus"}}"#).unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    let lines: Vec<_> = String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect();
    assert_eq!(lines.len(), 2);
    for l in &lines { serde_json::from_str::<cliplite_core::ipc::Event>(l).expect(l); }
}
```
(`Event` gains `Deserialize`.)

- [ ] **Step 4: Wire the build scripts.** `apps/desktop/package.json`:
  - `"dev": "cargo build -p cliplite-recorder && vite"`
  - `"build": "cargo build -p cliplite-recorder --release && tsc && vite build"`
  - `"typecheck": "tsc --noEmit"`

  `find_recorder()` keeps looking next to `cliplite.exe` (both land in `<root>/target/<profile>/`).

- [ ] **Step 5: Run everything**

Run: `cargo test --workspace` and `$env:RUSTFLAGS="-Dwarnings"; cargo build --workspace --all-targets`
Expected: PASS; snapshot files byte-identical to Task 2's (`git diff --stat` shows renames only for `.snap` files); zero warnings.

- [ ] **Step 6: Add the `-Dwarnings` build step to CI**, then repeat the Task 3 Step 5 manual smoke test.

- [ ] **Step 7: Commit and push; verify CI**

```bash
git add -A && git commit -m "refactor: extract cliplite-core and cliplite-recorder crates; remove duplicated and dead code"
git push && gh run watch --exit-status
```

---

### Task 5: Structured JSON logs with correlation ids across both processes

**Files:**
- Create: `crates/cliplite-core/src/telemetry.rs`
- Modify: `crates/cliplite-core/src/ipc.rs` (envelope with `cid`)
- Modify: `apps/desktop/src-tauri/src/{lib.rs,hotkey_listener.rs,commands.rs,recorder.rs}`, `crates/cliplite-recorder/src/main.rs`
- Modify: `apps/desktop/src/{api.ts,components/RecorderView.tsx,App.tsx}` (pass `cid` through)
- Modify: `apps/desktop/src-tauri/Cargo.toml`, `crates/*/Cargo.toml` (drop `env_logger`; add `tracing`, `tracing-subscriber` with `json`+`env-filter`, `tracing-appender`, `uuid` with `v4`)

**Interfaces:**
- Produces (`cliplite_core::telemetry`):
  - `init_json_logging(file_stem: &str) -> tracing_appender::non_blocking::WorkerGuard`. It writes `app_dir()/logs/<file_stem>.jsonl` with daily rotation, keeping 7 files, using `tracing_subscriber::fmt().json().flatten_event(true)`. Existing `log::` macros flow in through `tracing-log`. It installs a panic hook that logs `error` with the panic message and location. File stems: `"app"` and `"recorder"`.
  - `new_cid() -> String`: a UUID v4 in simple (32 hex) form.
  - `enum Stage { HotkeyPressed, HotkeyDispatched, CommandReceived, IpcSent, RecorderCommand, FfmpegSpawned, FfmpegExited, ReplaySaved, Error }` with `as_str()` returning `"hotkey.pressed"`, `"hotkey.dispatched"`, `"command.received"`, `"ipc.sent"`, `"recorder.command"`, `"ffmpeg.spawned"`, `"ffmpeg.exited"`, `"replay.saved"`, `"error"`.
  - Convention: `tracing::info!(stage = Stage::X.as_str(), cid = %cid, ...extra fields)`. Every JSON line then has top-level `timestamp`, `level`, `target`, `stage`, `cid`.
- Produces (`cliplite_core::ipc`): `Envelope<T> { cid: Option<String>, body: T }`, serialized flat: `{"cmd":"stop","cid":"abc"}` and `{"event":"stopped","path":"…","cid":"abc"}`. A missing `cid` → `None`, so old JSON still parses.
- Tauri ↔ React: hotkey events carry the payload `{ cid: string }`. `saveReplay(secs, cid?)`, `startReplay(..., cid?)`, `stopReplay(cid?)`, `startRecording(..., cid?)` and `stopRecording(cid?)` pass it to Rust. Commands generate a new cid when none is given.

- [ ] **Step 1: Write the failing tests** in `crates/cliplite-core`

```rust
#[test] fn envelope_with_and_without_cid() {
    let e: Envelope<Command> = serde_json::from_str(r#"{"cmd":"stop","cid":"abc"}"#).unwrap();
    assert_eq!(e.cid.as_deref(), Some("abc")); assert!(matches!(e.body, Command::Stop));
    let e: Envelope<Command> = serde_json::from_str(r#"{"cmd":"stop"}"#).unwrap();
    assert!(e.cid.is_none());
    let s = serde_json::to_string(&Envelope { cid: Some("x".into()), body: Event::ReplayStopped }).unwrap();
    assert_eq!(s, r#"{"event":"replay_stopped","cid":"x"}"#);
}
#[test] fn stage_line_is_flat_json() {
    // capture with a MakeWriter into a Vec<u8>; emit info!(stage = Stage::ReplaySaved.as_str(), cid = "c1", path = "p")
    // parse the single line: v["stage"] == "replay.saved", v["cid"] == "c1", v["path"] == "p", v["level"] == "INFO"
}
#[test] fn new_cid_is_32_hex() { let c = new_cid(); assert_eq!(c.len(), 32); assert!(c.chars().all(|ch| ch.is_ascii_hexdigit())); }
```
All Task 2 golden tests must still pass on the inner `Command`/`Event`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p cliplite-core`
Expected: FAIL to compile (`Envelope`, `Stage` and `new_cid` are undefined).

- [ ] **Step 3: Implement `telemetry.rs` and `Envelope`.** Hand-write `Serialize`/`Deserialize` for `Envelope` if `#[serde(flatten)]` on an internally tagged enum rejects the golden shapes.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: PASS

- [ ] **Step 5: Wire the stages.**
  - Hotkey hook: log `hotkey.pressed` with a new cid, and emit `{cid}` to the window, logging `hotkey.dispatched`. Leave the callback's structure otherwise as-is; changing it is WS1.
  - Each Tauri command: log `command.received` with `name`.
  - Writing to the recorder child: log `ipc.sent`.
  - Recorder: log `recorder.command`, `ffmpeg.spawned` (pid and args), `ffmpeg.exited` (code) and `replay.saved` (path and bytes), plus `error` on failures.
  - Replace `env_logger` init in `lib.rs` and the recorder's `eprintln!` calls with `tracing`. Keep the returned `WorkerGuard` alive for the process lifetime.

- [ ] **Step 6: Manual verification**

Run the app with `CLIPLITE_HOME` and `CLIPLITE_TEST_MODE=1` set, then press Ctrl+Shift+F with the ClipLite window focused.
Expected: `logs/app.jsonl` and `logs/recorder.jsonl` contain the same cid on `hotkey.pressed` → `hotkey.dispatched` → `command.received` → `ipc.sent` → `recorder.command` → `replay.saved`.

- [ ] **Step 7: Commit and push**

```bash
git add -A && git commit -m "feat: structured JSON logging with cross-process correlation ids"
git push
```

---

### Task 6: Fake game (D3D11 load generator)

**Files:**
- Create: `tools/testkit/Cargo.toml` (package `cliplite-testkit`, `publish = false`), `tools/testkit/src/bin/fakegame.rs`, `tools/testkit/src/fakegame/{mod.rs,render.rs,cpu.rs,frametimes.rs}`
- Modify: root `Cargo.toml` members

**Interfaces:**
- Produces the CLI:
  `fakegame --mode <windowed|borderless|exclusive> [--width 1920 --height 1080] [--gpu-load <0-100>, default 70] [--cpu-threads <n>, default 0] [--duration-secs <n>, default 0 = until killed] [--frametimes <path.csv>] [--vsync]`
- On start, after the swapchain is created and the window is focused, it prints exactly one stdout line: `READY hwnd=0x<hex> fullscreen=<true|false>`. Here `fullscreen` is the result of `IDXGISwapChain::GetFullscreenState`.
- Frametime CSV header: `frame,t_ms,frametime_ms`, one row per Present.
- Window title: `ClipLite Test Load`. Visuals: a solid colour cycling through hues plus a horizontal bar sweeping top to bottom once per second, so captured video is never static or black.

- [ ] **Step 1: Write the failing unit tests** for the pure parts

```rust
#[test] fn parses_args() { let a = Args::parse_from(["fakegame","--mode","exclusive","--cpu-threads","4"]);
    assert_eq!(a.mode, Mode::Exclusive); assert_eq!(a.cpu_threads, 4); assert_eq!(a.gpu_load, 70); }
#[test] fn frametime_writer_header_and_rows() { /* write 3 frames to Vec<u8>; assert header "frame,t_ms,frametime_ms" and 4 lines */ }
#[test] fn gpu_load_maps_to_shader_iterations() { assert_eq!(iterations_for(0), 0); assert!(iterations_for(100) > iterations_for(50)); }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p cliplite-testkit`
Expected: FAIL (undefined items)

- [ ] **Step 3: Implement the fake game.**
  - Arguments use `clap` derive. Window and renderer use raw Win32 + D3D11 via the `windows` crate.
  - Draw a fullscreen triangle with a pixel shader compiled at runtime by `D3DCompile`. The shader loops `iterations_for(gpu_load)` times.
  - Use a `DXGI_SWAP_EFFECT_DISCARD` swapchain so that `SetFullscreenState(TRUE)` gives legacy exclusive fullscreen in `exclusive` mode.
  - `borderless` = a `WS_POPUP` window covering the monitor. `windowed` = a 1280×720 `WS_OVERLAPPEDWINDOW`.
  - Present with sync interval 0 unless `--vsync` is set.
  - CPU threads busy-spin with `std::hint::black_box`.
  - On WM_CLOSE, Ctrl+C or duration elapsed: call `SetFullscreenState(FALSE)`, then exit.

- [ ] **Step 4: Run the unit tests and verify each mode manually**

Run: `cargo test -p cliplite-testkit`, then `cargo run -p cliplite-testkit --bin fakegame -- --mode exclusive --duration-secs 5 --frametimes "$env:TEMP\ft test.csv"`
Expected: tests PASS; the screen goes fullscreen with a moving bar for 5 s, then the desktop returns normally; stdout `READY ... fullscreen=true`; CSV has more than 100 rows. Repeat for `borderless` and `windowed`.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test: add D3D11 fake game load generator for hardware-in-the-loop tests"
```

---

### Task 7: HIL runner — hotkey matrix (including the elevated helper)

**Files:**
- Create: `tools/testkit/src/bin/hil.rs`, `tools/testkit/src/{lib.rs,job.rs,input.rs,clip.rs,harness.rs,elevated.rs,matrix.rs}`

**Interfaces:**
- Consumes: Task 1 env contract; the Task 6 `fakegame` CLI and `READY` line; the Task 5 log format, which is optional because the baseline build has no JSON logs.
- Produces the CLI:
  `hil matrix --app <path\to\cliplite.exe> [--trials 5] [--elevated] [--hotkey Ctrl+Shift+F] [--buffer-secs 30] --out <dir>`
- `harness::Harness::start(app: &Path, out: &Path, buffer_secs: u32) -> Result<Harness>`:
  - creates `<out>/home` (`CLIPLITE_HOME`)
  - writes a settings file there with the replay buffer at `buffer_secs`, replay output dir `<out>/home/clips`, monitor 0 and default hotkeys
  - launches the app with `CLIPLITE_HOME` and `CLIPLITE_TEST_MODE=1` inside a kill-on-close Job Object
  - is ready when a `cliplite-recorder.exe` child is alive and 10 s have passed
  - `Drop` kills everything (Job Object)
- `job::Job::new_kill_on_close() -> Result<Job>`; `job.spawn(cmd) -> Result<Child>`.
- `input::send_chord("Ctrl+Shift+F") -> Result<()>`: `SendInput` with key down for modifiers then key, then key up in reverse order.
- `clip::validate(ffprobe: &Path, ffmpeg: &Path, file: &Path) -> ClipReport { duration_s: f64, has_video: bool, has_audio: bool, black: bool }`. `black` = ffmpeg `blackdetect=d=0.5:pix_th=0.10` covers ≥ 90% of the duration.
- `matrix::evaluate_trial(new_files: &[PathBuf], report: Option<&ClipReport>, expected_min_s: f64) -> TrialResult`, where:
  - `TrialResult { ok: bool, failure: Option<&'static str>, latency_ms: Option<u64>, file: Option<PathBuf>, duration_s: Option<f64> }`
  - failures: `"no_file"`, `"duplicate"`, `"invalid"`, `"black"`, `"too_short"`
- Results: `<out>/matrix.json` with `{ build: { app_path, git_sha (from env GIT_SHA or "unknown") }, machine: { gpu, cpu }, started_at, cells: [{ mode, elevated, window: "visible"|"hidden", cpu: "idle"|"saturated", trials: [TrialResult] }] }`, plus `<out>/summary.md` (one row per cell, `passed/total`).
- Cells: mode ∈ {windowed, borderless, exclusive} × window ∈ {visible, hidden} × cpu ∈ {idle (`--cpu-threads 0`), saturated (`--cpu-threads <logical cores>`)}, so 12 cells. `--elevated` runs the same 12 with an elevated fake game.
- Per trial:
  1. Snapshot the clips dir.
  2. Launch the fake game for this cell and wait for `READY` (15 s timeout). Confirm `GetForegroundWindow()` equals its hwnd (else `failure: "no_focus"`).
  3. Wait 5 s.
  4. `send_chord`.
  5. Poll for new `.mp4` files every 250 ms for 15 s, then wait for the size to be stable for 1 s.
  6. Validate the clip.
  7. Close the fake game (WM_CLOSE, then kill after 3 s). Pause 2 s.

  "Hidden" = post `WM_CLOSE` to the ClipLite main window (it hides to tray) before the cell. "Visible" = `ShowWindow(SW_SHOWNOACTIVATE)`. `expected_min_s` = `min(buffer_secs, seconds since harness ready) - 2`. The app stays up across trials: a buffer that doesn't re-arm after a save is a real failure to record.
- Elevated mode: `elevated::Helper::launch() -> Result<Helper>` runs `hil elevated-helper --pipe <name>` via `ShellExecuteExW` with verb `runas`, which prompts UAC once. The helper accepts newline JSON over a named pipe: `{"op":"launch_fakegame","args":[...]}` → `{"hwnd":"0x..","pid":n}`, `{"op":"send_chord","chord":"Ctrl+Shift+F"}` → `{"ok":true}`, and `{"op":"close","pid":n}`. Input must come from the elevated helper, because UIPI blocks `SendInput` from medium integrity into an elevated foreground window. ClipLite itself stays medium integrity (launched by the non-elevated runner).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn trial_no_file()   { assert_eq!(evaluate_trial(&[], None, 10.0).failure, Some("no_file")); }
#[test] fn trial_duplicate() { assert_eq!(evaluate_trial(&[p("a.mp4"), p("b.mp4")], None, 10.0).failure, Some("duplicate")); }
#[test] fn trial_black()     { assert_eq!(evaluate_trial(&[p("a.mp4")], Some(&rep(30.0, true)), 10.0).failure, Some("black")); }
#[test] fn trial_too_short() { assert_eq!(evaluate_trial(&[p("a.mp4")], Some(&rep(3.0, false)), 10.0).failure, Some("too_short")); }
#[test] fn trial_ok()        { assert!(evaluate_trial(&[p("a.mp4")], Some(&rep(29.5, false)), 10.0).ok); }
#[test] fn chord_parse()     { assert_eq!(parse_chord("Ctrl+Shift+F").unwrap(), vec![VK_CONTROL, VK_SHIFT, VK_F]); }
#[test] #[ignore = "needs ffmpeg; run in hil self-test"]
fn validator_detects_black() { /* ffmpeg -f lavfi -i color=black:s=320x240:d=3 → black=true; -i testsrc2=d=3 → black=false; paths under a temp dir containing a space */ }
#[test] fn job_kills_child_on_drop() { /* spawn `fakegame --mode windowed` in Job; drop Job; assert process exited within 2 s */ }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p cliplite-testkit`
Expected: FAIL (undefined items)

- [ ] **Step 3: Implement the modules and the `matrix` and `elevated-helper` subcommands.** Also add `hil self-test`, which runs the `#[ignore]` validator tests against `apps/desktop/src-tauri/bin`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p cliplite-testkit` then `cargo run -p cliplite-testkit --bin hil -- self-test`
Expected: PASS

- [ ] **Step 5: Smoke run against the current branch build.** Ask the owner to step away for ~3 minutes first.

Run: `cargo build --workspace; cargo run -p cliplite-testkit --bin hil -- matrix --app target\debug\cliplite.exe --trials 1 --out "target\hil\smoke run"`
Expected: completes, the display returns to the desktop, and `summary.md` has 12 rows. Pass/fail counts are data, not a gate (the hotkey bug is expected to show up here).

- [ ] **Step 6: Commit and push**

```bash
git add -A && git commit -m "test: add HIL runner with fullscreen hotkey matrix and elevated helper"
git push
```

---

### Task 8: HIL runner — soak and bench

**Files:**
- Create: `tools/testkit/src/{procmon.rs,soak.rs,bench.rs}`; extend `bin/hil.rs`

**Interfaces:**
- Consumes: `Harness`, `Job`, `send_chord`, `clip::validate` and `evaluate_trial` from Task 7, plus `fakegame` from Task 6.
- `procmon::sample(names: &[&str]) -> Vec<ProcSample>` with `ProcSample { name, pid, private_bytes, working_set, handles, cpu_time_ms, write_bytes }`. It uses ToolHelp32 for enumeration plus `GetProcessMemoryInfo` (`PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`), `GetProcessHandleCount`, `GetProcessTimes` and `GetProcessIoCounters`. Tracked names: `cliplite.exe`, `cliplite-recorder.exe`, `ffmpeg.exe`.
- `hil soak --app <exe> [--hours 2.0] [--save-every-mins 10] [--sample-every-secs 5] [--warmup-mins 10] --out <dir>` runs the harness plus `fakegame --mode borderless --gpu-load 50` and writes:
  - `procs.csv` (`t_s,name,pid,private_bytes,working_set,handles,cpu_time_ms,write_bytes`)
  - `disk.csv` (`t_s,temp_dir_bytes`), where `temp_dir_bytes` is the recursive size of `CLIPLITE_HOME/temp`
  - `saves.csv` (`t_s,ok,failure,latency_ms,duration_s,file`)
  - `soak.json`: `{ pass, slope_mb_per_hour, band_mb, max_temp_dir_mb, saves_ok, saves_total }`
- Soak pass criteria (after warm-up, on the total private bytes of all tracked processes):
  - least-squares slope < **5 MB/hour**
  - max − min < **100 MB**
  - `max(temp_dir_bytes)` ≤ `max_replay_buffer_size_mb × 1.1`
  - every save ok
- `soak::verdict(samples: &[(f64, u64)], temp_max: u64, cap_mb: u32, saves: &[TrialResult], warmup_s: f64) -> SoakVerdict` is pure and unit-tested.
- `hil bench --app <exe> [--runs 5] [--duration-secs 60] [--mode exclusive] [--gpu-load 100] --out <dir>` runs the scenarios `baseline` (ClipLite not running), `replay` (harness running) and `replay_record`. For `replay_record`, it sends `Ctrl+Shift+R` while the desktop has focus, confirms recording started (a growing new file in the output dir within 10 s), and only then launches the fake game. It writes `<scenario>/run<k>/frametimes.csv.gz` and `procs.csv`, plus `bench.json` with per-run `{ avg_fps, p1_low_fps, p99_frametime_ms, frametime_std_ms, cliplite_cpu_pct, cliplite_private_mb }`. Analysis and charts belong to WS4. Here we only compute these summaries with the pure function `bench::summarize(frametimes_ms: &[f64]) -> FrameStats`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn verdict_flat_passes() { /* 2h of samples at 400MB ±10MB noise → pass, |slope| < 5 */ }
#[test] fn verdict_leak_fails()  { /* +50MB/hour linear → pass=false, slope≈50 */ }
#[test] fn verdict_disk_over_cap_fails() { /* temp_max = 600MB, cap 500 → fail */ }
#[test] fn verdict_ignores_warmup() { /* big growth only in first 10 min → pass */ }
#[test] fn frame_stats() {
    let mut ft = vec![10.0; 99]; ft.push(30.0);           // mean 10.2 ms
    let s = summarize(&ft);
    assert!((s.avg_fps - 1000.0 / 10.2).abs() < 0.01);
    assert_eq!(s.p99_frametime_ms, 10.0);                  // nearest rank: ceil(0.99*100)=99th value
    assert!((s.p1_low_fps - 1000.0 / 30.0).abs() < 0.01);  // slowest 1% = [30.0]
}
```
Define 1% low as 1000 / (mean of the slowest 1% of frametimes) and p99 as the nearest-rank 99th percentile, and write those definitions in a doc comment on `summarize`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p cliplite-testkit`
Expected: FAIL

- [ ] **Step 3: Implement `procmon`, `soak` and `bench`.**

- [ ] **Step 4: Run the tests plus a short soak and bench**

Run: `cargo test -p cliplite-testkit`, then `hil soak --app target\debug\cliplite.exe --hours 0.1 --save-every-mins 2 --warmup-mins 1 --out "target\hil\soak smoke"`, then `hil bench --app target\debug\cliplite.exe --runs 1 --duration-secs 15 --out "target\hil\bench smoke"`
Expected: tests PASS; the CSVs and JSON exist and are well-formed; the display is restored after each run.

- [ ] **Step 5: Commit and push**

```bash
git add -A && git commit -m "test: add soak and benchmark modes to HIL runner"
git push
```

---

### Task 9: Capture the "before" data from `baseline-instrumented`

**Files:**
- Create: `docs/benchmarks/raw/baseline/{matrix/,soak/,bench/}`, `docs/benchmarks/raw/baseline/README.md` (build, commit sha, machine, date, exact commands, and the instrumentation disclosure: "baseline = `final_updates` + owner WIP (`baseline` tag) + CLIPLITE_HOME/test-mode only")

- [ ] **Step 1: Build the baseline in a separate worktree**

```bash
git worktree add ../cliplite-baseline baseline-instrumented
powershell -File scripts/fetch-ffmpeg.ps1 -Dest "../cliplite-baseline/src-tauri/bin"
cd ../cliplite-baseline/src-tauri && npm --prefix .. ci && npm --prefix .. run build && cargo build --release
```
Expected: `../cliplite-baseline/src-tauri/target/release/cliplite.exe` and `cliplite-recorder.exe` exist.

- [ ] **Step 2: Ask the owner to step away (~3.5 hours total, or split across sessions), then run the matrix, bench and soak**

```powershell
$app = "..\cliplite-baseline\src-tauri\target\release\cliplite.exe"; $env:GIT_SHA = (git rev-parse baseline-instrumented)
cargo run --release -p cliplite-testkit --bin hil -- matrix --app $app --trials 5 --out docs\benchmarks\raw\baseline\matrix
cargo run --release -p cliplite-testkit --bin hil -- bench  --app $app --runs 5 --duration-secs 60 --out docs\benchmarks\raw\baseline\bench
cargo run --release -p cliplite-testkit --bin hil -- soak   --app $app --hours 2 --out docs\benchmarks\raw\baseline\soak
```
Expected: all three complete; JSON summaries exist. Failing cells and soak `pass: false` are **expected findings**, not errors.

- [ ] **Step 3: Elevated cells.** Ask the owner to approve one UAC prompt.

Run: `... hil matrix --app $app --trials 5 --elevated --out docs\benchmarks\raw\baseline\matrix-elevated`
Expected: completes.

- [ ] **Step 4: Write the baseline README** with a findings summary: which hotkey cells fail and at which logged stage, soak slope/band, temp-dir growth, and save failures after the first save. This is the input for the WS1 and WS2 plans. Delete clip files from the results; keep only JSON, CSV and summaries. Check that the total size added is under 20 MB.

- [ ] **Step 5: Commit and push**

```bash
git add docs/benchmarks/raw/baseline && git commit -m "docs: capture baseline hotkey matrix, soak and benchmark data"
git push
git worktree remove ../cliplite-baseline
```

---

## Not in this plan (later plans)

- WS1 hotkey fix: written from Task 9's matrix findings.
- WS2 replay ring, WS3 encoders, WS4 benchmark analysis and charts (consumes Task 8/9 data), WS5 full CI/README/ADRs/licensing, WS6 post kit.
- The parent-folder files (`LW_Clipper/build_and_test.ps1`, `LW_Clipper/test_recordings/`) are outside the repo and stale (the script references a missing `test_hotkeys.ps1`). They are left untouched; tell the owner they can delete them.
