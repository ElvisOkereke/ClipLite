/** Pure helpers for the shortcut UI. Chord parsing lives in Rust; the UI only
 *  formats strings Rust already produced. */

/** Actions the Rust dispatcher supports (hotkey_actions::Action), in UI order. */
export const HOTKEY_ACTIONS: { key: string; label: string }[] = [
  { key: "start",         label: "Start recording" },
  { key: "stop",          label: "Stop recording" },
  { key: "pause",         label: "Pause / resume" },
  { key: "replay_toggle", label: "Replay buffer on/off" },
  { key: "replay_save",   label: "Save replay" },
  { key: "library",       label: "Open library" },
];

export const actionLabel = (key: string): string =>
  HOTKEY_ACTIONS.find(a => a.key === key)?.label ?? key;

/** "Ctrl + Shift + F2" -> ["Ctrl", "Shift", "F2"] (one entry per key cap). */
export function splitDisplay(display: string): string[] {
  return display.trim() === "" ? [] : display.split(" + ");
}

/** For every action whose (non-empty) chord is shared with another action,
 *  the action it clashes with. Pass canonical strings so legacy spellings of
 *  the same chord compare equal. */
export function findConflicts(hotkeys: Record<string, string>): Record<string, string> {
  const byChord = new Map<string, string[]>();
  for (const [action, chord] of Object.entries(hotkeys)) {
    const k = chord.trim().toLowerCase();
    if (!k) continue;
    byChord.set(k, [...(byChord.get(k) ?? []), action]);
  }
  const out: Record<string, string> = {};
  for (const actions of byChord.values()) {
    if (actions.length < 2) continue;
    for (const a of actions) out[a] = actions.find(o => o !== a)!;
  }
  return out;
}

/** Rust reports a lone Esc as a finished chord; the UI treats it as cancel. */
export const isCancelChord = (canonical: string): boolean =>
  canonical === "Escape" || canonical === "Esc";

export interface HotkeyDescription { canonical: string; display: string; valid: boolean }

export interface HotkeyCaptureEvent {
  state: "holding" | "done" | "cancelled";
  canonical?: string;
  display?: string;
  reason?: "timeout" | "reinstall" | "user";
}

export interface HotkeyFiredEvent {
  action: string;
  cid: string;
  display?: string;
  error?: string;
}

/** Toast text for a `hotkey-fired` event. */
export function firedMessage(e: HotkeyFiredEvent): { text: string; error: boolean } {
  if (e.error) return { text: e.error, error: true };
  const ok: Record<string, string> = {
    start: "Recording started",
    stop: "Recording stopped",
    pause: "Pause toggled",
    replay_toggle: "Replay buffer toggled",
    replay_save: "Replay saved",
    library: "Library opened",
  };
  return { text: ok[e.action] ?? `${actionLabel(e.action)} done`, error: false };
}

/** One entry of `get_debug_state().hotkey_status.bindings`. */
export interface BindingStatus {
  action: string;
  canonical: string | null;
  parsed: boolean;
  registered: boolean | null;
  register_error?: string | null;
}
export interface HotkeyStatus { bindings: BindingStatus[]; capturing: boolean }

/** Banner text for an `elevated-foreground` event. Deliberately hedged:
 *  whether shortcuts survive an elevated foreground app is not guaranteed. */
export function elevatedNoticeText(exe: string): string {
  const name = exe.trim() || "An elevated app";
  return `${name} is running as administrator. Windows may block ClipLite's shortcuts while it's focused \u2014 if they don't work, restart ClipLite as administrator. Keyboard-only combos (e.g. Ctrl+F2) have the best chance; mouse buttons and modifier-only shortcuts are the most likely to be blocked.`;
}
