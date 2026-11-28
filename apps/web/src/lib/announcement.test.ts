import { describe, expect, it } from "vite-plus/test";
import { announceNewest } from "./announcement";

const say = (newest: { seq: number }, others: number) => `item ${newest.seq}, ${others} others`;

describe("announceNewest", () => {
  it("says nothing for an empty list at first", () => {
    expect(announceNewest(undefined, [], say)).toEqual({ seq: 0, text: "" });
  });

  it("reads out the newest item, with how many others wait", () => {
    expect(announceNewest(undefined, [{ seq: 3 }], say)).toEqual({
      seq: 3,
      text: "item 3, 0 others",
    });
    expect(
      announceNewest({ seq: 3, text: "item 3, 0 others" }, [{ seq: 3 }, { seq: 5 }], say),
    ).toEqual({ seq: 5, text: "item 5, 1 others" });
  });

  it("keeps the same text, unchanged, when no newer item arrived", () => {
    const last = { seq: 5, text: "item 5, 1 others" };
    expect(announceNewest(last, [{ seq: 3 }], say)).toBe(last);
    expect(announceNewest(last, [{ seq: 3 }, { seq: 5 }], say)).toBe(last);
  });

  it("clears once the list is empty, and stays cleared", () => {
    const cleared = announceNewest({ seq: 5, text: "item 5, 1 others" }, [], say);
    expect(cleared).toEqual({ seq: 5, text: "" });
    expect(announceNewest(cleared, [], say)).toBe(cleared);
    // An item older than the last one read out is not read out again.
    expect(announceNewest(cleared, [{ seq: 4 }], say)).toBe(cleared);
  });
});
