import { describe, expect, it } from "vite-plus/test";
import { leaveQuestion, withShown } from "./PanelHost";

describe("leaveQuestion", () => {
  const unsaved = "A file has unsaved changes.";
  const asked = "A file has unsaved changes. Leave this session and lose them?";

  it("asks before leaving for another session or the home page", () => {
    expect(leaveQuestion(unsaved, "/", "/s/s1")).toBe(asked);
    expect(leaveQuestion(unsaved, "/s/s2/files", "/s/s1")).toBe(asked);
    // Only a whole segment is the same session.
    expect(leaveQuestion(unsaved, "/s/s10", "/s/s1")).toBe(asked);
  });

  it("lets the session's own panels through, whatever their query or fragment", () => {
    expect(leaveQuestion(unsaved, "/s/s1", "/s/s1")).toBeUndefined();
    expect(leaveQuestion(unsaved, "/s/s1/changes", "/s/s1")).toBeUndefined();
    expect(leaveQuestion(unsaved, "/s/s1?agent=a1", "/s/s1")).toBeUndefined();
    expect(leaveQuestion(unsaved, "/s/s1#item-3", "/s/s1")).toBeUndefined();
  });

  it("lets anything through while nothing is unsaved, and the browser's Back always", () => {
    expect(leaveQuestion(undefined, "/", "/s/s1")).toBeUndefined();
    expect(leaveQuestion(unsaved, -1, "/s/s1")).toBeUndefined();
  });
});

describe("withShown", () => {
  it("adds a panel the first time it is shown, and keeps the same set otherwise", () => {
    const first = withShown(undefined, "files");
    expect([...first]).toEqual(["files"]);
    expect(withShown(first, "files")).toBe(first);
    expect(withShown(first, undefined)).toBe(first);
    const second = withShown(first, "terminal");
    expect([...second]).toEqual(["files", "terminal"]);
    expect([...first]).toEqual(["files"]);
    expect([...withShown(undefined, undefined)]).toEqual([]);
  });
});
