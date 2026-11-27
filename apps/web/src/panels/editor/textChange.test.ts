import { describe, expect, it } from "vite-plus/test";
import { minimalChange } from "./textChange";

const apply = (before: string, after: string): string => {
  const change = minimalChange(before, after);
  if (!change) return before;
  return before.slice(0, change.from) + change.insert + before.slice(change.to);
};

describe("minimalChange", () => {
  it("is nothing when the texts are equal", () => {
    expect(minimalChange("same", "same")).toBeUndefined();
  });

  it("touches only the middle that differs, so a caret outside it keeps its place", () => {
    expect(minimalChange("one\ntwo\nthree\n", "one\nTWO\nthree\n")).toEqual({
      from: 4,
      to: 7,
      insert: "TWO",
    });
    expect(minimalChange("abc", "abXc")).toEqual({ from: 2, to: 2, insert: "X" });
    expect(minimalChange("abXc", "abc")).toEqual({ from: 2, to: 3, insert: "" });
  });

  it.each([
    ["", "new"],
    ["old", ""],
    ["aaa", "aa"],
    ["aa", "aaa"],
    ["abab", "ab"],
    ["prefix-only", "prefix-only and more"],
    ["x\ny\n", "y\n"],
    ["héllo wörld", "héllo, wörld!"],
  ])("turns %j into %j", (before, after) => {
    expect(apply(before, after)).toBe(after);
  });
});
