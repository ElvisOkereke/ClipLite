# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib>=3.8"]
# ///
"""Charts for Post #1, generated only from data committed under docs/benchmarks/raw/.

    uv run docs/posts/phase1-post1/make_charts.py

Writes PNGs (1600x900, social 16:9) next to this script.
"""
from __future__ import annotations

import csv
import json
from collections import defaultdict
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

HERE = Path(__file__).resolve().parent
RAW = HERE.parents[1] / "benchmarks" / "raw"

# Palette: validated (light surface) with the dataviz validator; all checks pass.
SURFACE = "#fcfcfb"
INK = "#0b0b0b"
INK_2 = "#52514e"
GRID = "#e6e5e1"
BEFORE = "#eb6834"  # categorical slot 2 (orange): baseline build
AFTER = "#2a78d6"  # categorical slot 1 (blue): Phase 1 build

plt.rcParams.update({
    "font.family": ["Segoe UI", "DejaVu Sans"],
    "font.size": 15,
    "axes.edgecolor": GRID,
    "axes.labelcolor": INK_2,
    "xtick.color": INK_2,
    "ytick.color": INK_2,
    "figure.facecolor": SURFACE,
    "axes.facecolor": SURFACE,
    "savefig.facecolor": SURFACE,
})
SIZE = (16, 9)
DPI = 100


def frame(ax, title: str, subtitle: str) -> None:
    ax.set_title(title, loc="left", fontsize=26, fontweight="bold", color=INK, pad=46)
    ax.text(0, 1.035, subtitle, transform=ax.transAxes, fontsize=15, color=INK_2, va="bottom")
    for side in ("top", "right"):
        ax.spines[side].set_visible(False)


def footer(fig, text: str) -> None:
    fig.text(0.06, 0.03, text, fontsize=12, color=INK_2)


# ── data ─────────────────────────────────────────────────────────────────────
baseline_saves = json.loads((RAW / "baseline/soak/saves_reconstructed.json").read_text())
lat = sorted(s["latency_s"] for s in baseline_saves)
before_latency = lat[len(lat) // 2]  # median of 11 saves (21.2 s); range 21.21–21.35
after_latency = 0.71  # worst of the two Phase 1 saves at a 10-min buffer age
# (hotkey.pressed → replay.saved, docs/benchmarks/raw/phase1-ws0-checks/README.md)

normal = json.loads((RAW / "baseline/matrix/matrix.json").read_text())
elevated = json.loads((RAW / "baseline/matrix-elevated/matrix.json").read_text())


def passed(m) -> tuple[int, int]:
    trials = [t for c in m["cells"] for t in c["trials"]]
    return sum(t["ok"] for t in trials), len(trials)


n_ok, n_all = passed(normal)
e_ok, e_all = passed(elevated)
# The one elevated press that reached ClipLite saved a clip too late for the
# trial window (see README), so hook reach = FIRED count: 1/60.
e_reach = 1

totals: dict[float, int] = defaultdict(int)
with open(RAW / "baseline/soak/procs.csv", newline="") as f:
    for row in csv.DictReader(f):
        totals[float(row["t_s"])] += int(row["private_bytes"])
soak_t = sorted(totals)
soak_mb = [totals[t] / 1048576 for t in soak_t]
soak = json.loads((RAW / "baseline/soak/soak.json").read_text())["verdict"]

bench = json.loads((RAW / "baseline/bench/bench.json").read_text())["scenarios"]


def mean(xs):
    return sum(xs) / len(xs)


fps = {k: mean([r["stats"]["avg_fps"] for r in v if "stats" in r]) for k, v in bench.items()}
low = {k: mean([r["stats"]["p1_low_fps"] for r in v if "stats" in r]) for k, v in bench.items()}

# ── 1. results card ──────────────────────────────────────────────────────────
fig = plt.figure(figsize=SIZE, dpi=DPI)
fig.text(0.06, 0.88, "ClipLite: what a hardware-in-the-loop test rig found", fontsize=30, fontweight="bold", color=INK)
fig.text(0.06, 0.82, "Automated verification loop: fake DirectX game, synthetic hotkeys, frame-time benchmarks, 2 h soak",
         fontsize=16, color=INK_2)
tiles = [
    (f"{before_latency / after_latency:.0f}×", "faster clip saves",
     f"{before_latency:.1f} s → {after_latency:.2f} s, same 10-min buffer\nroot cause: an undrained FFmpeg\nstderr pipe blocked FFmpeg"),
    (f"{e_reach}/{e_all}", "hotkeys reach ClipLite while\nan admin game has focus",
     f"{n_ok}/{n_all} with a normal game\nroot cause: Windows UIPI blocks\nthe low-level keyboard hook"),
    (f"{soak['slope_mb_per_hour']:.1f} MB/h", "memory drift over a 2 h soak",
     "no leak: flat after warm-up\n11/11 saves produced clips"),
    (f"−{(1 - fps['replay'] / fps['baseline']) * 100:.1f}%", "game fps cost of the replay buffer",
     f"−{(1 - low['replay'] / low['baseline']) * 100:.1f}% on 1% lows\n~1% of one CPU core\nGPU-bound, RTX 3070, 5 × 60 s"),
]
for i, (big, label, detail) in enumerate(tiles):
    x = 0.06 + i * 0.225
    fig.add_artist(plt.Rectangle((x - 0.01, 0.16), 0.205, 0.58, transform=fig.transFigure,
                                 facecolor="#f3f2ef", edgecolor="none"))
    fig.text(x, 0.58, big, fontsize=44, fontweight="bold", color=INK)
    fig.text(x, 0.47, label, fontsize=16, color=INK, va="top")
    fig.text(x, 0.34, detail, fontsize=11.5, color=INK_2, va="top", linespacing=1.5)
footer(fig, "Data and methods: github.com/ElvisOkereke/ClipLite/tree/main/docs/benchmarks/raw")
fig.savefig(HERE / "1-results-card.png")
plt.close(fig)

# ── 2. save latency before / after ───────────────────────────────────────────
fig, ax = plt.subplots(figsize=SIZE, dpi=DPI)
fig.subplots_adjust(left=0.2, right=0.9, top=0.78, bottom=0.18)
labels = ["Before\n(original build)", "After\n(Phase 1 WS0)"]
vals = [before_latency, after_latency]
bars = ax.barh(labels, vals, color=[BEFORE, AFTER], height=0.5)
ax.invert_yaxis()
for b, v in zip(bars, vals):
    ax.text(v + 0.3, b.get_y() + b.get_height() / 2, f"{v:.2f} s", va="center", fontsize=20, fontweight="bold", color=INK)
ax.set_xlim(0, 25)
ax.set_xlabel("seconds from hotkey press to finished clip")
ax.xaxis.grid(True, color=GRID, linewidth=1)
ax.set_axisbelow(True)
ax.tick_params(axis="y", length=0, labelsize=17)
frame(ax, f"Clip saves got {before_latency / after_latency:.0f}× faster",
      "Same 10-minute replay buffer. Fix: the recorder now drains FFmpeg's stderr, so FFmpeg never blocks.")
footer(fig, "Before: median of 11/11 saves in a 2 h soak (range 21.21–21.35 s). After: worst of 2 saves (0.29 s, 0.71 s). RTX 3070, FFmpeg 8.1.1.")
fig.savefig(HERE / "2-save-latency.png")
plt.close(fig)

# ── 3. hotkey reach ──────────────────────────────────────────────────────────
fig, ax = plt.subplots(figsize=SIZE, dpi=DPI)
fig.subplots_adjust(left=0.24, right=0.9, top=0.78, bottom=0.18)
labels = ["Game runs normally", "Game runs as administrator"]
pct = [n_ok / n_all * 100, e_reach / e_all * 100]
bars = ax.barh(labels, pct, color=BEFORE, height=0.5)
ax.invert_yaxis()
for b, v, t in zip(bars, pct, [f"{n_ok}/{n_all}", f"{e_reach}/{e_all}"]):
    ax.text(v + 1.5, b.get_y() + b.get_height() / 2, t, va="center", fontsize=20, fontweight="bold", color=INK)
ax.set_xlim(0, 112)
ax.set_xticks([0, 25, 50, 75, 100])
ax.set_xlabel("% of hotkey presses that reached ClipLite (12 display/load conditions × 5 trials)")
ax.xaxis.grid(True, color=GRID, linewidth=1)
ax.set_axisbelow(True)
ax.tick_params(axis="y", length=0, labelsize=17)
frame(ax, "Why hotkeys “don't work in fullscreen”",
      "Windows UIPI blocks a normal-integrity keyboard hook from input meant for an elevated game, in every display mode.")
footer(fig, "Original build. Windowed, borderless and exclusive fullscreen; ClipLite visible or hidden; CPU idle or saturated. Fix lands in WS1.")
fig.savefig(HERE / "3-hotkey-reach.png")
plt.close(fig)

# ── 4. soak memory ───────────────────────────────────────────────────────────
fig, ax = plt.subplots(figsize=SIZE, dpi=DPI)
fig.subplots_adjust(left=0.1, right=0.95, top=0.78, bottom=0.18)
minutes = [t / 60 for t in soak_t]
ax.axvspan(0, 10, color="#f3f2ef", linewidth=0)
ax.text(5, 30, "warm-up", ha="center", fontsize=13, color=INK_2)
ax.plot(minutes, soak_mb, color=BEFORE, linewidth=2)
ax.set_ylim(0, max(soak_mb) * 1.25)
ax.set_xlim(0, 120)
ax.set_xlabel("minutes")
ax.set_ylabel("ClipLite private memory (MB, all processes)")
ax.yaxis.grid(True, color=GRID, linewidth=1)
ax.set_axisbelow(True)
dip = min((i for i in range(len(soak_mb)) if minutes[i] > 10), key=lambda i: soak_mb[i])
ax.annotate("one sample taken during a save,\nwhile FFmpeg was being relaunched", xy=(minutes[dip], soak_mb[dip]),
            xytext=(minutes[dip] + 6, soak_mb[dip] + 140), fontsize=12.5, color=INK_2,
            arrowprops=dict(arrowstyle="-", color=INK_2, linewidth=1))
ax.text(minutes[-1], soak_mb[-1] + 25, f"{soak_mb[-1]:.0f} MB", ha="right", fontsize=16, fontweight="bold", color=INK)
frame(ax, "No memory leak: flat across a 2-hour soak",
      f"Replay buffer + fake game, a save every 10 min. Slope after warm-up: {soak['slope_mb_per_hour']:.1f} MB/h, band {soak['band_mb']:.0f} MB.")
footer(fig, "Original build, sampled every 5 s across UI, recorder and FFmpeg processes. The real problem was elsewhere: save latency (chart 2).")
fig.savefig(HERE / "4-soak-memory.png")
plt.close(fig)

print("wrote", sorted(p.name for p in HERE.glob("*.png")))
print(f"ratio={before_latency / after_latency:.1f}x avg_cost={(1 - fps['replay'] / fps['baseline']) * 100:.2f}% "
      f"low_cost={(1 - low['replay'] / low['baseline']) * 100:.2f}%")
