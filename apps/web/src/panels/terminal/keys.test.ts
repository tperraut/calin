import { describe, expect, it } from "vite-plus/test";
import { controlCode, keysAfterCtrl, keysBeforeCtrl } from "./keys";

describe("terminal keys", () => {
  it("turns a key into its control code", () => {
    expect(controlCode("c")).toBe("\x03");
    expect(controlCode("C")).toBe("\x03");
    expect(controlCode("d")).toBe("\x04");
    expect(controlCode("z")).toBe("\x1a");
    expect(controlCode("[")).toBe("\x1b");
    expect(controlCode("?")).toBe("\x7f");
    expect(controlCode("-")).toBe("\x1f");
    expect(controlCode(" ")).toBe("\x00");
  });

  it("leaves alone what Ctrl does not change", () => {
    expect(controlCode("1")).toBeUndefined();
    expect(controlCode("\x1b[A")).toBeUndefined();
    expect(controlCode("")).toBeUndefined();
  });

  it("offers the keys a phone keyboard lacks", () => {
    const caps = [...keysBeforeCtrl, ...keysAfterCtrl].map((key) => key.cap);
    expect(caps).toEqual(["Esc", "Tab", "←", "↑", "↓", "→", "|", "/", "-", "~"]);
    expect(keysBeforeCtrl.map((key) => key.sequence(false))).toEqual(["\x1b", "\t"]);
  });

  it("sends arrows in the mode the running program asked for", () => {
    const [left, up, down, right] = keysAfterCtrl;
    expect([left, up, down, right].map((key) => key.sequence(false))).toEqual([
      "\x1b[D",
      "\x1b[A",
      "\x1b[B",
      "\x1b[C",
    ]);
    expect(up.sequence(true)).toBe("\x1bOA");
  });
});
