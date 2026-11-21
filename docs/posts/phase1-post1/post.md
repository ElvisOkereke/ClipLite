# Post #1: drafts

Images, in order: `1-results-card.png`, `2-save-latency.png`, `3-hotkey-reach.png`, `4-soak-memory.png`. To regenerate them from the committed data: `uv run docs/posts/phase1-post1/make_charts.py`.

Every number below is backed by `docs/benchmarks/raw/`. Rewrite the wording in your own voice, but keep the numbers exactly as they are.

---

## LinkedIn (long form)

**My screen recorder's hotkeys "didn't work in fullscreen." Instead of guessing, I built a verification loop to find out why.**

ClipLite is my Windows instant-replay clipper, built in Rust and Tauri. Before adding features, I wanted every claim about it to be measured, so I built a **hardware-in-the-loop (HIL) test harness**:

🎮 A DirectX 11 "fake game" that runs windowed, borderless, or in true exclusive fullscreen, with tunable GPU and CPU load.
⌨️ Synthetic hotkeys injected with SendInput, including games running as administrator, through a UAC-elevated helper over a locked-down named pipe.
✅ Automated verification of every clip: ffprobe, black-frame and freeze detection, coverage checks, and duplicate-save detection.
📈 Frame-time benchmarks (avg fps, 1% lows, p99) and 2-hour soak tests, run unattended and fully isolated with Windows Job Objects.
🔎 Observability: structured JSON logs with correlation IDs that trace each hotkey press across processes, from hook to UI to IPC to recorder to FFmpeg.

What the root-cause analysis turned up:
• **Clip saves are 30× faster: 21.2 s → 0.71 s.** FFmpeg's stderr pipe was never drained, so FFmpeg blocked and only died when a 20-second timeout killed it.
• **The "fullscreen" bug was actually two bugs.** Windows UIPI blocks a normal-privilege keyboard hook while an admin game has focus: 1 of 60 presses got through, versus 60/60 with a normal game. And DXGI_ERROR_ACCESS_LOST kills screen capture in exclusive fullscreen.
• **No memory leak.** Memory was flat over 2 hours (4.1 MB/h).
• **The overhead is quantified.** The replay buffer costs 2.5% average fps and 8.6% on 1% lows.

Along the way I refactored the codebase into a Rust workspace, guarded by characterization and snapshot tests. The build now has CI that allows zero warnings, reproducible benchmarks, and pinned, checksum-verified dependencies.

Next: fixing both root causes and measuring the before and after with the same rig. Then on-device AI: local transcription and semantic search over clips, with an evaluation pipeline and model-quality gates in CI.

Code, data and methods: https://github.com/ElvisOkereke/ClipLite

#Rust #SystemsProgramming #PerformanceEngineering #Observability #TestAutomation #SoftwareEngineering #MLOps

---

## LinkedIn (short form)

Hotkeys in my Rust screen recorder "didn't work in fullscreen." So I built a hardware-in-the-loop verification loop: a DirectX fake game, synthetic input, automated clip validation, frame-time benchmarks and 2-hour soak tests, plus cross-process tracing with correlation IDs.

What the root-cause analysis found:
• Clip saves are 30× faster (21.2 s → 0.71 s). The cause was an undrained FFmpeg pipe.
• The "fullscreen" bug was two bugs: Windows UIPI (1/60 hotkeys with an admin game) and DXGI capture loss.
• No memory leak (flat over 2 h), and the overhead is quantified (−2.5% fps).

Data and code: https://github.com/ElvisOkereke/ClipLite
#Rust #PerformanceEngineering #Observability #TestAutomation

---

## X / Twitter thread

1/ My screen recorder's hotkeys "didn't work in fullscreen." Instead of guessing, I built a hardware-in-the-loop test rig to find out why. Here's what it found 🧵 [image 1]

2/ The rig, all in Rust:
- a DirectX 11 fake game (windowed, borderless, or exclusive fullscreen)
- synthetic hotkeys through SendInput, including admin games via a UAC helper
- every clip auto-verified (ffprobe, black and freeze detection)
- frame-time benchmarks and 2 h soaks, isolated with Job Objects

3/ Clip saves got 30× faster, 21.2 s → 0.71 s. The recorder never drained FFmpeg's stderr pipe. Once it filled, FFmpeg blocked and ignored "stop", so every save waited out a 20 s timeout. [image 2]

4/ The "fullscreen" bug was really two bugs:
- Windows UIPI blocks a normal-privilege keyboard hook while an admin game has focus: 1/60 presses, vs 60/60 [image 3]
- DXGI_ERROR_ACCESS_LOST kills capture in exclusive fullscreen

5/ The things I suspected turned out fine: no memory leak (flat over 2 h), and the replay buffer costs 2.5% avg fps. Measuring first saved me from fixing the wrong thing. [image 4]

6/ Next: fix both root causes and prove it with the same rig, then add on-device AI search with an eval pipeline. Code and raw data: https://github.com/ElvisOkereke/ClipLite

---

## Keywords this post covers (for recruiter search)

Hardware-in-the-loop (HIL) testing · verification loop · test automation · root cause analysis · performance engineering · benchmarking (p99, 1% lows) · soak / reliability testing · observability · structured logging · distributed tracing (correlation IDs) · Rust · systems programming · Windows internals (Win32, UIPI, Job Objects, named pipes) · DirectX 11 / DXGI · FFmpeg · GPU encoding (NVENC) · IPC · CI/CD · regression and snapshot testing · reproducibility · dependency pinning · MLOps (as the stated next step).

**Keep it honest:** the AI features are *next*, not done. The post says so, and the README's "For reviewers and AI agents" section separates built from planned.
