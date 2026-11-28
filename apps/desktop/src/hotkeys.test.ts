import { describe, it, expect } from "vitest";
import { splitDisplay, findConflicts, isCancelChord, HOTKEY_ACTIONS, actionLabel } from "./hotkeys";

describe("splitDisplay", () => {
  it("splits a chord into key caps", () => {
    expect(splitDisplay("Ctrl + Shift + F2")).toEqual(["Ctrl", "Shift", "F2"]);
  });
  it("keeps punctuation keys", () => {
    expect(splitDisplay("Ctrl + `")).toEqual(["Ctrl", "`"]);
  });
  it("keeps a literal plus key", () => {
    expect(splitDisplay("Ctrl + +")).toEqual(["Ctrl", "+"]);
  });
  it("single key and empty", () => {
    expect(splitDisplay("F2")).toEqual(["F2"]);
    expect(splitDisplay("")).toEqual([]);
  });
});

describe("findConflicts", () => {
  it("reports both sides of a duplicate", () => {
    const c = findConflicts({ start: "Ctrl+Shift+R", stop: "Ctrl+Shift+S", replay_save: "ctrl+shift+r" });
    expect(c).toEqual({ start: "replay_save", replay_save: "start" });
  });
  it("is empty without duplicates, ignoring unbound actions", () => {
    expect(findConflicts({ start: "F1", stop: "F2", pause: "", library: "" })).toEqual({});
  });
});

describe("isCancelChord", () => {
  it("only a lone Escape cancels", () => {
    expect(isCancelChord("Escape")).toBe(true);
    expect(isCancelChord("Esc")).toBe(true);
    expect(isCancelChord("Ctrl+Escape")).toBe(false);
    expect(isCancelChord("F2")).toBe(false);
  });
});

describe("actions", () => {
  it("labels the six supported actions", () => {
    expect(HOTKEY_ACTIONS.map(a => a.key)).toEqual(["start", "stop", "pause", "replay_toggle", "replay_save", "library"]);
    expect(actionLabel("replay_save")).toBe("Save replay");
    expect(actionLabel("zzz")).toBe("zzz");
  });
});
