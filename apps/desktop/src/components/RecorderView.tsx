import { useEffect, useState, useRef, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  startRecording, stopRecording, pauseRecording, resumeRecording,
  getRecordingStatus, listAudioDevices, listSystemAudioDevices,
  getMonitors, startReplay, stopReplay, saveReplay,
} from "../api";
import type {
  AudioConfig, AudioDevice, EncConfig, MonitorInfo,
  Region, Settings,
} from "../types";
import { autoSelectPlan, isGpuAudioDevice } from "../audioSelect";
import { FPS_OPTIONS, QUALITY_PRESETS as QP } from "../types";

interface Props {
  settings:       Settings;
  hwEncoder:      string;   // from App-level cache — no FFmpeg spawn on tab switch
  onStatus:       (msg: string) => void;
  onSettingsChange: (partial: Partial<Settings>) => void;
}

const REPLAY_OPTIONS = [
  { label: "Off",    secs: 0   },
  { label: "1 min",  secs: 60  },
  { label: "2 min",  secs: 120 },
  { label: "3 min",  secs: 180 },
  { label: "5 min",  secs: 300 },
] as const;

export function RecorderView({ settings, hwEncoder, onStatus, onSettingsChange }: Props) {
  // Devices (loaded once; refreshed via the ↻ button)
  const [micDevices, setMicDevices] = useState<AudioDevice[]>([]);
  const [sysDevices, setSysDevices] = useState<AudioDevice[]>([]);
  const [monitors,   setMonitors]   = useState<MonitorInfo[]>([]);

  // ── Config — initialised from persisted settings ──────────────────────────
  const [selectedMonitor, setSelectedMonitor] = useState(settings.selected_monitor ?? 0);
  const [fps,         setFps]        = useState(settings.default_fps);
  const [qualityIdx,  setQualityIdx] = useState(() => {
    const idx = QP.findIndex(p => p.crf === settings.default_quality_crf);
    return idx >= 0 ? idx : 2; // default High
  });
  const [micDevice,   setMicDevice]   = useState<string | null>(null);
  const [micDeviceId, setMicDeviceId] = useState<string | null>(settings.mic_device_id ?? null);
  const [sysDeviceId, setSysDeviceId] = useState<string | null>(settings.sys_audio_device_id ?? null);
  const [replayIdx,   setReplayIdx]   = useState(() => {
    const idx = REPLAY_OPTIONS.findIndex(o => o.secs === settings.replay_buffer_duration_secs);
    return idx >= 0 ? idx : 0;
  });

  // ── Sync local config state when settings prop updates (e.g. after async load) ──
  // Only update fields that aren't actively being changed by the user (not recording).
  // We track whether settings have been loaded from the backend to avoid overwriting
  // user changes with stale defaults.
  const settingsLoadedRef = useRef(false);
  useEffect(() => {
    // Skip the very first render — useState already consumed the initial settings.
    // Only sync once the real settings arrive from the backend (non-empty output_dir
    // is a reliable indicator that getSettings() has resolved).
    if (!settingsLoadedRef.current && settings.output_dir) {
      settingsLoadedRef.current = true;
      setSelectedMonitor(settings.selected_monitor ?? 0);
      setFps(settings.default_fps);
      setQualityIdx(() => {
        const idx = QP.findIndex(p => p.crf === settings.default_quality_crf);
        return idx >= 0 ? idx : 2;
      });
      const newMicId = settings.mic_device_id ?? null;
      setMicDeviceId(newMicId);
      // Also restore the display name so buildAudioCfg sends the correct legacy name
      if (newMicId) {
        setMicDevice(micDevices.find(d => d.id === newMicId)?.name ?? null);
      }
      setSysDeviceId(settings.sys_audio_device_id ?? null);
      setReplayIdx(() => {
        const idx = REPLAY_OPTIONS.findIndex(o => o.secs === settings.replay_buffer_duration_secs);
        return idx >= 0 ? idx : 0;
      });
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings]);

  // ── Recording state ───────────────────────────────────────────────────────
  const [isRecording,  setIsRecording]  = useState(false);
  const [isPaused,     setIsPaused]     = useState(false);
  const [elapsedSecs,  setElapsedSecs]  = useState(0);
  const [busy,         setBusy]         = useState(false);
  const [replayActive, setReplayActive] = useState(false);
  const [replayBusy,   setReplayBusy]   = useState(false);
  const [bufferedSecs, setBufferedSecs] = useState(0);
  const [restarts,     setRestarts]     = useState(0);

  const startedAtRef  = useRef<number>(0);
  const pausedDurRef  = useRef<number>(0);
  const pauseStartRef = useRef<number>(0);

  // ── Load devices on mount — no detectHwEncoder here (that spawns FFmpeg) ──
  // hwEncoder is passed from App where it's cached at startup.
  useEffect(() => {
    getMonitors().then(setMonitors).catch(() => {});
  }, []);

  // Device auto-selection persists to Settings (the Rust engine config reads
  // them), so wait until the real settings have arrived.
  useEffect(() => {
    if (settings.output_dir) loadDevices();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings.output_dir]);

  const loadDevices = () => {
    Promise.all([
      listAudioDevices().catch(() => [] as AudioDevice[]),
      listSystemAudioDevices().catch(() => [] as AudioDevice[]),
    ]).then(([mics, sys]) => {
      setMicDevices(mics);
      setSysDevices(sys);
      const plan = autoSelectPlan(
        { micId: settings.mic_device_id, sysId: settings.sys_audio_device_id },
        { mics, sys },
      );
      if (plan.selection.micId) {
        setMicDeviceId(plan.selection.micId);
        setMicDevice(plan.selection.micName);
      }
      if (plan.selection.sysId) setSysDeviceId(plan.selection.sysId);
      // One persist (and one engine re-apply) for everything auto-selected.
      if (plan.persist) persist(plan.persist);
    });
  };

  // ── Persist settings when config changes ──────────────────────────────────
  const persist = useCallback((changes: Partial<Settings>) => {
    onSettingsChange(changes);
  }, [onSettingsChange]);

  // ── Elapsed timer ─────────────────────────────────────────────────────────
  useEffect(() => {
    if (!isRecording) { setElapsedSecs(0); return; }
    const id = window.setInterval(() => {
      if (isPaused) return;
      setElapsedSecs(Math.max(0, (Date.now() - startedAtRef.current - pausedDurRef.current) / 1000));
    }, 250);
    return () => clearInterval(id);
  }, [isRecording, isPaused]);

  // ── Status sync ───────────────────────────────────────────────────────────
  // Hotkey actions run in Rust, so the backend is the source of truth: pull
  // its status every 3 s and immediately after a `hotkey-fired` event.
  const busyRef = useRef(busy); busyRef.current = busy;
  const replayBusyRef = useRef(replayBusy); replayBusyRef.current = replayBusy;
  const isRecordingRef = useRef(isRecording); isRecordingRef.current = isRecording;
  const isPausedRef = useRef(isPaused); isPausedRef.current = isPaused;
  const replayActiveRef = useRef(replayActive); replayActiveRef.current = replayActive;
  // epochRef is bumped by every local action; a poll that started before one
  // is stale and must not overwrite the state that action set.
  const epochRef = useRef(0);
  const syncRef = useRef<() => Promise<void>>(async () => {});
  syncRef.current = async () => {
    if (busy || replayBusy) return; // a click-driven action is mid-flight and will set the state itself
    const epoch = epochRef.current;
    try {
      const s = await getRecordingStatus();
      if (epoch !== epochRef.current || busyRef.current || replayBusyRef.current) return;
      const wasRecording = isRecordingRef.current;
      const wasPaused = isPausedRef.current;
      if (s.is_recording !== wasRecording) {
        setIsRecording(s.is_recording);
        if (s.is_recording) {
          startedAtRef.current = Date.now() - (s.elapsed_seconds ?? 0) * 1000;
          pausedDurRef.current = 0;
          onStatus("Recording started.");
        } else {
          setElapsedSecs(0);
          onStatus("Recording stopped.");
        }
      }
      const paused = s.is_recording && s.is_paused;
      if (paused !== wasPaused) {
        if (paused) pauseStartRef.current = Date.now();
        else if (wasRecording) pausedDurRef.current += Date.now() - pauseStartRef.current;
        setIsPaused(paused);
        if (s.is_recording && wasRecording) onStatus(paused ? "Paused." : "Resumed.");
      }
      if (s.replay_active !== replayActiveRef.current) {
        setReplayActive(s.replay_active);
        onStatus(s.replay_active ? "Replay buffer active." : "Replay buffer stopped.");
      }
      setBufferedSecs(s.buffered_secs ?? 0);
      setRestarts(s.restarts ?? 0);
    } catch { /* ignore */ }
  };
  useEffect(() => {
    const poll = window.setInterval(() => syncRef.current(), 3000);
    return () => clearInterval(poll);
  }, []);
  useEffect(() => {
    const p = listen("hotkey-fired", () => { syncRef.current(); });
    return () => { p.then(f => f()); };
  }, []);

  // Capture restarted underneath us (engine crash/respawn) — say so, unobtrusively.
  useEffect(() => {
    const p = listen("engine-restarted", () => onStatus("Capture restarted."));
    return () => { p.then(f => f()); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const elapsedStr = useCallback((secs: number) => {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    const s = Math.floor(secs % 60);
    return `${String(h).padStart(2,"0")}:${String(m).padStart(2,"0")}:${String(s).padStart(2,"0")}`;
  }, []);

  // ── GPU audio detection ───────────────────────────────────────────────────
  const isGpuSelected = sysDeviceId
    ? isGpuAudioDevice(sysDevices.find(d => d.id === sysDeviceId)?.name ?? "")
    : false;

  // ── Recording handlers ────────────────────────────────────────────────────
  const buildAudioCfg = (): AudioConfig => ({
    mic_device:    micDevice,
    sys_device_id: sysDeviceId,
    mic_device_id: micDeviceId,
  });

  const buildEncCfg = (): EncConfig => ({
    fps,
    quality_crf: QP[qualityIdx].crf,
    format: "mp4",   // recording is always MP4
    hw_encoder: hwEncoder,
  });

  const buildRegion = (): Region => {
    const m = monitors[selectedMonitor] ?? { index:0, x:0, y:0, width:1920, height:1080 };
    return { x: m.x??0, y: m.y??0, width: m.width??1920, height: m.height??1080, monitor: selectedMonitor };
  };

  // Every local action bumps the epoch before and after, so an in-flight
  // status poll can never overwrite what the action just set.
  const tracked = (f: () => Promise<void>) => async () => {
    epochRef.current++;
    try { await f(); } finally { epochRef.current++; }
  };
  // handleRecord is the UI button toggle (start if stopped, stop if recording)
  const handleRecord = async () => {
    if (isRecording) { await handleStopRecord(); } else { await handleStartRecord(); }
  };

  const handleStartRecordInner = async () => {
    if (busy || isRecording) return;
    setBusy(true);
    onStatus("Starting…");
    try {
      const path = await startRecording(buildRegion(), buildAudioCfg(), buildEncCfg());
      startedAtRef.current = Date.now(); pausedDurRef.current = 0;
      setIsRecording(true); setIsPaused(false);
      onStatus(`Recording → ${path.split(/[/\\]/).pop()}`);
    } catch (e: any) {
      onStatus(`Failed: ${e}`);
    } finally { setBusy(false); }
  };

  const handleStopRecordInner = async () => {
    if (busy || !isRecording) return;
    setBusy(true);
    setIsRecording(false); setIsPaused(false); setElapsedSecs(0);
    onStatus("Stopping…");
    try {
      const path = await stopRecording();
      onStatus(path ? `Saved: ${path.split(/[/\\]/).pop()}` : "Stopped.");
    } catch (e: any) {
      onStatus(`Stop error: ${e}`);
    } finally { setBusy(false); }
  };

  const handlePauseResumeInner = async () => {
    if (!isRecording || busy) return;
    try {
      if (isPaused) {
        await resumeRecording();
        pausedDurRef.current += Date.now() - pauseStartRef.current;
        setIsPaused(false); onStatus("Resumed.");
      } else {
        await pauseRecording();
        pauseStartRef.current = Date.now();
        setIsPaused(true); onStatus("Paused.");
      }
    } catch (e: any) { onStatus(`Error: ${e}`); }
  };

  // ── Replay buffer handlers ────────────────────────────────────────────────
  const handleReplayToggleInner = async () => {
    if (replayBusy || REPLAY_OPTIONS[replayIdx].secs === 0) return;
    setReplayBusy(true);
    try {
      if (replayActive) {
        await stopReplay();
        setReplayActive(false);
        onStatus("Replay buffer stopped.");
      } else {
        await startReplay(buildRegion(), buildAudioCfg(), buildEncCfg());
        setReplayActive(true);
        onStatus(`Replay buffer active (${REPLAY_OPTIONS[replayIdx].label}).`);
      }
    } catch (e: any) {
      onStatus(`Replay error: ${e}`);
    } finally { setReplayBusy(false); }
  };

  const handleSaveReplayInner = async () => {
    if (!replayActive || replayBusy) return;
    setReplayBusy(true);
    const secs = REPLAY_OPTIONS[replayIdx].secs;
    if (secs === 0) { onStatus("Set a replay duration first."); setReplayBusy(false); return; }
    onStatus("Saving replay…");
    try {
      const path = await saveReplay(secs);
      onStatus(path ? `Replay saved: ${path.split(/[/\\]/).pop()}` : "Replay saved.");
      // Buffer is auto-restarted on the backend — keep replayActive=true optimistically.
      // A status poll will correct it within 3s if restart actually failed.
      setReplayActive(true);
    } catch (e: any) {
      onStatus(`Replay save error: ${e}`);
      // On error the buffer state is unknown — let the status poll resolve it.
      setReplayActive(false);
    } finally { setReplayBusy(false); }
  };

  const handleStartRecord = tracked(handleStartRecordInner);
  const handleStopRecord = tracked(handleStopRecordInner);
  const handlePauseResume = tracked(handlePauseResumeInner);
  const handleReplayToggle = tracked(handleReplayToggleInner);
  const handleSaveReplay = tracked(handleSaveReplayInner);

  // ── Render ────────────────────────────────────────────────────────────────
  return (
    <div>
      {/* Capture */}
      <div className="card">
        <div className="card-title">Capture</div>
        <div className="form-row">
          <span className="form-label">Monitor</span>
          <select value={selectedMonitor} onChange={e => { const v=+e.target.value; setSelectedMonitor(v); persist({ selected_monitor: v }); }} disabled={isRecording}>
            {monitors.length > 0
              ? monitors.map((m, i) => <option key={i} value={i}>{m.name || `Display ${i+1}`} ({m.width}×{m.height})</option>)
              : <option value={0}>Display 1 (1920×1080)</option>}
          </select>
        </div>
        <div className="form-row">
          <span className="form-label">FPS</span>
          <select value={fps} onChange={e => { const v=+e.target.value; setFps(v); persist({ default_fps: v }); }} disabled={isRecording}>
            {FPS_OPTIONS.map(f => <option key={f} value={f}>{f}</option>)}
          </select>
        </div>
      </div>

      {/* Audio */}
      <div className="card">
        <div className="card-title" style={{ display:"flex", justifyContent:"space-between", alignItems:"center" }}>
          Audio
          <button
            className="btn btn-secondary"
            style={{ padding:"1px 8px", fontSize:11 }}
            title="Re-scan audio devices"
            onClick={loadDevices}
            disabled={isRecording}
          >↻</button>
        </div>

        <div className="form-row">
          <span className="form-label">Microphone</span>
          <select
            value={micDeviceId ?? ""}
            onChange={e => {
              const id = e.target.value || null;
              setMicDeviceId(id);
              const dev = micDevices.find(d => d.id === id);
              setMicDevice(dev?.name ?? null);
              persist({ mic_device_id: id ?? undefined, mic_device: dev?.name ?? undefined });
            }}
            disabled={isRecording}
          >
            <option value="">None</option>
            {micDevices.map(d => <option key={d.id} value={d.id}>{d.name}</option>)}
          </select>
        </div>

        <div className="form-row">
          <span className="form-label" title="Captures speaker output. Avoid NVIDIA/GPU audio — causes dropped frames.">
            System audio
          </span>
          <select
            value={sysDeviceId ?? ""}
            onChange={e => {
              const id = e.target.value || null;
              setSysDeviceId(id);
              persist({ sys_audio_device_id: id ?? undefined });
            }}
            disabled={isRecording}
            style={isGpuSelected ? { borderColor:"#cc6600", color:"#cc6600" } : {}}
          >
            <option value="">None</option>
            {sysDevices.map(d => (
              <option key={d.id} value={d.id}>
                {d.name}{d.kind === "loopback" ? " [Loopback]" : ""}{isGpuAudioDevice(d.name) ? " ⚠ GPU" : ""}
              </option>
            ))}
          </select>
        </div>
        {isGpuSelected && (
          <div style={{ fontSize:10, color:"#cc6600", marginTop:-4, marginBottom:4, padding:"0 4px" }}>
            ⚠ GPU audio shares hardware with the capture engine and may cause dropped frames.
          </div>
        )}
      </div>

      {/* Output */}
      <div className="card">
        <div className="card-title">Output</div>
        <div className="form-row">
          <span className="form-label">Quality</span>
          <select
            value={qualityIdx}
            onChange={e => { const i=+e.target.value; setQualityIdx(i); persist({ default_quality_crf: QP[i].crf }); }}
            disabled={isRecording}
          >
            {QP.map((p, i) => <option key={p.label} value={i}>{p.label}</option>)}
          </select>
        </div>
        <div className="form-row" style={{ fontSize:11, color:"var(--text-muted)" }}>
          <span className="form-label">Encoder</span>
          <span style={{ color:"var(--text)" }}>{hwEncoder}</span>
        </div>
      </div>

      {/* Replay buffer */}
      <div className="card">
        <div className="card-title">Replay Buffer</div>
        <div className="form-row">
          <span className="form-label" title="Keep a rolling buffer of recent footage. Hit 'Save Replay' to clip the last N minutes instantly.">
            Duration
          </span>
          <select
            value={replayIdx}
            onChange={e => { const i=+e.target.value; setReplayIdx(i); persist({ replay_buffer_duration_secs: REPLAY_OPTIONS[i].secs }); }}
            disabled={isRecording && replayActive}
          >
            {REPLAY_OPTIONS.map((o, i) => <option key={o.label} value={i}>{o.label}</option>)}
          </select>

          <button
            className={`btn btn-sm ${replayActive ? "btn-replay active" : "btn-replay"}`}
            style={{ marginLeft:8 }}
            onClick={() => handleReplayToggle()}
            disabled={replayBusy || REPLAY_OPTIONS[replayIdx].secs === 0}
            title={REPLAY_OPTIONS[replayIdx].secs === 0
              ? "Select a duration above to enable replay buffer"
              : replayActive
              ? "Stop replay buffer"
              : "Start replay buffer — records continuously in background"}
          >
            {replayActive ? "⏹ Stop" : "⏺ Start"}
          </button>

          <button
            className="btn btn-primary btn-sm"
            style={{ marginLeft:4 }}
            onClick={() => handleSaveReplay()}
            disabled={!replayActive || replayBusy}
            title={replayActive
              ? `Save the last ${REPLAY_OPTIONS[replayIdx].label} to a clip`
              : "Start the replay buffer first"}
          >
            💾 Save
          </button>
        </div>
        {replayActive && (
          <div style={{ fontSize:10, color:"var(--accent-green)", marginTop:-4, padding:"0 4px" }}>
            ● Replay buffer recording ({REPLAY_OPTIONS[replayIdx].label})
          </div>
        )}
        {replayActive && (
          <div style={{ fontSize:10, color:"var(--text-muted)", padding:"0 4px" }}>
            Buffer: {Math.min(Math.round(bufferedSecs), REPLAY_OPTIONS[replayIdx].secs)} s / {REPLAY_OPTIONS[replayIdx].secs} s
            {restarts > 0 && ` · capture restarted ${restarts}×`}
          </div>
        )}
      </div>

      {/* Controls */}
      <div style={{ display:"flex", gap:8, marginBottom:8 }}>
        <button
          className={`btn btn-record ${isRecording ? "recording" : ""}`}
          onClick={handleRecord}
          disabled={busy}
        >
          {busy
            ? (isRecording ? "Stopping…" : "Starting…")
            : isRecording ? "■  Stop Recording" : "●  Start Recording"}
        </button>
        {isRecording && (
          <button className="btn btn-secondary" onClick={() => handlePauseResume()} disabled={busy}>
            {isPaused ? "▶ Resume" : "⏸ Pause"}
          </button>
        )}
      </div>

      <div className="timer" style={isPaused ? { color:"var(--text-muted)" } : {}}>
        {elapsedStr(isRecording ? elapsedSecs : 0)}
      </div>
    </div>
  );
}
