import { flush } from "solid-js";
import { describe, expect, it, vi } from "vite-plus/test";
import { conflict, deferred } from "../test/fakes";
import { createPermissionAnswers } from "./permissionAnswers";

describe("permission answers", () => {
  it("sends an answer once, and holds the request until its resolution arrives", async () => {
    const pending = deferred();
    const resolve = vi.fn(() => pending.promise);
    const answers = createPermissionAnswers(resolve);
    expect(answers.waiting("p1")).toBe(false);

    const first = answers.answer("p1", "deny", "Not now.");
    const second = answers.answer("p1", "allow");
    flush();
    expect(resolve).toHaveBeenCalledTimes(1);
    expect(resolve).toHaveBeenCalledWith("p1", "deny", "Not now.");
    expect([answers.waiting("p1"), answers.sending("p1")]).toEqual([true, true]);
    // Another request is not held by this one.
    expect(answers.waiting("p2")).toBe(false);

    pending.resolve();
    await expect(first).resolves.toBe(true);
    await expect(second).resolves.toBe(false);
    flush();
    // Accepted: still waiting, for the event, and asking again would be a 409.
    expect([answers.waiting("p1"), answers.sending("p1")]).toEqual([true, false]);
    await expect(answers.answer("p1", "allow")).resolves.toBe(false);
    expect(resolve).toHaveBeenCalledTimes(1);
  });

  it("keeps the server's reason when an answer is refused, and lets it be given again", async () => {
    const resolve = vi
      .fn<(id: string, decision: "allow" | "deny") => Promise<void>>()
      .mockRejectedValueOnce(conflict("the request was already answered"))
      .mockResolvedValue();
    const answers = createPermissionAnswers(resolve);

    await expect(answers.answer("p1", "allow")).resolves.toBe(false);
    flush();
    expect(answers.error("p1")).toBe("the request was already answered");
    expect(answers.waiting("p1")).toBe(false);
    expect(answers.error("p2")).toBeUndefined();

    await expect(answers.answer("p1", "allow")).resolves.toBe(true);
    flush();
    expect(answers.error("p1")).toBeUndefined();
    expect(answers.waiting("p1")).toBe(true);
  });
});
