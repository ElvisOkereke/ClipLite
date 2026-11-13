# ClipLite Phase 1 — Foundation, Reliability & Measured Performance

**Date:** 2026-11-11
**Status:** Draft for owner approval (builder check done; owner answers incorporated 2026-11-12)
**Scope:** Phase 1 of 5 (see "Roadmap context")

---

## 1. Intent — why this exists

ClipLite is a Windows screen recorder / instant-replay clipper (Tauri v2 + Rust + React). The owner uses it daily for gaming, Discord moments with friends, and live events. It works, but it is about to become the owner's flagship public portfolio piece, used on social media as proof of **senior-level engineering**, aimed at **MLOps, ML Engineer and Forward-Deployed Engineer** roles.

Phase 1 makes the foundation trustworthy and *measured* before AI features are layered on in later phases. Every claim made publicly ("works in fullscreen games", "flat memory", "<X% FPS cost") must be backed by a reproducible test or benchmark in the repo. The phase ends with material for the owner's first public post.

The owner wants to post **as soon as possible**, so Phase 1 is deliberately scoped to ship fast, and each workstream should leave the app in a working state.

The owner wants the testing, diagnosis and fixing loop to be done **autonomously by the agent** as much as possible. The owner will (a) step away while screen-takeover tests run, (b) approve a UAC prompt for elevated test cases, and (c) run a short manual checklist in real anti-cheat games.

### Roadmap context (not in this phase)
1. **Phase 1 (this doc):** foundation, reliability, performance, repo.
2. Phase 2: local AI — transcription, auto-naming/tagging, semantic search; library UI redesign.
3. Phase 3: live highlight detection + MLOps layer (eval set, model registry, quantization, CI quality/perf gates).
4. Phase 4: cloud — share links that unfurl in Discord, shared group library, Discord login (Cloudflare R2, Rust/Axum on Fly.io, Neon Postgres, Terraform).
5. Phase 5: Shorts/montage generator + signed installer, auto-updater, landing page, launch.

Phase 1 decisions should not paint later phases into a corner — in particular, the recorder should be able to expose recent audio/video to an analysis component later (Phase 3 live highlight detection).

---

## 2. Current state (as of 2026-11-11)

Repo: `cliplite-tauri/` is the git root (remote `github.com/ElvisOkereke/ClipLite`). Parent folder `LW_Clipper/` is *not* a repo and holds `build_and_test.ps1` (references a missing `test_hotkeys.ps1`) and an empty `test_recordings/`.

Current branch `final_updates` (**8 commits ahead of `main`**, including the "separate replay buffer into its own process" refactor, mic auto-select and replay save fixes). `final_updates` + the WIP below is what the owner actually runs. Other branches: `fixing_keybinds`, `global-hotkey`.

**Uncommitted work in progress** (must be preserved, not discarded): `hotkey_listener.rs` rewritten from `RegisterHotKey` (global-hotkey crate) to a `WH_KEYBOARD_LL` low-level hook; `Cargo.toml`/`Cargo.lock` drop `global-hotkey` and `tauri-plugin-global-shortcut`; `RecorderView.tsx` changes.

### Architecture (keep)
- `cliplite.exe` — Tauri UI process (WebView2 + React).
- `cliplite-recorder.exe` — standalone Rust binary (`src-tauri/src/recorder_bin.rs`), no WebView2, spawned with `CREATE_BREAKAWAY_FROM_JOB`; runs FFmpeg (`ddagrab` capture) + WASAPI audio (piped to FFmpeg over a named pipe). IPC = newline-delimited JSON over stdin/stdout. This two-process design was a hard-won fix for D3D11 contention with WebView2 and **must be kept**.
- `ARCHITECTURE.md` documents this in detail but is partly stale (still describes `RegisterHotKey` hotkeys).

### Known problems found in code review
1. **Hotkeys don't fire in fullscreen applications** (owner report). In the WIP hook (`hotkey_listener.rs`), the hook callback does `try_lock` on a mutex (silently drops keys on contention), logs, and calls `win.emit()` into the WebView *inside the hook callback*. The actual action is then performed by React (`RecorderView.tsx` listens for `hotkey-*` events and invokes commands). Candidate causes, unconfirmed:
   - (a) Action depends on the hidden/occluded WebView2, which is throttled/suspended while a fullscreen game has focus.
   - (b) Slow hook callback → Windows silently removes `WH_KEYBOARD_LL` hooks that exceed `LowLevelHooksTimeout`.
   - (c) Game runs elevated / under anti-cheat → UIPI blocks a medium-integrity process from seeing its input.
   - Also: modifier state read via `GetAsyncKeyState` inside the hook can be stale.
2. **Replay buffer** (`recorder_bin.rs`, `StartReplay`/`SaveReplay`): FFmpeg writes one ever-growing MKV temp file (`~/.cliplite/temp/cliplite_replay_<pid>.mkv`). Size limit cleanup only runs at start. Unbounded disk growth while running (owner reports "memory leaking replay buffer" — exact mechanism to be confirmed by soak test). To save, it **stops** the writer, extracts the last N seconds with stream copy and deletes the temp file; the recorder itself does not restart. A Rust-side restart exists in `commands.rs::save_replay`, but it rebuilds start args incorrectly (hardcoded 1920×1080 region at ~line 700, drops the mic name). So every save leaves a gap, and the restarted buffer may capture the wrong region/audio.
2b. **Capture config lives in React.** The UI builds start args for every action (region from monitor list, audio from UI state, including an auto-selected first mic that is never persisted, encoder). The replay buffer is off by default (`replay_buffer_duration_secs: 0`) and starts only from a UI button. `max_replay_buffer_size_mb` (default 500) caps temp disk usage.
3. **Duplicated code**: `ffmpeg.rs`/`rec_ffmpeg.rs`, `audio.rs`/`rec_audio.rs`, `recorder.rs`/`rec_recorder.rs` are parallel copies ("mirrors cliplite_lib::ffmpeg but uses local types"). `src-recorder/main.rs` appears unreferenced (the `cliplite-recorder` bin points at `src-tauri/src/recorder_bin.rs`) — verify, then remove.
4. **Encoders**: detection exists (`detect_hw_encoder`, `EncoderCache`), but arg building only special-cases `h264_nvenc` and `libx264`; anything else gets a generic fallback. `h264_nvenc` already takes `ddagrab`'s D3D11 frames directly (no download); only `libx264` downloads frames.
5. Repo hygiene: tracked `src-tauri/build_output.txt`; no CI; no tests; no `.gitattributes` (CRLF warnings); README missing (only ARCHITECTURE.md / QUICKSTART.md); commit messages informal; repo name doesn't match product.

Dev machine: Windows 10 Home, RTX 3070 (+ AMD iGPU), Ryzen 7 9800X3D, 32 GB RAM.

---

## 3. Goals

1. Hotkeys work reliably in **windowed, borderless and exclusive-fullscreen** applications, with ClipLite's window visible or hidden in the tray, and under CPU load. Elevated-game behaviour is understood and handled (fixed or clearly surfaced to the user).
2. Replay buffer uses **bounded memory and disk regardless of uptime**, and **keeps recording through a save** (no gap, no re-arm needed).
3. Hardware encoding is selected automatically and correctly on **NVIDIA (NVENC), AMD (AMF) and Intel (QSV)**, with CPU (libx264) as last resort; frames stay on the GPU wherever the encoder allows.
4. ClipLite's performance cost on a running game is **measured, reproducible and published** (charts in README).
5. The repo reads as senior-engineered: clean structure, no duplication, CI, tests, accurate docs, decision records.
6. The owner has a ready-to-edit Post #1 kit.

## 4. Non-goals (Phase 1)
- Any AI/ML features.
- Any cloud/online component.
- Installer, code signing, auto-updater.
- Visual redesign of the UI (planned for Phase 2). Small UX fixes required by the work above are fine (e.g. surfacing an "elevated game" warning, showing replay buffer status).
- macOS/Linux support. Existing `cfg(not(windows))` branches may be kept or removed, but no effort goes into making them work.

## 5. Anti-goals (failure even if it "works")
- Regressing anything that works today: recording, pause/resume, audio (system + mic mix), library, trim, settings, tray.
- Making in-game FPS or frame-time stability worse than today.
- **Publishing numbers that aren't reproducible from the repo**, or benchmark charts produced from a single run without variance shown.
- Automating input into real anti-cheat-protected games (ban risk for the owner).
- Losing the owner's uncommitted WIP, force-pushing, or rewriting published git history.
- "Enterprise cosplay": empty scaffolding, placeholder packages, or tooling that isn't actually used. Everything added must earn its place.
- Breaking the two-process isolation (no FFmpeg/WASAPI work inside the Tauri process).

## 6. Constraints
- Windows 10/11 only. Tauri v2, Rust, React 18/TypeScript stay. FFmpeg (external binary) stays as the capture/encode engine for Phase 1.
- Must not require admin rights to run normally.
- GitHub-hosted CI runners have no GPU/interactive desktop: GPU/fullscreen tests run locally on the owner's PC; CI runs everything else.
- Hardware-dependent tests take over the screen; they must be runnable as a single command, finish in minutes, clean up after themselves (kill fake game, restore display), and write machine-readable results.
- Open-source license is MIT (existing `LICENSE`); bundled third-party binaries must be license-compliant.

---

## 7. Workstreams

Recommended order: **0 → 1 → 2 → 3 → 4 → 5 → 6.** Workstream 0 comes first because moving files later would cause churn, and the test harness is needed by 1–4.

### WS0 — Baseline, restructure, observability, test kit

**Baseline**
- Commit the owner's WIP as-is on `final_updates` (or a new branch off it) so it's preserved before any changes. Work happens on a new branch (e.g. `phase1/foundation`); merge to `main` via PR at the end of the phase.
- Capture a **"before" benchmark and soak run** (needed for before/after charts in Post #1) once the test kit exists — before fixing the replay buffer. **"Before" = `final_updates` + the owner's WIP** (what the owner actually runs), tagged `baseline`. Since that build has no test mode, add the *minimum* backport needed for the harness to drive it (e.g. a `--test-mode` flag that auto-starts the replay buffer and sets the clip output dir) on a `baseline-instrumented` branch, with **no behaviour changes** to capture/replay/hotkey logic. Methodology docs and posts state this disclosure explicitly. *(agent's call)*

**Repo restructure** *(agent's call on exact names)*
- Move the parent-folder loose files into the repo or delete them (`build_and_test.ps1` references a missing script — replace with the new test commands). The repo root becomes the product root.
- Cargo workspace. Target shape:
  ```
  apps/desktop/            Tauri app (React frontend + src-tauri)
  crates/cliplite-core/    shared types, FFmpeg arg builder, audio, IPC protocol types
  crates/cliplite-recorder/ recorder binary (depends on core)
  tools/testkit/           fake game + HIL test runner
  tools/bench/             benchmark analysis + chart generation
  docs/                    architecture, ADRs, specs, benchmarks
  ```
- Eliminate the duplicated `ffmpeg/audio/recorder` modules into `cliplite-core`; delete dead `src-recorder/`.
- IPC message types defined once in `cliplite-core` and shared by both processes.
- `.gitattributes` for line endings; remove tracked `build_output.txt`.

**Observability**
- Replace `env_logger` with `tracing` + structured JSON log file (rotated, not truncated on launch), in both processes. Every user action (hotkey press, IPC command, FFmpeg spawn/exit, file written) logs with a **correlation id** so one hotkey press can be traced end-to-end across processes. The HIL tests assert on these logs.

**Test kit (hardware-in-the-loop)**
- **Fake game** (`tools/testkit`): native Rust D3D11/DXGI app. Modes: windowed, borderless, exclusive fullscreen (`SetFullscreenState(TRUE)`). Configurable GPU load (draw calls/overdraw) and CPU load (busy threads). Logs present-to-present frame times to CSV. Can be launched elevated.
- **HIL runner**: launches ClipLite (headless-friendly test mode: e.g. `--test-mode` that auto-starts the replay buffer with a test profile and writes clips to a temp dir), launches the fake game in a given mode, waits for focus, injects the hotkey via `SendInput`, asserts (1) a replay file appears within a timeout, (2) it's a valid, playable file of the expected duration (ffprobe), and (3) the log shows each pipeline stage. Writes JSON results + a human-readable summary table.
- **Test matrix:** display mode {windowed, borderless, exclusive FS} × game integrity {normal, elevated} × ClipLite window {visible, hidden to tray} × CPU {idle, saturated}. Elevated cases require one UAC approval per run and are a separate, opt-in command.
- **Soak test**: replay buffer running N hours (default 2h, configurable) with the fake game; samples ClipLite processes' private bytes / working set, handle count, temp-dir disk usage, and fake-game frame times every few seconds; periodic saves during the run. Pass = memory and disk stay within a bounded band after warm-up; every save valid.
- The fake game must be clearly a test tool (no resemblance to a real game).

**Done when:** workspace builds, app behaves as before, `cargo test` + frontend checks pass, HIL runner and soak test run end-to-end on the dev machine, "before" numbers captured.

### WS1 — Hotkeys in fullscreen

1. **Diagnose first** (systematic, evidence-based): run the HIL matrix against the current build, use the correlation-id logs to identify which stage fails in which cells, and confirm/refute causes (a)/(b)/(c). Record findings in an ADR — this write-up is also post material.
2. **Architecture change (decided regardless of diagnosis):** hotkey actions are executed **in Rust**, never dependent on the WebView. The hook callback does the minimum (track modifier state from its own key events, match, push a message onto a channel, return immediately — no locks that can block, no logging, no IPC inside the callback). A dispatcher thread maps the hotkey to an action and sends the command directly to the recorder process; the UI is notified afterwards for display/toasts.
3. Hook resilience: guard against silent hook removal (e.g. periodically verify/reinstall, or re-install on suspicion), and keep the hook thread's message pump healthy.
4. Elevated games: decide based on diagnosis. Options the builder may choose between: detect that the foreground process is elevated and show a clear one-time notice with an opt-in "run ClipLite as administrator" path; or another mechanism that achieves reliability without requiring admin by default. Must not require admin for normal use.
5. **Rust owns capture config.** Persisted Settings become the single source of truth for capture args (region/monitor, audio devices, encoder, buffer length). Any UI selection, including the auto-selected mic, is persisted immediately. Rust builds start args from Settings for every action, whether triggered by hotkey, tray or UI. The broken `commands.rs::save_replay` restart (hardcoded region) is removed as part of WS2. *(agent's call)*
6. Key repeat: holding a hotkey must trigger once.
6. Unit tests for hotkey string parsing and matching logic.

**Done when:** all non-elevated matrix cells pass 20/20 repeated trials; elevated cells pass or are explicitly handled with user-facing messaging; owner's manual real-game check passes (see §8).

### WS2 — Replay buffer rewrite

**Requirements**
- Memory and disk bounded by `buffer_seconds × bitrate` (+ small constant), independent of uptime.
- Recording continues through a save — no gap, no restart, no UI round-trip.
- Save of 60 s completes in < 2 s on the dev machine, by stream copy (no re-encode).
- Saved clips: valid MP4, correct duration (±1 keyframe interval), A/V in sync, playable in common players and the existing trim dialog.
- Survives audio device failure/absence (video-only fallback, as today).
- Buffer length configurable (existing settings); changing it doesn't require an app restart.
- Design must allow a later consumer (Phase 3 highlight detector) to read recent media.
- **Always-on by default (owner decision):** the replay buffer starts automatically when ClipLite launches (including launched/minimised to tray), so the save hotkey works without ever opening the window. A setting turns it off. Tray icon/tooltip reflects buffer state. The settings migration changes the default for existing installs from `0` (disabled) to a sensible duration (e.g. 60 s) unless the user had explicitly configured it.
- **One capture pipeline (owner decision):** a single `ddagrab` capture + single encoder session in the recorder process feeds the replay ring. Manual **recording becomes a second consumer of the same encoded stream**, writing MP4 (stream copy) from the next/most recent keyframe; pause/resume pauses that consumer. No second capture, no second encoder. Recording therefore uses the same encoder settings as the buffer. **WebM and GIF are no longer recording formats; they become export options** (re-encode) from the library/trim dialog. The settings migration maps existing `webm`/`gif` format preferences to `mp4`. The recorder process remains separate from the Tauri process (two-process isolation unchanged).

**Design** *(my call: in-memory ring as the default; builder may fall back after a spike)*
- FFmpeg encodes continuously with a fixed GOP (keyframe every 1 s, as today via `-g fps`) and outputs a streamable container (e.g. MPEG-TS) to a pipe read by the recorder process.
- The recorder keeps a ring of **keyframe-aligned chunks in RAM**, evicting the oldest beyond the configured duration. Avoids continuous SSD writes (~8–16 Mbps continuous ≈ 85–170 GB/day of disk writes with a disk-based buffer).
- Save = snapshot the last N seconds of chunks (cheap, under a short lock), write them out, remux to MP4 via stream copy in a background thread while recording continues.
- **Fallback** if the spike shows chunk parsing too fragile: FFmpeg `segment` muxer writing short segments with `segment_wrap` into a temp dir (bounded on-disk ring), save via concat demuxer + stream copy. Either way, write an ADR comparing both, with the measured trade-off.
- **Size cap** *(agent's call)*: the ring is bounded by both duration and bytes. `max_replay_buffer_size_mb` is repurposed as the ring's memory cap (default 512 MB) and evicts oldest chunks when exceeded. Settings shows the estimated RAM for the chosen duration/bitrate (worst case today ≈ 5 min × 16 Mbps ≈ 600 MB). If the on-disk fallback is chosen, the same cap applies to disk.
- Also remove the old temp-file cleanup path and stale-temp-file handling once unused (keep a one-time cleanup of legacy `cliplite_replay_*.mkv` files).

**Done when:** soak test (≥ 2 h) shows flat memory/disk with periodic saves all valid; save latency target met; no regressions in recording mode; unit tests cover ring eviction and snapshot logic (property tests welcome).

### WS3 — GPU encoding

- Encoder detection probes in order NVENC → AMF → QSV → libx264, caches the result, and exposes it in Settings (with manual override, as today if present).
- Correct, tuned FFmpeg args per encoder (rate control, preset tuned for low latency/low overhead, B-frames where supported, matching GOP for the replay ring). Keep frames on the GPU: `ddagrab` D3D11 frames → NVENC/AMF directly; QSV via the appropriate hw mapping; only libx264 downloads frames.
- H.264 remains the default. HEVC may be offered as an option if trivial; AV1 not in scope (RTX 3070 has no AV1 encoder).
- FFmpeg arg builder gets **snapshot tests** (e.g. `insta`) for every encoder × audio combination so regressions are caught in CI without a GPU.
- AMD/Intel paths can't be hardware-tested on the dev machine (the AMD iGPU may allow an AMF smoke test — builder's call). Untested paths are labeled as such in docs.

**Driver / build compatibility (owner decision, 2026-11-12).** Found during WS0: gyan.dev FFmpeg ≥ 8.1.2 links NVENC API 13.1, which needs NVIDIA driver ≥ 610; on driver 596 `h264_nvenc` fails to open. FFmpeg is pinned to 8.1.1 for now. Users will hit variants of this, so:
- **Probe the real pipeline** at startup (ddagrab D3D11 → encoder, a few frames), not a synthetic encode. Cache results keyed on GPU name + driver version + FFmpeg version; re-probe only when one changes.
- **Automatic fallback at record/replay start:** if the chosen encoder fails to open, retry down the chain (NVENC → AMF → QSV → libx264) instead of surfacing an error. Never lose a clip to an encoder problem.
- **Actionable diagnosis:** parse FFmpeg's failure text (e.g. `Required: 13.1 Found: 13.0`, `minimum required Nvidia driver … 610`) into a structured reason and show a plain-language notice ("NVIDIA encoding unavailable: driver 596 is older than required 610 — using CPU encoding; update your driver for best performance"). Unit-test the parser against captured FFmpeg error strings.
- **"Copy diagnostics"** in Settings: GPU(s), driver versions, FFmpeg version, probe results per encoder, recent log lines — pasteable into a GitHub issue.
- **FFmpeg pin = compatibility contract:** `fetch-ffmpeg.ps1` documents the minimum NVIDIA driver for the pinned build; bumping FFmpeg requires re-running the encoder probe/HIL checks and updating the documented floor.
- **AMD path testable locally:** the dev machine's Ryzen 9800X3D iGPU exposes AMF; the HIL runner should exercise AMF too (selecting the adapter/output as needed), so AMF is not "untested".
- Later option (not Phase 1): build FFmpeg in CI against pinned nv-codec-headers (control the driver floor; possible LGPL build).
- The owner keeps the current driver (596) until Phase 1 before/after numbers are captured, so a driver change doesn't confound the comparison.

**Done when:** NVENC path verified on dev machine with benchmark; snapshot tests for all paths; fallback chain verified by forcing failures (including an encoder that fails to open at start); diagnosis parser tested on real FFmpeg error output; AMF path exercised on the iGPU.

### WS4 — Benchmarks

- Scenarios: **baseline** (fake game only) / **replay buffer on** / **replay buffer + recording on** (one shared capture after Phase 1; two separate captures in the "before" build, which is part of the story), at 1080p60 and 1440p (dev monitor permitting), fake game GPU-bound.
- Metrics: avg FPS, 1% low, p99 frame time, frame-time std dev; ClipLite CPU %, GPU %, RAM (private bytes), disk write rate.
- Multiple runs per scenario (≥ 5), report mean and variance/CI. Raw CSVs committed (or stored as release artifacts if large) alongside the script that generates charts, so anyone can reproduce them.
- Before (`baseline-instrumented`, see WS0) vs after (Phase 1) comparison.
- Optional: document how to measure a real game with PresentMon (manual, by the owner).
- Analysis/charting in Python *(my call — sets up the Python tooling Phase 2/3 ML work will use)*, managed with `uv`, output PNG/SVG charts for README and posts.

**Done when:** one command produces the results table and charts; README shows them with methodology and hardware listed.

### WS5 — Repo & docs makeover

- Rename product/repo to **ClipLite** (GitHub rename `Clips.gg` → `ClipLite` is an outward-facing action: the agent asks the owner before doing it, or the owner does it).
- **README**: one-line pitch, demo GIF/screenshot, features, benchmark chart + headline numbers, architecture diagram (Mermaid), how it works (two-process design, replay ring, hotkey path), install/run, development, testing (unit vs HIL), roadmap (phases 2–5 as "coming"), license.
- **ARCHITECTURE.md** updated to reality (it's currently stale on hotkeys).
- **ADRs** in `docs/adr/`: two-process recorder (retroactive), hotkeys executed in Rust, replay ring design, Cargo workspace, benchmark methodology. Short — context / decision / consequences.
- **CI** (GitHub Actions, `windows-latest`): `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, frontend typecheck + lint + format check, Tauri build (artifact uploaded). `cargo-deny` for licenses/advisories. Fast and green.
- Debug panel excluded from release builds.
- CONTRIBUTING.md (short), CHANGELOG.md, issue/PR templates only if lightweight.
- **FFmpeg licensing**: `src-tauri/bin/` is gitignored; replace manual placement with a script that downloads a **pinned FFmpeg build with checksum verification**; document which build/license (prefer an LGPL build if it has everything needed — `libx264` fallback requires GPL; if GPL is used, document compliance). Add THIRD_PARTY notices.
- Going forward: conventional commit messages. Existing history is **not** rewritten.
- Stale branches (`fixing_keybinds`, `global-hotkey`): ask owner before deleting.

### WS6 — Post #1 kit

- Before/after charts (memory/disk over time for the soak, FPS cost, save latency, hotkey matrix pass rate).
- A short draft write-up (≈ 200–400 words, owner will rewrite in their voice): the problem, how it was diagnosed (the HIL harness), the fix, the numbers. Plus a short "thread" variant.
- A 15–30 s screen capture checklist the owner can record (hotkey working in a fullscreen game, instant save).
- Stored under `docs/posts/` *(my call; owner may prefer outside the repo)*.

---

## 8. Owner's manual checks (only what can't be automated)
- Real-game check, ~5 min per release: press save-replay in 2–3 games the owner actually plays (including at least one with anti-cheat and one exclusive-fullscreen title), verify clips saved and in sync. Agent provides a checklist.
- Approve the UAC prompt when running the elevated test cells.
- Step away from the PC during HIL/benchmark runs.
- Approve outward-facing actions: GitHub rename, branch deletion, merging to `main`.
- **Pushing the Phase 1 feature branch(es) to `origin` is pre-approved by the owner** (2026-11-12), so the agent can iterate on CI until green. Never force-push; never push to `main` directly.

## 9. Left to the builder
- Exact crate/module names and file layout within the target shape.
- Choice of fake-game rendering approach (raw D3D11 via `windows` crate recommended), log format details, test runner language (Rust recommended for `SendInput`/process control).
- The precise hook-resilience mechanism and elevated-game handling (within §WS1 constraints).
- In-memory ring vs on-disk segment ring, after a spike (in-memory preferred).
- Encoder tuning values.
- Which property/snapshot testing crates to use.

## 10. Calls made by the agent (not explicitly decided by the owner)
- Hotkey actions move into Rust regardless of root cause.
- Replay buffer keeps recording through saves; in-memory ring preferred.
- Cargo workspace restructure with shared core crate.
- `tracing` structured logs with correlation ids.
- Python + `uv` for benchmark analysis.
- UI visual redesign deferred to Phase 2.
- Post kit lives in `docs/posts/`.
- "Before" baseline = `final_updates` + WIP with a minimal, disclosed test-mode backport.
- Persisted Settings are the single source of truth for capture config; Rust builds start args.
- Replay ring capped by bytes via repurposed `max_replay_buffer_size_mb` (default 512 MB).
- FFmpeg fetched by pinned, checksummed script.

## 11. Risks
- **Root cause of fullscreen hotkeys is UIPI/anti-cheat** → cannot be fully fixed without elevation; mitigated by clear UX and an opt-in elevated mode.
- **MPEG-TS chunking edge cases** (audio/video interleave, keyframe detection) → spike first; on-disk segment ring fallback.
- **Benchmarks noisy** on a daily-use PC → multiple runs, report variance, close background apps, document conditions.
- **Restructure breaks Tauri build config paths** → do it first, verify build + manual smoke before other work.
- **Exclusive fullscreen tests leave display in a bad state** if a test crashes → runner must always kill the fake game and restore on exit (including on Ctrl+C / panic).
