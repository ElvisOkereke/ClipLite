import { describe, it, expect } from "vitest";
import {
  splitDisplay, findConflicts, isCancelChord, elevatedNoticeText,
  usedByOtherApp, WebChord, webMouseCode, hotkeysChanged, webChordDisplay, type BindingStatus,
} from "./hotkeys";

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

describe("elevatedNoticeText", () => {
  it("names the app and is honest about what may not work", () => {
    const t = elevatedNoticeText("game.exe");
    expect(t).toContain("game.exe is running as administrator");
    expect(t).toContain("Windows may block ClipLite's shortcuts");
    expect(t).toContain("restart ClipLite as administrator");
    expect(t).toContain("Keyboard-only");
    expect(t).not.toMatch(/still work/i);
  });
  it("falls back to a generic name", () => {
    expect(elevatedNoticeText("")).toMatch(/^An elevated app is running as administrator/);
  });
});

describe("usedByOtherApp", () => {
  const b = (over: Partial<BindingStatus>): BindingStatus => ({
    action: "start", canonical: "Ctrl+Alt+F9", parsed: true,
    registerable: true, registered: false, register_error: null, ...over,
  });
  it("only a failed RegisterHotKey attempt counts", () => {
    expect(usedByOtherApp(b({ register_error: "Hot key is already registered." }), "Ctrl+Alt+F9")).toBe(true);
    // Not registered right now (nothing focused needs it) is not a conflict.
    expect(usedByOtherApp(b({}), "Ctrl+Alt+F9")).toBe(false);
    // Internal duplicates are never attempted.
    expect(usedByOtherApp(b({ registerable: false }), "Ctrl+Alt+F9")).toBe(false);
  });
  it("ignores status for a different (unsaved) chord", () => {
    expect(usedByOtherApp(b({ register_error: "x" }), "Ctrl+Alt+F10")).toBe(false);
    expect(usedByOtherApp(undefined, "Ctrl+Alt+F9")).toBe(false);
    expect(usedByOtherApp(b({ register_error: "x" }), "")).toBe(false);
  });
});

describe("WebChord", () => {
  it("records the union and finishes when everything is released", () => {
    const c = new WebChord();
    c.down("ControlLeft");
    c.down("AltLeft");
    c.down("F12");
    c.down("F12"); // repeat
    expect(c.up("F12")).toBe(false);
    expect(c.up("AltLeft")).toBe(false);
    expect(c.up("ControlLeft")).toBe(true);
    expect(c.codes()).toEqual(["ControlLeft", "AltLeft", "F12"]);
  });
  it("counts a key seen only on keyup (PrintScreen)", () => {
    const c = new WebChord();
    c.down("ControlLeft");
    expect(c.up("PrintScreen")).toBe(false);
    expect(c.up("ControlLeft")).toBe(true);
    expect(c.codes()).toEqual(["ControlLeft", "PrintScreen"]);
  });
  it("finished() reports a released chord", () => {
    const c = new WebChord();
    expect(c.finished()).toBe(false);
    c.down("KeyA");
    expect(c.finished()).toBe(false);
    c.up("KeyA");
    expect(c.finished()).toBe(true);
  });
  it("ignores empty codes and reset clears", () => {
    const c = new WebChord();
    c.down("");
    expect(c.up("")).toBe(false);
    c.down("KeyA");
    c.reset();
    expect(c.codes()).toEqual([]);
  });
});

describe("webMouseCode", () => {
  it("maps middle and side buttons", () => {
    expect(webMouseCode(1)).toBe("Mouse3");
    expect(webMouseCode(3)).toBe("Mouse4");
    expect(webMouseCode(4)).toBe("Mouse5");
    expect(webMouseCode(0)).toBeNull();
    expect(webMouseCode(2)).toBeNull();
  });
});

describe("hotkeysChanged", () => {
  it("compares bindings regardless of key order", () => {
    expect(hotkeysChanged({ a: "Ctrl+F1", b: "" }, { b: "", a: "Ctrl+F1" })).toBe(false);
    expect(hotkeysChanged({ a: "Ctrl+F1" }, { a: "Ctrl+F2" })).toBe(true);
    expect(hotkeysChanged({ a: "Ctrl+F1" }, { a: "Ctrl+F1", b: "F3" })).toBe(true);
    expect(hotkeysChanged({ a: "Ctrl+F1", b: "" }, { a: "Ctrl+F1" })).toBe(true);
  });
});

describe("webChordDisplay", () => {
  it("labels codes like key caps", () => {
    expect(webChordDisplay(["ControlLeft", "AltRight", "KeyK"])).toBe("Ctrl + Alt + K");
    expect(webChordDisplay(["MetaLeft", "Digit1", "ArrowLeft", "Mouse4"])).toBe("Win + 1 + ArrowLeft + Mouse 4");
    expect(webChordDisplay([])).toBe("");
  });
});
