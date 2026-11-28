# Phase 1 WS1 — Hotkeys that always fire, any combination

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Hotkeys fire reliably in games, with ClipLite focused or hidden, for any key/mouse combination the owner can press, and the actions run in Rust instead of depending on the WebView.

**Architecture:** A pure, unit-tested chord model and matcher lives in `cliplite-core::hotkey`. The desktop app runs one input thread. It installs low-level keyboard and mouse hooks whose callbacks only forward raw events over a channel. It also registers `RegisterHotKey` duplicates for chords that API can express (these still fire when an elevated window or ClipLite itself is focused). A dispatcher thread matches events, de-duplicates the two paths, and runs the action through the same Rust code the UI commands use, then notifies the UI. Settings capture goes through the same hook ("press it and we record exactly what the hook sees"), so what you bind is what fires.

**Tech Stack:** Rust (windows 0.58 crate, Win32 hooks and RegisterHotKey), Tauri v2, React 18/TypeScript, vitest.

**Spec:** `docs/superpowers/specs/2026-11-11-phase1-foundation-design.md` (§3 goal 1, §7 WS1). Evidence: `docs/benchmarks/raw/baseline/README.md` findings 4 and 5, plus the owner's 2026-11-27 session log (`~/.cliplite/logs/app.2026-11-28.jsonl`). That log has hooks installed but zero `hotkey.pressed` events in about 2 min of attempts with Ctrl+` bound.

## Global Constraints

- Windows 10/11 only. Must not require admin rights for normal use.
- No screen-takeover or input-injection tests in this plan's execution. The owner is using the PC. Verification is `cargo test --workspace`, `cargo build --workspace` with `-D warnings`, `npm run typecheck` and `npm test`. The HIL matrix runs later, when the owner is away.
- Never inject input into, or automate, real games (anti-cheat ban risk).
- Hook callbacks: no logging, no locks that can block, no IPC, no allocation-heavy work. They forward the event and return.
- Keep two-process isolation: no FFmpeg/WASAPI in the Tauri process.
- Existing saved hotkey strings must keep working (`CommandOrControl+Shift+F`, `Ctrl+Digit1`, `CommandOrControl+Backquote`, `Alt+F9`, ...).
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Holding other keys while gaming** (Shift to sprint, W to move) must not stop Ctrl+F2 from firing. Extra held keys are tolerated, and the most specific binding wins.
2. **Key repeat:** holding a chord fires once, until all of its keys are released.
3. **Both paths fire for one press** (hook + RegisterHotKey). The action must run exactly once.
4. **A modifier-only chord** (e.g. Ctrl+Alt) fires when the last modifier is pressed, but not when that modifier is part of a longer chord already in progress. Ruling: modifier-only chords fire on **release**, and only if no other key was pressed meanwhile.
5. **Rebinding while the listener runs** must not drop or double-install hooks, and must not leave stale `RegisterHotKey` registrations behind.

---

### Task 1: Chord model and matcher (`cliplite-core::hotkey`)

**Files:**
- Create: `crates/cliplite-core/src/hotkey.rs`, test module `crates/cliplite-core/src/hotkey_tests.rs`
- Modify: `crates/cliplite-core/src/lib.rs` (add `pub mod hotkey;`)

**Interfaces (Produces):**
- `pub enum Input { Key(u32 /*VK*/), Mouse(MouseButton) }` and `pub enum MouseButton { Middle, X1, X2 }`. Left/right buttons cannot be bound.
- Modifier normalisation: `VK_LCONTROL/RCONTROL→VK_CONTROL (0x11)`, `L/RSHIFT→0x10`, `L/RMENU→0x12`, `LWIN/RWIN→VK_LWIN (0x5B)`. `pub fn normalize_vk(vk: u32) -> u32`.
- `pub struct Chord { keys: BTreeSet<Input> }` with `Chord::parse(&str) -> Result<Chord, String>`, `fn to_canonical(&self) -> String`, `fn display(&self) -> String`, `fn is_modifier_only(&self) -> bool`, `fn registerhotkey_form(&self) -> Option<(u32 /*MOD_* flags*/, u32 /*vk*/)>` (Some only for zero or more modifiers plus exactly one non-modifier keyboard key).
- Canonical string: modifiers in the fixed order `Ctrl+Shift+Alt+Win`, then other inputs in VK order. Names: letters `A`–`Z`, digits `0`–`9`, `F1`–`F24`, `Numpad0`–`Numpad9`, `NumpadAdd`, etc. Keep every name the current parser accepts. Add `PrintScreen`, `Pause`, `ScrollLock`, `CapsLock`, `Menu`, `BrowserBack` and similar, plus the media keys, `Mouse3` (middle), `Mouse4` (X1), `Mouse5` (X2). Any other VK is `VK_0xNN`.
- `parse` accepts the legacy aliases too: `CommandOrControl`, `CmdOrCtrl`, `Control`, `Key*`, `Digit*`, `Backquote`/`Backtick`. **`Meta`/`Super`/`Cmd` now map to Win, not Ctrl** (ruling: the old Ctrl mapping was a bug, and the UI never produced Meta on Windows).
- `display`: human labels joined with ` + `, e.g. `Ctrl + Shift + F2`, `` Ctrl + ` ``, `Mouse 4`.
- `pub enum RawEvent { Down(Input), Up(Input) }`.
- `pub struct Matcher` with `Matcher::new(bindings: Vec<(String /*action*/, Chord)>)` and `fn on_event(&mut self, ev: RawEvent) -> Option<String /*action*/>`. Rules:
  - Track the set of held inputs from its own events. Ignore a `Down` for an input already held (that is key repeat).
  - Non-modifier-only chords fire on the `Down` that makes every chord input held, where that down-input belongs to the chord. Extra held inputs are allowed. If several bindings complete on the same event, pick the one with the most inputs. A chord fires at most once until one of its inputs is released.
  - Modifier-only chords fire on the `Up` of one of their inputs if, since the chord became fully held, no non-chord input went down.
- `pub fn find_conflicts(bindings: &[(String, Chord)]) -> Vec<(String, String)>`: pairs of actions with identical chords.
- `pub struct Capture` for settings recording: `fn on_event(&mut self, ev: RawEvent) -> CaptureState`, where `CaptureState::{Idle, Holding(Chord), Done(Chord)}`. `Done` is the union of everything held, emitted when every input has been released. A lone left/right mouse button is ignored.

- [ ] **Step 1: Write failing tests** in `hotkey_tests.rs`. At minimum:
  - `legacy_strings_parse`: every string in the current `hotkey_listener_tests.rs` and in `settings.rs` defaults, plus `CommandOrControl+Backquote`. Round-trip `parse → to_canonical → parse` is identity.
  - `function_keys_to_f24_and_mouse_buttons_parse`: `Ctrl+F13`, `F24`, `Mouse4`, `Shift+Mouse5`.
  - `meta_maps_to_win`.
  - `display_is_human`: `` Ctrl + ` ``, `Mouse 4`.
  - `fires_with_extra_held_keys`: hold Shift and W, then Ctrl down and F2 down → `Ctrl+F2` fires.
  - `most_specific_wins`: `Ctrl+F2` and `Ctrl+Shift+F2` both bound; with Ctrl+Shift held, F2 → only `Ctrl+Shift+F2`.
  - `repeat_fires_once`: Ctrl down, F2 down ×3 → one fire. Release F2, press again → second fire.
  - `modifier_only_fires_on_clean_release`: Ctrl, Alt down, Alt up → fires. Ctrl, Alt, K down, ups → no fire for `Ctrl+Alt`.
  - `conflicts_detected`.
  - `capture_records_union_on_full_release`: Ctrl down, Mouse4 down, Mouse4 up → `Holding`. Ctrl up → `Done(Ctrl+Mouse4)`.
  - `registerhotkey_form`: `Ctrl+Shift+F2` → `(MOD_CONTROL|MOD_SHIFT, 0x71)`. `Mouse4` and `Ctrl+Alt` → None.
- [ ] **Step 2:** `cargo test -p cliplite-core hotkey`. Expected: FAIL (module missing).
- [ ] **Step 3:** Implement `hotkey.rs`. Pure Rust, no `windows` crate.
- [ ] **Step 4:** `cargo test -p cliplite-core`. Expected: all pass.
- [ ] **Step 5:** Commit `feat(core): chord model and matcher for any-combination hotkeys`.

### Task 2: Input thread — hooks forward, dispatcher matches, actions run in Rust

**Files:**
- Rewrite: `apps/desktop/src-tauri/src/hotkey_listener.rs` (delete `parse_hotkey_to_vk`; update `hotkey_listener_tests.rs` to cover the remaining desktop-only logic, or delete it if empty)
- Modify: `apps/desktop/src-tauri/src/lib.rs` (start and re-register), `apps/desktop/src-tauri/src/commands.rs` (an action executor callable without the WebView, plus capture commands), `apps/desktop/src-tauri/Cargo.toml` (windows features if needed)
- Create: `apps/desktop/src-tauri/src/hotkey_actions.rs` (action → Rust command)

**Interfaces:**
- Consumes: Task 1 `Chord`, `Matcher`, `Capture`, `RawEvent`, `Input`, `normalize_vk`.
- Produces:
  - `HotkeyListener::start(app, bindings: HashMap<String,String>) -> HotkeyListener` and `fn stop_and_wait(&self)`. `stop_and_wait` must really join the thread, not sleep 300 ms.
  - Tauri commands `begin_hotkey_capture()` and `cancel_hotkey_capture()`. While capturing, the matcher is paused. The listener emits `hotkey-capture` with `{ state: "holding"|"done", canonical, display }`.
  - The UI event `hotkey-fired` with `{ action, cid, display }`, emitted **after** the action ran. On failure it emits `{ action, cid, error }`.
  - `hotkey_status` (extend `get_debug_state`): per binding, whether it parsed, whether `RegisterHotKey` succeeded (and the error if a different app owns the combo), hook installed, and the time of the last hook reinstall.
- Behaviour:
  - One input thread. It installs `WH_KEYBOARD_LL` and `WH_MOUSE_LL` (XBUTTON1/2, middle down/up only, with mouse move ignored as early as possible). The callback ignores `LLKHF_INJECTED` events only when `CLIPLITE_TEST_MODE` is unset; the HIL kit injects. It sends `RawEvent` through `std::sync::mpsc::Sender` (or crossbeam), then `CallNextHookEx`. Nothing else.
  - The same thread creates a message-only window and calls `RegisterHotKey(hwnd, id, mods | MOD_NOREPEAT, vk)` for every binding with a `registerhotkey_form`. `WM_HOTKEY` goes to the dispatcher as `Fired{action, source: Register}`. Registration failures are logged and reported in `hotkey_status`, never fatal. Unregister everything on stop.
  - Watchdog: reinstall both hooks every 60 s, and immediately after `WM_POWERBROADCAST` resume / session unlock if easy. Hooks are unhooked and rehooked on the input thread itself, through a posted thread message, so the message pump stays the owner.
  - Dispatcher thread: runs `Matcher` (or `Capture` while capturing). If the same action fires from the other source within 250 ms, it runs once (de-duplication). It then calls `hotkey_actions::run(app, action, cid)` on the tauri async runtime, and logs `hotkey.pressed` / `hotkey.dispatched` with the cid. That logging lives here, never in the callback.
  - `hotkey_actions::run` maps:
    - `start` → `commands::start_recording` logic
    - `stop` → stop
    - `pause` → toggle, based on the recorder's status
    - `replay_toggle` → start or stop the replay buffer, based on state
    - `replay_save` → `save_replay` with the configured duration
    - `library` → show and focus the main window, then emit `navigate` `"library"`
    - Refactor the command bodies into plain `async fn`s taking `&AppHandle`, called both by the `#[tauri::command]` wrappers and by the dispatcher. No logic duplication.
  - `simulate_hotkey` (the debug command) goes through the dispatcher.
- [ ] **Step 1: Failing tests (pure parts only):** the dispatcher de-duplication as a pure struct (`Dedup::should_run(action, source, now) -> bool`), with tests `same_action_other_source_within_250ms_runs_once`, `same_source_twice_runs_twice` (two real presses) and `after_window_runs_again`. Also `hotkey_actions::Action::from_str` for every action name, unknown → Err.
- [ ] **Step 2:** Run `cargo test -p cliplite`. Expected: FAIL.
- [ ] **Step 3:** Implement. No screen tests and no `SendInput`.
- [ ] **Step 4:** `cargo build --workspace` (RUSTFLAGS `-D warnings`) and `cargo test --workspace`. Expected: green.
- [ ] **Step 5:** Commit `feat(desktop): hotkeys run in Rust via hook + RegisterHotKey dispatcher`.

### Task 3: Frontend — bind anything, see what's bound, actions no longer in the WebView

**Files:**
- Create: `apps/desktop/src/components/HotkeyRecorder.tsx`, `apps/desktop/src/hotkeys.ts` (pure helpers), `apps/desktop/src/hotkeys.test.ts`
- Modify: `apps/desktop/src/components/SettingsView.tsx` (replace the text-input capture and remove `keyEventToHotkeyString`), `apps/desktop/src/components/RecorderView.tsx` (stop running actions on `hotkey-*` events; refresh status and show a toast on `hotkey-fired`), `apps/desktop/src/App.tsx` (`navigate` event), `apps/desktop/src/components/DebugPanel.tsx`, `apps/desktop/src/api.ts`, `apps/desktop/src/types.ts`

**Behaviour:**
- Each action row shows the binding as key caps, e.g. `[Ctrl] [Shift] [F2]`, from the `display` string, plus buttons **Change**, **Clear** and **Reset**. Reset restores the default from `Settings::default`, exposed via a `default_hotkeys` command or a constant mirrored in `types.ts`.
- **Change** calls `begin_hotkey_capture`, shows "Press your shortcut… (Esc to cancel)" and renders the live `holding` chord. On `done` it stores the canonical string. Esc held alone cancels (Rust reports `Esc` alone as done → the UI treats a lone `Escape` as cancel). Clicking elsewhere cancels.
- Conflicts: if two actions share a chord, both rows show an inline warning ("Also used by Save replay"). Saving is still allowed, but the warning stays visible. If `hotkey_status` says another app owns the combo for `RegisterHotKey`, show "Another app also uses this shortcut. It may still work in games."
- Missing actions get rows too, e.g. `annotate` if it exists in settings. Only show actions `hotkey_actions` supports.
- Pure helpers in `hotkeys.ts`: `splitDisplay(display) -> string[]` for key caps and `findConflicts(hotkeys) -> Record<action, otherAction>`. Unit-tested with vitest.
- [ ] **Step 1:** Write failing `hotkeys.test.ts` (`splitDisplay("Ctrl + Shift + F2")` → `["Ctrl","Shift","F2"]`; `` splitDisplay("Ctrl + `") ``; `findConflicts` on duplicates and on none).
- [ ] **Step 2:** `npm test`. Expected: FAIL.
- [ ] **Step 3:** Implement the component, helpers and wiring.
- [ ] **Step 4:** `npm run typecheck && npm test && npm run build`. Expected: green.
- [ ] **Step 5:** Commit `feat(ui): record any shortcut through the hook, key-cap display, conflict warnings`.

### Task 4: Elevated-game notice and docs

**Files:** `apps/desktop/src-tauri/src/hotkey_listener.rs` or a new `elevation.rs`, `apps/desktop/src/components/RecorderView.tsx`, `docs/adr/0003-hotkeys.md`, `README.md` and `docs/ROADMAP.md` (built-vs-planned)

- The dispatcher thread checks every 2 s whether the foreground window's process is elevated while ClipLite is not (`OpenProcess` fails with access denied, or `GetTokenInformation(TokenElevation)`). On the first such transition per session, it emits `elevated-foreground {exe}`. The UI shows a dismissible banner: "<game> runs as administrator. Shortcuts that use extra keys or mouse buttons can't reach ClipLite while it's focused. Keyboard shortcuts (e.g. Ctrl+F2) still work." No admin elevation path in this task. Pure helper with tests: `ElevationNotice::observe(fg_elevated: bool, self_elevated: bool, exe) -> Option<String>` fires once per exe per session.
- The ADR records the diagnosis from the baseline findings and the owner's log, the decision (hook + RegisterHotKey union, Rust-side dispatch, capture through the hook) and what it can't fix (anti-cheat that blocks hooks, which `RegisterHotKey` may still cover). It also states what HIL runs remain to prove it: matrix 20/20 and the elevated matrix.
- [ ] Tests for `ElevationNotice`, then implement, `cargo test --workspace`, commit `feat: elevated-game notice; ADR 0003 hotkeys`.
