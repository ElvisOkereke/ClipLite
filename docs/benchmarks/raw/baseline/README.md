# Baseline ("before") measurements

Captured 2026-11-12 with the hardware-in-the-loop kit (`tools/testkit`, `hil`).

## What was measured

- **Build:** branch `baseline-instrumented` (`1dd2eee`), which is the owner's daily build (`final_updates` + uncommitted hotkey WIP, tag `baseline`) plus **test instrumentation only**: a `CLIPLITE_HOME` data-dir override and `CLIPLITE_TEST_MODE=1` auto-start of the replay buffer. There are no changes to capture, replay or hotkey logic. Release build.
- **FFmpeg:** gyan.dev essentials 8.1.1, the same pinned build as the "after" runs, so only code changes differ.
- **Machine:** AMD Ryzen 7 9800X3D, NVIDIA RTX 3070 (driver 596.21) + AMD iGPU, 32 GB RAM, Windows 10 Home, 2560×1440 display.
- **Isolation:** every run used its own `CLIPLITE_HOME` and WebView2 profile. Audio was set to system loopback plus a mic, as a typical user would have it. Capture ran at 30 fps (the app's default).

Commands (from the repo root):

```powershell
powershell -File scripts\run-hil-suite.ps1 -App ..\cliplite-baseline\src-tauri\target\release\cliplite.exe -Out docs\benchmarks\raw\baseline -SkipElevated
target\release\hil.exe bench --app ..\cliplite-baseline\src-tauri\target\release\cliplite.exe --runs 5 --duration-secs 60 --out docs\benchmarks\raw\baseline\bench
target\release\hil.exe soak  --app ..\cliplite-baseline\src-tauri\target\release\cliplite.exe --hours 0.15 --save-every-mins 4 --warmup-mins 1 --out docs\benchmarks\raw\baseline\soak-4min
```

The benchmark was re-run on its own (with the launch-retry fix) after the suite's first attempt aborted on one fake-game launch.

## Results

### Hotkey matrix: `matrix/` (12 cells × 5 trials, game not elevated)

**60/60 passed.** The save hotkey fired and produced a valid, non-black clip in windowed, borderless and exclusive fullscreen, with ClipLite visible or hidden in the tray, and with the CPU idle or saturated. Latency is mostly 0.25–2 s.

Every clip after the first in a cell is only as long as the time since the previous save (8.5–8.8 s). That's because the baseline stops the buffer to save and then restarts it, so older footage is lost.

The **elevated** cells (game running as administrator) still need an owner-approved UAC prompt. Run `hil matrix --elevated`.

### Benchmark: `bench/` (5 × 60 s per scenario, fake game borderless, GPU-bound)

| Scenario | Avg fps | 1% low fps | p99 frame time | ClipLite CPU | ClipLite RAM |
|---|---|---|---|---|---|
| ClipLite off | 80.7 ± 0.6 | 69.9 ± 1.4 | 13.7 ± 0.2 ms | n/a | n/a |
| Replay buffer | 78.7 ± 0.2 (−2.5 %) | 63.8 ± 1.0 (−8.7 %) | 15.1 ± 0.1 ms | 1.0 % of one core | 653 MB |
| Replay + recording | 78.8 ± 0.2 | 62.3 ± 0.6 (−10.9 %) | 15.4 ± 0.1 ms | 1.5 % of one core | 1,208 MB |

ClipLite's numbers cover its whole process tree: the UI, the WebView2 renderers, the recorder(s) and FFmpeg. Recording on top of the replay buffer starts a **second, independent capture and encoder**, which roughly doubles the RAM.

Exclusive fullscreen was not benchmarked. There, the baseline's capture can die (see finding 1), which would understate its cost.

### Soak: `soak/` (2 h, replay buffer + fake game, save every 10 min)

- **Memory: flat.** The least-squares slope was 4.1 MB/h and the band was 13 MB (after a 10 min warm-up).
- **Disk:** the replay temp file grows to about 138 MB per 10 minutes, then resets on each save (because of the stop-and-restart).
- **Saves: 11/11 produced clips, but each took 21.2–21.35 s** and gave 70–72 s instead of the 60 s requested. The harness's then-20 s wait marked these as `no_file` in `soak.json`/`saves.csv`. The real latencies were reconstructed from the hotkey timestamps in `home/debug.log` and the clip completion times, and are in `soak/saves_reconstructed.json`.

**Equal-age comparison (save latency after 10 min of buffering).** Latency here means *hotkey press → clip file finished*:
- **Baseline:** the `[hotkeys] FIRED` timestamp in `debug.log`, compared with the clip file's last-write time.
- **Phase 1:** the `hotkey.pressed` and `replay.saved` log events, which share the same `cid`.

| Build | Hotkey → clip finished | Clip length (60 s requested) | Data |
|---|---|---|---|
| Baseline | 21.2–21.35 s (11/11 saves) | 70–72 s | `soak/saves_reconstructed.json` |
| Phase 1 WS0 (FFmpeg stderr drained) | 0.29 s, 0.71 s | 61.2–61.6 s | `../phase1-ws0-checks/phase1-soak-10min/` |

The harness's own `latency_ms` measures something else: *chord → clip file first appears*, polled every 250 ms. At a 4-minute buffer age both builds save in under 1 s on that measure (`soak-4min/`, `../phase1-ws0-checks/phase1-soak-check/`). So the stall appears somewhere between 4 and 10 minutes of buffering.

## Findings handed to WS1–WS3

1. **Capture dies in exclusive fullscreen (intermittent, load-dependent).** FFmpeg's `ddagrab` gets `AcquireNextFrame failed: 887a0026` (`DXGI_ERROR_ACCESS_LOST`) when a game takes exclusive fullscreen, and exits. Nothing supervises it, so the replay buffer is silently dead. The hotkey still fires, but saves contain only pre-death footage.
   - Reproduced standalone (FFmpeg + fake game, no ClipLite) and in a benchmark at GPU load 100.
   - Capture survived 20/20 exclusive trials at GPU load 50.
   - This is the leading explanation for the owner's "doesn't work in fullscreen" reports.
2. **20 s save stall from an undrained stderr pipe.** The baseline recorder pipes FFmpeg's stderr but never reads it. Once the pipe fills (between 4 and 10 min in these runs), FFmpeg's main thread blocks and never sees `q`. Saving then waits the recorder's 20 s stop timeout, kills FFmpeg, and extracts from an unfinalized MKV. This is fixed in WS0 (stderr is drained into the recorder log).
3. **Saving stops and restarts the buffer.** Older footage is lost at every save, and there is a capture gap during each restart (the clip lengths above show it).
4. **Hotkeys are ignored while ClipLite's own window has focus.** The low-level keyboard hook is never invoked then. Debug builds hide this, because DevTools takes focus.
5. **Mic-only recording writes nothing.** With only a microphone selected (which the UI auto-selects when no audio is configured), FFmpeg can't open `\\.\pipe\cliplite_sysaudio`, and no file or error is shown. Not yet root-caused.
6. **Recording during replay doubles resources.** There are two captures and two encoders (about 1.2 GB in total).

## Files

- `suite.log`: transcript of the unattended suite.
- `matrix/`: `matrix.json` (per-trial results) and `summary.md`.
- `bench/`: `bench.json` (per-run stats); `<scenario>/runN/frametimes.csv.gz` and `procs.csv`.
- `soak/`, `soak-4min/`: `procs.csv`, `disk.csv`, `saves.csv`, `soak.json`, plus `saves_reconstructed.json` for the 2 h run.
- `*/home/debug.log`, `settings.json`: the app's own log and the isolated settings used.

Clips, replay temp files and WebView2 profiles were deleted to keep the repo small. User-profile paths and device names were redacted.
