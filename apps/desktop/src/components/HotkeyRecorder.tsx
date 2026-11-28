import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  beginHotkeyCapture, cancelHotkeyCapture, defaultHotkeys, describeHotkeys, getHotkeyStatus,
} from "../api";
import {
  HOTKEY_ACTIONS, actionLabel, findConflicts, isCancelChord, splitDisplay,
  type HotkeyCaptureEvent, type HotkeyDescription, type HotkeyStatus,
} from "../hotkeys";

interface Props {
  hotkeys: Record<string, string>;
  onChange: (action: string, canonical: string) => void;
}

function KeyCaps({ display }: { display: string }) {
  return (
    <span className="keycaps">
      {splitDisplay(display).map((k, i) => <kbd className="keycap" key={i}>{k}</kbd>)}
    </span>
  );
}

/** Shortcut list. Recording happens in Rust (low-level hook), so any key,
 *  mouse button or modifier-only chord can be bound, and nothing typed here
 *  reaches the page or triggers an action. */
export function HotkeyRecorder({ hotkeys, onChange }: Props) {
  const [desc, setDesc] = useState<Record<string, HotkeyDescription>>({});
  const [defaults, setDefaults] = useState<Record<string, string>>({});
  const [status, setStatus] = useState<HotkeyStatus | null>(null);
  const [capturing, setCapturing] = useState<string | null>(null);
  const [liveChord, setLiveChord] = useState("");
  const [note, setNote] = useState("");

  const capturingRef = useRef<string | null>(null);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  const setCap = (a: string | null) => { capturingRef.current = a; setCapturing(a); };

  const stopCapture = useCallback((tellRust = true) => {
    if (capturingRef.current === null) return;
    setCap(null);
    setLiveChord("");
    if (tellRust) cancelHotkeyCapture().catch(() => {});
  }, []);

  // Display strings come from Rust (single source of truth for chord grammar).
  const hotkeysKey = JSON.stringify(hotkeys);
  useEffect(() => {
    let current = true;
    describeHotkeys(hotkeys).then(d => { if (current) setDesc(d); }).catch(() => {});
    return () => { current = false; };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hotkeysKey]);

  useEffect(() => {
    defaultHotkeys().then(setDefaults).catch(() => {});
    getHotkeyStatus().then(setStatus).catch(() => {});
  }, []);

  // Capture progress from Rust.
  useEffect(() => {
    const p = listen<HotkeyCaptureEvent>("hotkey-capture", e => {
      const ev = e.payload;
      const action = capturingRef.current;
      if (action === null) return;
      if (ev.state === "holding") {
        setLiveChord(ev.display ?? "");
      } else if (ev.state === "done") {
        setCap(null); setLiveChord("");
        if (ev.canonical && !isCancelChord(ev.canonical)) {
          setNote("");
          onChangeRef.current(action, ev.canonical);
        }
      } else if (ev.reason === "reinstall") {
        // The hook watchdog restarted the hooks; carry on transparently.
        beginHotkeyCapture().catch(() => { setCap(null); setLiveChord(""); });
      } else {
        setCap(null); setLiveChord("");
        if (ev.reason === "timeout") setNote("Timed out — try again");
      }
    });
    return () => { p.then(f => f()); };
  }, []);

  // Cancel on unmount.
  useEffect(() => () => {
    if (capturingRef.current !== null) cancelHotkeyCapture().catch(() => {});
  }, []);

  // While capturing: cancel on blur / click elsewhere, keep input out of the page.
  useEffect(() => {
    if (capturing === null) return;
    const onBlur = () => stopCapture();
    const onDown = (e: MouseEvent) => {
      if (e.button !== 0) { e.preventDefault(); return; } // side/middle buttons belong to the recorder
      if (!(e.target as HTMLElement | null)?.closest?.("[data-capture-btn]")) stopCapture();
    };
    const swallowKey = (e: KeyboardEvent) => { e.preventDefault(); e.stopPropagation(); };
    const swallowAux = (e: MouseEvent) => { if (e.button !== 0) e.preventDefault(); };
    window.addEventListener("blur", onBlur);
    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("mouseup", swallowAux, true);
    document.addEventListener("auxclick", swallowAux, true);
    window.addEventListener("keydown", swallowKey, true);
    window.addEventListener("keyup", swallowKey, true);
    return () => {
      window.removeEventListener("blur", onBlur);
      document.removeEventListener("mousedown", onDown, true);
      document.removeEventListener("mouseup", swallowAux, true);
      document.removeEventListener("auxclick", swallowAux, true);
      window.removeEventListener("keydown", swallowKey, true);
      window.removeEventListener("keyup", swallowKey, true);
    };
  }, [capturing, stopCapture]);

  const change = async (action: string) => {
    if (capturingRef.current === action) { stopCapture(); return; }
    if (capturingRef.current !== null) await cancelHotkeyCapture().catch(() => {});
    setNote(""); setLiveChord("");
    try {
      await beginHotkeyCapture();
      setCap(action);
    } catch (e) {
      setCap(null);
      setNote(`Could not start recording: ${e}`);
    }
  };

  const canonicalOf = (a: string) => {
    const d = desc[a];
    return d?.valid ? d.canonical : (hotkeys[a] ?? "");
  };
  const conflicts = findConflicts(
    Object.fromEntries(HOTKEY_ACTIONS.map(a => [a.key, canonicalOf(a.key)])),
  );
  // Another app owns this combo for RegisterHotKey (status reflects the applied bindings).
  const sharedWithOtherApp = (action: string) => {
    const c = canonicalOf(action);
    const b = status?.bindings.find(x => x.action === action);
    return !!c && !!b && b.canonical === c && b.registered === false;
  };

  return (
    <div>
      {HOTKEY_ACTIONS.map(({ key, label }) => {
        const d = desc[key];
        const raw = hotkeys[key] ?? "";
        const isCap = capturing === key;
        const other = conflicts[key];
        return (
          <div className="hotkey-row" key={key}>
            <div className="hotkey-main">
              <span className="hotkey-label">{label}</span>
              <span className="hotkey-binding">
                {isCap ? (
                  <>
                    {liveChord ? <KeyCaps display={liveChord} /> : null}
                    <span className="hotkey-hint">Press your shortcut… (Esc to cancel)</span>
                  </>
                ) : d?.valid ? (
                  <KeyCaps display={d.display} />
                ) : raw ? (
                  <span className="hotkey-warn">Not a valid shortcut ({raw})</span>
                ) : (
                  <span className="hotkey-hint">Not set</span>
                )}
              </span>
              <button className="btn btn-secondary btn-sm" data-capture-btn onClick={() => change(key)}>
                {isCap ? "Cancel" : "Change"}
              </button>
              <button className="btn btn-secondary btn-sm" disabled={!raw}
                onClick={() => { stopCapture(); onChange(key, ""); }}>Clear</button>
              <button className="btn btn-secondary btn-sm"
                disabled={!defaults[key] || raw === defaults[key]}
                title="Restore the default shortcut"
                onClick={() => { stopCapture(); onChange(key, defaults[key]); }}>Reset</button>
            </div>
            {other && <div className="hotkey-warn">Also used by {actionLabel(other)}</div>}
            {!isCap && sharedWithOtherApp(key) && (
              <div className="hotkey-warn">
                Another app also uses this shortcut. It may still work in games.
              </div>
            )}
          </div>
        );
      })}
      {note && <p className="hotkey-warn">{note}</p>}
      <p style={{ fontSize: 10, color: "var(--text-muted)", marginTop: 4 }}>
        Click Change, then press any keys or mouse buttons (a single modifier works too).
        Shortcuts apply after Save Settings.
      </p>
    </div>
  );
}
