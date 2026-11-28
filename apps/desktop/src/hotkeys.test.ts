import { describe, it, expect } from "vitest";
import { splitDisplay, findConflicts, isCancelChord, elevatedNoticeText } from "./hotkeys";

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
