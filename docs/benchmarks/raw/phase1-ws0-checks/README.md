# Phase 1 WS0 confirmation runs

These short soaks ran against the Phase 1 WS0 release build (`phase1/foundation`). They test the baseline finding that saves stall about 21 s once the replay buffer has run for a while. The cause is that the baseline recorder never drains FFmpeg's stderr, so FFmpeg's main thread blocks and never sees the `q` stop command. WS0 drains stderr.

The machine, FFmpeg build and harness settings are the same as `../baseline/README.md`. Commands, run from the repo root:

```powershell
target\release\hil.exe soak --app target\release\cliplite.exe --hours 0.35 --save-every-mins 10 --warmup-mins 1 --out "target\hil\phase1 soak 10min"
target\release\hil.exe soak --app target\release\cliplite.exe --hours 0.15 --save-every-mins 4  --warmup-mins 1 --out "target\hil\phase1 soak check"
```

| Run | Buffer age at save | Hotkey → `replay.saved` (same `cid`) | Clip length |
|---|---|---|---|
| `phase1-soak-10min` | 10 min, 20 min | 0.29 s, 0.71 s | 61.6 s, 61.2 s |
| `phase1-soak-check` | 4 min, 8 min | 0.49 s, 0.69 s | 61.7 s, 61.3 s |

The baseline saved in 21.2 s at a 10-minute buffer age.

The `pass: false` in each `soak.json` is expected: these runs are too short to judge memory. Their slopes (37–52 MB/h) are a few MB of warm-up, extrapolated. The 2-hour baseline soak is the memory reference.

Clips, temp files and WebView2 profiles were removed. User paths and device names were redacted.
