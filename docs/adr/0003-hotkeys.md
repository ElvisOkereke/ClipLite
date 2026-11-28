# ADR 0003: Global hotkeys run in Rust (hook + RegisterHotKey union)

- **Status:** Accepted. Code built on branch `phase1/ws1`; hardware verification pending (see "Not yet proven").
- **Date:** 2026-11-27
- **Numbering:** 0001 and 0002 are reserved for pending WS2 ADRs and don't exist yet. This ADR is numbered 0003 on purpose.

## Context

Users reported "hotkeys don't work in fullscreen." The original design was a low-level keyboard hook (`WH_KEYBOARD_LL`) that forwarded matches to the WebView, which executed the action. Chords were typed into a UI text field.

### Evidence

1. **Elevated games: 1/60.** In the baseline hotkey matrix with the game running as administrator, ClipLite (medium integrity) saw one press in 60, in every display mode, windowed included. A normal game: 60/60. This is UIPI blocking a lower-integrity hook from seeing input meant for an elevated window (baseline report, finding 4).
2. **Own-window focus.** The hook was never invoked while ClipLite's own window had focus; debug builds hid this because DevTools took focus (baseline report, finding 5).
3. **The owner's 2026-11-27 session.** Hooks installed, Ctrl+` bound to `replay_save`, about 2 minutes of attempts, zero `hotkey.pressed` events in the owner's app log. The keyboard layout was ruled out: Backquote maps to VK 0xC0 on both installed layouts (0x04091009 and 0x04090409). So the failure was in delivery, not in key mapping.
4. **Design problems independent of delivery:**
   - Exact modifier equality: holding Shift or W blocked a chord.
   - Actions executed in the WebView, so a hidden or busy WebView dropped them.
   - The UI text field could not capture mouse buttons, F13+, modifier-only chords or the Win key (which it mapped to Ctrl).
   - No conflict detection.

## Decision

Move the whole path into Rust:

- **One input thread** owns the low-level keyboard and mouse hooks **and** `RegisterHotKey` registrations for every bound chord that can use them. The two sources are a union: either one may trigger an action.
  - `RegisterHotKey` (keyboard-only chords) is delivered by the system rather than through the hook chain, so it is the path with the best chance in front of elevated or hook-hostile foreground apps, and while ClipLite's own window is focused.
  - The hook handles what `RegisterHotKey` cannot: mouse buttons, modifier-only chords, and superset matches (extra held keys don't block a chord).
- **Dedup** across the two sources, so a chord seen by both runs the action once.
- **Dispatcher** matches chords and hands actions to a **serial action worker** that runs them in press order and emits `hotkey-fired` for the UI. Actions no longer depend on the WebView.
- **Watchdog reinstall:** on the input thread, (hooks are unhooked and reinstalled every 60 s and after resume from sleep or session unlock, since Windows can silently drop or stale a hook).
- **Capture through the hook:** "record a shortcut" in Settings is a Rust capture mode driven by the same hook, so any chord the matcher can fire can be recorded, with conflict warnings in the UI.
- **Elevated-foreground notice:** a separate lightweight poller thread (every 2 s) checks whether the foreground process is elevated while ClipLite is not, and emits `elevated-foreground {exe}` once per exe per session. The UI shows a dismissible banner. It is separate from the dispatcher so the dispatcher's loop stays simple (it uses a timed receive only while capturing). The wording is deliberately hedged, because shortcut survival in front of an elevated app isn't proven yet.

## Consequences and trade-offs

- **Bound combos are exclusive to ClipLite while it runs.** `RegisterHotKey` takes the chord system-wide; other apps (and games reading that chord) stop seeing it. A chord already registered by another app can't be registered and falls back to hook-only, which is surfaced in hotkey status.
- **Not fixable here:** anti-cheat that blocks or strips hooks. `RegisterHotKey` may still cover keyboard-only chords in that case, but mouse and modifier-only chords will not work. No opt-in elevated mode is built; the banner tells the user to restart ClipLite as administrator if shortcuts fail.
- More moving parts (input thread, dispatcher, worker, poller) in exchange for deterministic, testable behavior: the matcher and dispatcher state machines are pure and unit-tested.

## Not yet proven

- **HIL matrix 20/20** (12-cell display-mode/window-state/load grid) with the new listener.
- **Elevated matrix** (game as administrator): the baseline was 1/60; whether `RegisterHotKey` delivers across the integrity boundary on target machines has not been measured.
- **Owner real-game check**: the 2026-11-27 no-events failure has not been re-tested with the new build.

Until those runs pass, nothing here should be described as "fixed."
