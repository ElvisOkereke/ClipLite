# ADR 0003: Global hotkeys run in Rust (hook + RegisterHotKey union)

- **Status:** Accepted. Code built on branch `phase1/ws1`; hardware verification pending (see "Not yet proven").
- **Date:** 2026-11-27
- **Numbering:** 0001 and 0002 are reserved for pending WS2 ADRs and don't exist yet. This ADR is numbered 0003 on purpose.

## Context

Users reported "hotkeys don't work in fullscreen." The original design was a low-level keyboard hook (`WH_KEYBOARD_LL`) that forwarded matches to the WebView, which executed the action. Chords were typed into a UI text field.

### Evidence

1. **Elevated games: 1/60.** In the baseline hotkey matrix with the game running as administrator, ClipLite (medium integrity) saw one press in 60, in every display mode, windowed included. A normal game: 60/60. This is UIPI blocking a lower-integrity hook from seeing input meant for an elevated window (baseline report, finding 4).
2. **Own-window focus.** The hook was never invoked while ClipLite's own window had focus; debug builds hid this because DevTools took focus (baseline report, finding 5).
3. **The owner's manual test.** Hooks installed, Ctrl+` bound to `replay_save`, a couple of minutes of attempts, zero `hotkey.pressed` events in the owner's app log from that test. The keyboard layout was ruled out: both installed keyboard layouts map Backquote to VK 0xC0. So the failure was in delivery, not in key mapping.
4. **Design problems independent of delivery:**
   - Exact modifier equality: holding Shift or W blocked a chord.
   - Actions executed in the WebView, so a hidden or busy WebView dropped them.
   - The UI text field could not capture mouse buttons, F13+, modifier-only chords or the Win key (which it mapped to Ctrl).
   - No conflict detection.

## Decision

Move the whole path into Rust:

- **One input thread** owns the low-level keyboard and mouse hooks **and** the `RegisterHotKey` registrations. The two sources are a union: either one may trigger an action.
  - **The hook is always the primary path.** It handles everything, including what `RegisterHotKey` cannot: mouse buttons, modifier-only chords, and superset matches (extra held keys don't block a chord).
  - **`RegisterHotKey` is held only while needed**, because a registration takes the chord away from every other app system-wide (the owner's Ctrl+1, Ctrl+F4 and Ctrl+` would stop working in games, Discord, browsers and editors). A foreground watcher (the elevation poller, now every 500 ms) decides `should_register`: true while ClipLite's own window is focused (the foreground window's process is ClipLite's) or while a *definitely* elevated app is focused (UIPI blocks the hook there); Unknown elevation is false. On a change it posts `WM_APP_REGISTER` to the input thread, which registers or unregisters every registerable binding on that thread (`RegisterHotKey` binds to the calling thread's window). Registrations are also released during a shortcut capture (`WM_APP_CAPTURING` from the dispatcher), so a chord being recorded is not swallowed.
  - Only chords with Ctrl, Alt or Win are ever registered (`registerable`); bare keys and Shift-only chords stay hook-only even when registration is active. A chord bound to two actions is registered once, and a failure shown as "another app uses this" only comes from a real `RegisterHotKey` failure, never from an internal duplicate.
  - `hotkey_status` reports `register_active` and, per binding, `registerable`, `registered` (right now) and the last `register_error`.
- **Dedup** across the two sources, so a chord seen by both runs the action once.
- **Dispatcher** matches chords and hands actions to a **serial action worker** that runs them in press order and emits `hotkey-fired` for the UI. Actions no longer depend on the WebView.
- **Watchdog reinstall:** on the input thread, hooks are unhooked and reinstalled every 60 s and after resume from sleep or session unlock, since Windows can silently drop or stale a hook.
- **Capture through the hook:** "record a shortcut" in Settings is a Rust capture mode driven by the same hook, so any chord the matcher can fire can be recorded, with conflict warnings in the UI.
  - **Compatibility-mode fallback:** if a WebView keydown arrives during a capture and the hook sends no `holding` event within 300 ms (baseline finding 5: the hook was not invoked while ClipLite's window was focused, probably because of the old in-callback IPC, unproven), that capture is recorded from WebView key/mouse events (`KeyboardEvent.code`, mouse buttons 1/3/4 as Mouse3/4/5), converted by the Rust command `chord_from_web` (table: `Input::from_web_code`), and the Rust capture is then cancelled. The UI says "recorded in compatibility mode" and Rust logs a `capture fallback` warning so the HIL kit can tell the fallback was used. Chords the WebView never sees (some Win-key combos) can't be recorded this way.
- **Defaults** are Ctrl+Alt+F7 to F12 (library, replay toggle, start, stop, pause, replay save), which games, browsers, Discord and editors rarely use. Saved bindings are never migrated.
- **Elevated-foreground notice:** a separate lightweight poller thread (every 2 s) checks whether the foreground process is elevated while ClipLite is not, and emits `elevated-foreground {exe}` once per exe per session. The UI shows a dismissible banner. It is separate from the dispatcher so the dispatcher's loop stays simple (it uses a timed receive only while capturing). Detection is tri-state (Elevated / NotElevated / Unknown) and only a definite Elevated verdict can raise the banner: if the foreground process can't be opened for limited query, or the token can't be read for any reason other than access denied, the result is Unknown and nothing is shown (so protected, SYSTEM, other-user and exited processes don't produce false banners or use up the one-shot). Access denied opening the token of a process we could open counts as Elevated. If ClipLite can't read its own token, notices are disabled. The wording is deliberately hedged, because shortcut survival in front of an elevated app isn't proven yet.

## Consequences and trade-offs

- **Bound combos are exclusive only while an elevated app or ClipLite itself is focused.** During that time `RegisterHotKey` takes the chord system-wide and other apps stop seeing it; the rest of the time other apps see every bound chord (the hook observes without consuming). A chord already registered by another app can't be registered and stays hook-only, which is surfaced in hotkey status. Registration lags a focus change by up to 500 ms, so the first press right after alt-tabbing into an elevated game may rely on the hook alone.
- **Not fixable here:** anti-cheat that blocks or strips hooks. `RegisterHotKey` may still cover keyboard-only chords in that case, but mouse and modifier-only chords will not work. No opt-in elevated mode is built; the banner tells the user to restart ClipLite as administrator if shortcuts fail.
- More moving parts (input thread, dispatcher, worker, poller) in exchange for deterministic, testable behavior: the matcher and dispatcher state machines are pure and unit-tested.

## Not yet proven

- **HIL matrix 20/20** (12-cell display-mode/window-state/load grid) with the new listener.
- **Elevated matrix** (game as administrator): the baseline was 1/60; whether `RegisterHotKey` (now registered only while the elevated game is focused) delivers across the integrity boundary on target machines has not been measured.
- **Compatibility-mode capture** has not been exercised on hardware; whether the hook now fires while ClipLite is focused (making the fallback unnecessary) is also unmeasured.
- **Owner real-game check**: the no-events failure from the owner's manual test has not been re-tested with the new build.

Until those runs pass, nothing here should be described as "fixed."
