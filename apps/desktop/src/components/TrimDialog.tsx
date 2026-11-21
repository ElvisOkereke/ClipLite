import { useState } from "react";
import { exportClip } from "../api";
import type { Clip, ExportFormat } from "../types";
import { EXPORT_FORMATS } from "../types";
import { save } from "@tauri-apps/plugin-dialog";
import { convertFileSrc } from "@tauri-apps/api/core";

interface Props {
  clip: Clip;
  onClose: () => void;
  onStatus: (msg: string) => void;
}

export function TrimDialog({ clip, onClose, onStatus }: Props) {
  const dur = clip.duration_s || 60;
  const [inTime, setInTime] = useState(0);
  const [outTime, setOutTime] = useState(dur);
  const [exporting, setExporting] = useState(false);
  const [format, setFormat] = useState<ExportFormat>("mp4");

  const fmt = (s: number) => {
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = Math.floor(s % 60);
    return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}`;
  };

  const parseTime = (s: string): number | null => {
    const parts = s.split(":");
    try {
      if (parts.length === 3) return +parts[0] * 3600 + +parts[1] * 60 + +parts[2];
      if (parts.length === 2) return +parts[0] * 60 + +parts[1];
      return +parts[0];
    } catch { return null; }
  };

  const handleExport = async () => {
    const ext = EXPORT_FORMATS.find(f => f.value === format)!.ext;
    const base = clip.filepath.replace(/\.[^.]+$/, "");
    const defaultPath = `${base}_trimmed.${ext}`;

    const outPath = await save({
      defaultPath,
      filters: [{ name: format.toUpperCase(), extensions: [ext] }],
    });

    if (!outPath) return;

    setExporting(true);
    try {
      await exportClip(clip.filepath, outPath, inTime, outTime, format);
      onStatus(`Saved: ${outPath.split(/[/\\]/).pop()}`);
      onClose();
    } catch (e: any) {
      onStatus(`Export failed: ${e}`);
    } finally {
      setExporting(false);
    }
  };

  // Overlay style
  const overlay: React.CSSProperties = {
    position: "fixed", inset: 0, background: "rgba(0,0,0,0.7)",
    display: "flex", alignItems: "center", justifyContent: "center", zIndex: 100,
  };
  const dialog: React.CSSProperties = {
    background: "var(--bg-card)", border: "1px solid var(--border)",
    borderRadius: 8, padding: 20, width: 480, maxWidth: "90vw",
  };

  return (
    <div style={overlay} onClick={e => e.target === e.currentTarget && onClose()}>
      <div style={dialog}>
        <h3 style={{ marginBottom: 12, fontSize: 13 }}>✂ Trim — {clip.filename}</h3>

        {clip.thumbnail && (
          <img
            src={convertFileSrc(clip.thumbnail)}
            alt="preview"
            style={{ width: "100%", borderRadius: 4, marginBottom: 12, maxHeight: 140, objectFit: "cover" }}
          />
        )}

        <div style={{ marginBottom: 12 }}>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11, color: "var(--text-muted)", marginBottom: 4 }}>
            <span>In: {fmt(inTime)}</span>
            <span>Out: {fmt(outTime)}</span>
          </div>

          <div style={{ marginBottom: 8 }}>
            <label style={{ fontSize: 11, color: "var(--text-muted)" }}>In point</label>
            <input
              type="range" min={0} max={dur * 10} value={inTime * 10}
              onChange={e => setInTime(Math.min(+e.target.value / 10, outTime - 0.1))}
              style={{ width: "100%", accentColor: "var(--accent)" }}
            />
          </div>

          <div>
            <label style={{ fontSize: 11, color: "var(--text-muted)" }}>Out point</label>
            <input
              type="range" min={0} max={dur * 10} value={outTime * 10}
              onChange={e => setOutTime(Math.max(+e.target.value / 10, inTime + 0.1))}
              style={{ width: "100%", accentColor: "var(--accent)" }}
            />
          </div>
        </div>

        <div style={{ display: "flex", gap: 8, marginBottom: 12 }}>
          <div style={{ flex: 1 }}>
            <label className="form-label">In time</label>
            <input
              type="text" value={fmt(inTime)}
              onChange={e => { const t = parseTime(e.target.value); if (t !== null) setInTime(Math.max(0, Math.min(t, outTime - 0.1))); }}
            />
          </div>
          <div style={{ flex: 1 }}>
            <label className="form-label">Out time</label>
            <input
              type="text" value={fmt(outTime)}
              onChange={e => { const t = parseTime(e.target.value); if (t !== null) setOutTime(Math.max(inTime + 0.1, Math.min(t, dur))); }}
            />
          </div>
        </div>

        <div style={{ marginBottom: 12 }}>
          <label className="form-label">Format</label>
          <select value={format} onChange={e => setFormat(e.target.value as ExportFormat)} disabled={exporting}>
            {EXPORT_FORMATS.map(f => <option key={f.value} value={f.value}>{f.label}</option>)}
          </select>
        </div>

        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
          <button className="btn btn-secondary" onClick={onClose} disabled={exporting}>Cancel</button>
          <button className="btn btn-primary" onClick={handleExport} disabled={exporting}>
            {exporting ? "Exporting…" : "Export"}
          </button>
        </div>
      </div>
    </div>
  );
}
