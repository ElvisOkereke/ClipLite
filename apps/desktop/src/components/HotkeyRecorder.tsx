import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  beginHotkeyCapture, cancelHotkeyCapture, chordFromWeb, defaultHotkeys, describeHotkeys, getHotkeyStatus,
} from "../api";
import {
  CAPTURE_FALLBACK_MS, HOTKEY_ACTIONS, WebChord, actionLabel, findConflicts, isCancelChord, splitDisplay,
  usedByOtherApp, webChordDisplay, webMouseCode,
  type HotkeyCaptureEvent, type HotkeyDescription, type HotkeyStatus,
} from "../hotkeys";

interface Props {
  hotkeys: Record<string, string>;
  /** Bump to re-read hotkey status (after settings were applied). */
  refreshKey?: number;
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
 *  reaches the page or triggers an action.
 *
 *  Compatibility mode: if a WebView keydown gets no `holding` event from the
 *  hook within CAPTURE_FALLBACK_MS (the hook may not be invoked while
 *  ClipLite's own window is focused), that capture is recorded from WebView
 *  key/mouse events instead and converted by Rust (`chord_from_web`, which
 *  logs the fallback). The Rust capture keeps running until then so actions
 *  and RegisterHotKey stay paused. */
export function HotkeyRecorder({ hotkeys, onChange: onChangeProp, refreshKey = 0 }: Props) {
  const [desc, setDesc] = useState<Record<string, HotkeyDescription>>({});
  const [defaults, setDefaults] = useState<Record<string, string>>({});
  const [status, setStatus] = useState<HotkeyStatus | null>(null);
  const [capturing, setCapturing] = useState<string | null>(null);
  const [liveChord, setLiveChord] = useState("");
  const [note, setNote] = useState("");
  const [compat, setCompat] = useState(false);
  const [info, setInfo] = useState("");

  const capturingRef = useRef<string | null>(null);
  // Per-capture compatibility-mode state.
  const compatRef = useRef(false);
  const sawHookRef = useRef(false);
  const webChordRef = useRef(new WebChord());
  const fallbackTimer = useRef<number | null>(null);
  const mountedRef = useRef(true);
  const onChangeRef = useRef(onChangeProp);
  onChangeRef.current = onChangeProp;

  // Any edit drops the cached description so a stale chord is never shown.
  const onChange = (action: string, value: string) => {
    setDesc(prev => { const { [action]: _gone, ...rest } = prev; return rest; });
    onChangeProp(action, value);
  };

  const clearFallback = () => {
    if (fallbackTimer.current !== null) { clearTimeout(fallbackTimer.current); fallbackTimer.current = null; }
  };
  const resetCompat = () => {
    clearFallback();
    compatRef.current = false; setCompat(false);
    sawHookRef.current = false;
    webChordRef.current.reset();
  };
  const setCap = (a: string | null) => {
    capturingRef.current = a; setCapturing(a);
    resetCompat();
  };

  const stopCapture = useCallback((tellRust = true) => {
    if (capturingRef.current === null) return;
    setCap(null);
    setLiveChord("");
    if (tellRust) cancelHotkeyCapture().catch(() => {});
  }, []);

  // Compatibility mode finished: convert in Rust, then end the Rust capture.
  const finishCompat = useCallback(async () => {
    const action = capturingRef.current;
    if (action === null) return;
    const codes = webChordRef.current.codes();
    setCap(null); setLiveChord("");
    // End the Rust capture now (not after the await, which could cancel a
    // capture the user started meanwhile); its "cancelled" event is ignored.
    cancelHotkeyCapture().catch(() => {});
    try {
      const c = await chordFromWeb(codes);
      if (!isCancelChord(c.canonical)) {
        setNote("");
        setInfo("Recorded in compatibility mode.");
        onChangeRef.current(action, c.canonical);
      }
    } catch (e) {
      setNote(`Could not record that shortcut: ${e}`);
    }
  }, []);

  // No hook event arrived in time: record this capture from the WebView.
  const armFallback = useCallback(() => {
    if (compatRef.current || sawHookRef.current || fallbackTimer.current !== null) return;
    const action = capturingRef.current;
    fallbackTimer.current = window.setTimeout(() => {
      fallbackTimer.current = null;
      if (capturingRef.current !== action || sawHookRef.current) return;
      console.warn("[hotkeys] no hook capture events; recording in compatibility mode");
      compatRef.current = true; setCompat(true);
      setLiveChord(webChordDisplay(webChordRef.current.codes()));
      if (webChordRef.current.finished()) void finishCompat(); // already released
    }, CAPTURE_FALLBACK_MS);
  }, [finishCompat]);

  // Display strings come from Rust (single source of truth for chord grammar).
  const hotkeysKey = JSON.stringify(hotkeys);
  useEffect(() => {
    let current = true;
    describeHotkeys(hotkeys).then(d => { if (current) setDesc(d); }).catch(() => { if (current) setDesc({}); });
    return () => { current = false; };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hotkeysKey]);

  useEffect(() => {
    defaultHotkeys().then(setDefaults).catch(() => {});
  }, []);
  useEffect(() => {
    // Again after a moment: RegisterHotKey registrations are made by the
    // foreground watcher (<= 500 ms) after the listener restarted.
    let current = true;
    const read = () => getHotkeyStatus().then(s => { if (current) setStatus(s); }).catch(() => {});
    read();
    const id = window.setTimeout(read, 1200);
    return () => { current = false; clearTimeout(id); };
  }, [refreshKey]);

  // Capture progress from Rust.
  useEffect(() => {
    const p = listen<HotkeyCaptureEvent>("hotkey-capture", e => {
      const ev = e.payload;
      const action = capturingRef.current;
      if (action === null || compatRef.current) return; // compatibility mode ignores the hook
      if (ev.state !== "cancelled") { sawHookRef.current = true; clearFallback(); }
      if (ev.state === "holding") {
        setLiveChord(ev.display ?? "");
      } else if (ev.state === "done") {
        setCap(null); setLiveChord("");
        if (ev.canonical && !isCancelChord(ev.canonical)) {
          setNote("");
          onChangeRef.current(action, ev.canonical);
        }
      } else if (ev.reason === "user") {
        return; // UI-initiated; the UI already updated itself (a late one must not clobber a new capture)
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
  // (Re)set on mount too: React StrictMode mounts, unmounts and remounts in dev.
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      clearFallback();
      if (capturingRef.current !== null) cancelHotkeyCapture().catch(() => {});
    };
  }, []);

  // While capturing: cancel on blur / click elsewhere, keep input out of the page.
  useEffect(() => {
    if (capturing === null) return;
    // WebView events are always tracked, so keys already held when the
    // capture falls back to compatibility mode are part of the chord.
    const press = (code: string) => {
      webChordRef.current.down(code);
      if (compatRef.current) setLiveChord(webChordDisplay(webChordRef.current.codes()));
      else armFallback();
    };
    const release = (code: string) => {
      const done = webChordRef.current.up(code);
      if (compatRef.current && done) void finishCompat();
    };
    const onBlur = () => stopCapture();
    const onDown = (e: MouseEvent) => {
      if (e.button !== 0) { // side/middle buttons belong to the recorder
        e.preventDefault();
        const code = webMouseCode(e.button);
        if (code) press(code);
        return;
      }
      if (!(e.target as HTMLElement | null)?.closest?.("[data-capture-btn]")) stopCapture();
    };
    const onUp = (e: MouseEvent) => {
      if (e.button === 0) return;
      e.preventDefault();
      const code = webMouseCode(e.button);
      if (code) release(code);
    };
    const swallowAux = (e: MouseEvent) => { if (e.button !== 0) e.preventDefault(); };
    const onKeyDown = (e: KeyboardEvent) => { e.preventDefault(); e.stopPropagation(); press(e.code); };
    const onKeyUp = (e: KeyboardEvent) => { e.preventDefault(); e.stopPropagation(); release(e.code); };
    window.addEventListener("blur", onBlur);
    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("mouseup", onUp, true);
    document.addEventListener("auxclick", swallowAux, true);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("keyup", onKeyUp, true);
    return () => {
      window.removeEventListener("blur", onBlur);
      document.removeEventListener("mousedown", onDown, true);
      document.removeEventListener("mouseup", onUp, true);
      document.removeEventListener("auxclick", swallowAux, true);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("keyup", onKeyUp, true);
    };
  }, [capturing, stopCapture, armFallback, finishCompat]);

  const change = async (action: string) => {
    if (capturingRef.current === action) { stopCapture(); return; }
    const switching = capturingRef.current !== null;
    setCap(action); // synchronously, before any await
    setNote(""); setInfo(""); setLiveChord("");
    try {
      if (switching) await cancelHotkeyCapture().catch(() => {});
      await beginHotkeyCapture();
      if (!mountedRef.current) cancelHotkeyCapture().catch(() => {});
    } catch (e) {
      if (capturingRef.current === action) { setCap(null); }
      setNote(`Could not start recording: ${e}`);
    }
  };

  const canonicalOf = (a: string) => {
    const d = desc[a];
    if (!hotkeys[a]) return "";
    return d?.valid ? d.canonical : hotkeys[a];
  };
  const conflicts = findConflicts(
    Object.fromEntries(HOTKEY_ACTIONS.map(a => [a.key, canonicalOf(a.key)])),
  );
  // Another app owns this combo for RegisterHotKey (status reflects the
  // applied bindings; internal duplicates are never attempted).
  const sharedWithOtherApp = (action: string) =>
    usedByOtherApp(status?.bindings.find(x => x.action === action), canonicalOf(action));

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
                    <span className="hotkey-hint">
                      Press your shortcut… (Esc to cancel){compat ? " · compatibility mode" : ""}
                    </span>
                  </>
                ) : !raw ? (
                  <span className="hotkey-hint">Not set</span>
                ) : d?.valid ? (
                  <KeyCaps display={d.display} />
                ) : d ? (
                  <span className="hotkey-warn">Not a valid shortcut ({raw})</span>
                ) : (
                  <span className="hotkey-hint">{raw}</span>
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
      {info && <p className="hotkey-hint" style={{ fontSize: 10 }}>{info}</p>}
      <p style={{ fontSize: 10, color: "var(--text-muted)", marginTop: 4 }}>
        Click Change, then press any keys or mouse buttons (a single modifier works too).
        Shortcuts apply after Save Settings.
      </p>
    </div>
  );
}
