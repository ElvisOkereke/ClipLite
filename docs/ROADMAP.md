# ClipLite roadmap

**Vision:** an always-on clipper that runs entirely on your PC. It catches the moments for you, lets you find any moment by what was said or shown, and turns clips into something worth posting, without costing your game frames. Every claim is backed by a reproducible test or benchmark in this repo.

This is also a public portfolio project. Each phase ships something measurable and ends with a write-up.

Status key: ✅ done · 🔄 in progress · ⏳ planned

---

## Phase 1: foundation, reliability, measured performance

Design: [`docs/superpowers/specs/2026-11-11-phase1-foundation-design.md`](superpowers/specs/2026-11-11-phase1-foundation-design.md)

| Workstream | Goal | Status |
|---|---|---|
| **WS0** · Baseline, restructure, observability, test kit | Cargo workspace, a recorder crate that can't link WebView2, characterization tests, JSON logs with correlation ids, the D3D11 fake game, and the `hil` matrix/soak/bench runner. "Before" data is captured. | ✅ [PR #2](https://github.com/ElvisOkereke/Clips.gg/pull/2) · [baseline report](benchmarks/raw/baseline/README.md) |
| **WS1** · In-game hotkeys | Hotkeys that fire in every display mode, including games running as administrator. Measure `RegisterHotKey` alongside the low-level hook, plus an opt-in elevated mode. Handle actions in Rust, not the UI. | ⏳ |
| **WS2** · Replay buffer rewrite | A fixed-size ring buffer (bounded RAM and disk) that keeps recording through a save. Capture that survives exclusive fullscreen (supervise and restart on `DXGI_ERROR_ACCESS_LOST`, or switch to Windows Graphics Capture). One shared capture for replay and recording. On by default. | ⏳ |
| **WS3** · Encoders and compatibility | NVENC / AMF / QSV / x264 with tuned settings. Probe the real pipeline at startup, fall back automatically when an encoder fails to open, explain driver mismatches in plain language, and offer a "copy diagnostics" button. | ⏳ |
| **WS4** · Benchmarks | Before/after charts generated reproducibly from the raw data (frame times, 1% lows, RAM, save latency). | ⏳ |
| **WS5** · Docs and CI | Decision records, a full architecture refresh, licensing notes, a lint/format/MSRV gate. | 🔄 (README and CI done) |
| **WS6** · Write-up | Post #1: the test rig, the bugs it found, the 28× faster saves. | ⏳ |

**Found by the WS0 test rig and handed to WS1–WS3:**
1. Capture dies with `DXGI_ERROR_ACCESS_LOST` in exclusive fullscreen, and nothing restarts it.
2. Hotkeys reach ClipLite 1/60 times while an elevated game has focus (UIPI), versus 60/60 for a normal game.
3. Saving stops and restarts the buffer, so older footage is lost.
4. Hotkeys are ignored while ClipLite's own window has focus.
5. Mic-only recording writes nothing.
6. Recording during replay runs a second capture (about 1.2 GB RAM).

A seventh, a 21 s save stall caused by an undrained FFmpeg stderr pipe, is already fixed in WS0: saves now take under 1 s.

## Phase 2: local AI that finds your clips ⏳

- Transcribe every clip on-device (Whisper-family models through ONNX Runtime with GPU acceleration; heavy jobs are deferred until the game closes).
- Auto-generate titles, tags and summaries from the transcript plus sampled frames.
- Meaning-based search over the library from a local vector index (speech and frame embeddings), jumping to the exact second.
- A library UI redesign around search.

## Phase 3: live highlight detection and the MLOps layer ⏳

- Detect highlights in real time while the replay buffer runs: audio events such as laughter, shouting and loudness spikes, plus visual cues. Save them automatically within a frame-time budget.
- **MLOps:**
  - a labelled evaluation set of clips
  - a versioned model registry with checksummed downloads
  - quantized models (INT8)
  - accuracy-versus-latency dashboards
  - CI that blocks changes which regress model quality or ClipLite's performance overhead

  The HIL rig from WS0 provides the performance half of those gates.

## Phase 4: cloud sharing ⏳

- One-click share links that play inline in Discord and X.
- A shared library for a friend group, with Discord sign-in.
- Stack: Rust/Axum on Fly.io, Cloudflare R2 for video (no egress fees), Postgres (Neon) with pgvector, Terraform, monitoring, per-user quotas.
- Expected cost at friend-group scale: about $0–15 per month.

## Phase 5: Shorts and launch ⏳

- Automatic vertical 9:16 Shorts that keep the action in frame, with burned-in captions from the transcript, and weekly highlight montages.
- A signed installer, auto-update and a landing page. The launch demo video is made by ClipLite itself.

---

## Principles

- **Measure before and after.** No performance or reliability claim goes public without data and a reproducible command in `docs/benchmarks/`.
- **Local first.** AI runs on the user's GPU; the cloud is optional and only for sharing.
- **Never cost the player frames.** Overhead is benchmarked on a GPU-bound test load in every phase.
- **Honest scope.** The README separates what is built from what is planned.
