import { describe, it, expect } from "vitest";
import { autoSelectPlan } from "./audioSelect";
import type { AudioDevice } from "./types";

const mic = (id: string): AudioDevice => ({ id, name: `Mic ${id}`, kind: "input" } as AudioDevice);
const out = (id: string, name = `Out ${id}`): AudioDevice => ({ id, name, kind: "output" } as AudioDevice);
const mics = [mic("m1"), mic("m2")];
const sys = [out("s1", "NVIDIA High Definition Audio"), out("s2", "Realtek Speakers")];

describe("autoSelectPlan", () => {
  it("nothing saved: selects first devices and persists both together", () => {
    const p = autoSelectPlan({ micId: null, sysId: null }, { mics, sys });
    expect(p.selection).toEqual({ micId: "m1", micName: "Mic m1", sysId: "s2" });
    expect(p.persist).toEqual({ mic_device_id: "m1", mic_device: "Mic m1", sys_audio_device_id: "s2" });
  });

  it("saved mic present: no persist for it", () => {
    const p = autoSelectPlan({ micId: "m2", sysId: "s2" }, { mics, sys });
    expect(p.selection.micId).toBe("m2");
    expect(p.persist).toBeNull();
  });

  it("saved mic absent: fallback shown, nothing persisted", () => {
    const p = autoSelectPlan({ micId: "gone", sysId: "s2" }, { mics, sys });
    expect(p.selection.micId).toBe("m1");
    expect(p.persist).toBeNull();
  });

  it("saved sys present, mic unsaved: persists only the mic", () => {
    const p = autoSelectPlan({ micId: null, sysId: "s1" }, { mics, sys });
    expect(p.selection.sysId).toBe("s1");
    expect(p.persist).toEqual({ mic_device_id: "m1", mic_device: "Mic m1" });
  });

  it("saved sys absent: fallback, not persisted", () => {
    const p = autoSelectPlan({ micId: "m1", sysId: "gone" }, { mics, sys });
    expect(p.selection.sysId).toBe("s2");
    expect(p.persist).toBeNull();
  });
});
