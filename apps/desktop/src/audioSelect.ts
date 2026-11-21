import type { AudioDevice, Settings } from "./types";

export const isGpuAudioDevice = (name: string) => {
  const l = name.toLowerCase();
  return l.includes("nvidia") || l.includes("amd") ||
         (l.includes("high definition audio") && !l.includes("realtek")) ||
         l.includes("radeon");
};

export interface AutoSelectPlan {
  selection: { micId: string | null; micName: string | null; sysId: string | null };
  /** Settings to persist, or null. Only devices Settings has NO value for are
   *  persisted; a saved-but-absent device gets a session-only fallback. */
  persist: Partial<Settings> | null;
}

export function autoSelectPlan(
  saved: { micId?: string | null; sysId?: string | null },
  available: { mics: AudioDevice[]; sys: AudioDevice[] },
): AutoSelectPlan {
  const persist: Partial<Settings> = {};

  let micDev = saved.micId ? available.mics.find(d => d.id === saved.micId) : undefined;
  if (!micDev) {
    micDev = available.mics[0];
    if (micDev && !saved.micId) {
      persist.mic_device_id = micDev.id;
      persist.mic_device = micDev.name;
    }
  }

  const outputs = available.sys.filter(d => d.kind === "output" || d.kind === "loopback");
  let sysDev = saved.sysId ? available.sys.find(d => d.id === saved.sysId) : undefined;
  if (!sysDev) {
    sysDev = outputs.find(d => !isGpuAudioDevice(d.name)) ?? outputs[0];
    if (sysDev && !saved.sysId) persist.sys_audio_device_id = sysDev.id;
  }

  return {
    selection: { micId: micDev?.id ?? null, micName: micDev?.name ?? null, sysId: sysDev?.id ?? null },
    persist: Object.keys(persist).length ? persist : null,
  };
}
