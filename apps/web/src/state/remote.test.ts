import { flush } from "solid-js";
import { describe, expect, it, vi } from "vite-plus/test";
import { deferred, settle } from "../test/fakes";
import { RemoteValue } from "./remote";

type Count = { status: "ready"; count: number } | { status: "error"; message: string };

function setup(read: () => Promise<number>) {
  const value = new RemoteValue<number, Count>({
    read,
    ready: (count) => ({ status: "ready", count }),
    failed: (error) => ({ status: "error", message: String(error) }),
  });
  const shown = () => {
    flush();
    return value.state();
  };
  return { value, shown };
}

describe("RemoteValue", () => {
  it("shows loading, then the answer, and asks once while a read is on its way", async () => {
    const answer = deferred<number>();
    const read = vi.fn(() => answer.promise);
    const { value, shown } = setup(read);
    expect(shown()).toEqual({ status: "loading" });

    value.read();
    value.read();
    expect(read).toHaveBeenCalledTimes(1);
    answer.resolve(3);
    await settle();
    expect(shown()).toEqual({ status: "ready", count: 3 });
    expect(value.known).toBe(true);
  });

  it("shows a failure until a value is known, then keeps the value through one", async () => {
    const read = vi
      .fn<() => Promise<number>>()
      .mockRejectedValueOnce("down")
      .mockResolvedValueOnce(1)
      .mockRejectedValue("down again");
    const { value, shown } = setup(read);

    value.read();
    await settle();
    expect(shown()).toEqual({ status: "error", message: "down" });
    value.read();
    await settle();
    value.read();
    await settle();
    expect(shown()).toEqual({ status: "ready", count: 1 });
  });

  it("refreshes only once wanted: everything, or only what never arrived", async () => {
    const read = vi.fn(() => Promise.resolve(1));
    const { value } = setup(read);
    value.refresh(true);
    expect(read).not.toHaveBeenCalled();

    value.want();
    await settle();
    expect(read).toHaveBeenCalledTimes(1);
    value.refresh(false);
    await settle();
    expect(read).toHaveBeenCalledTimes(1);
    value.refresh(true);
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("takes a value given by other means, and shows loading only while none is known", () => {
    const { value, shown } = setup(() => Promise.resolve(0));
    value.take(5);
    value.showLoading();
    expect(shown()).toEqual({ status: "ready", count: 5 });
  });

  it("forgets the value and the wish on reset", async () => {
    const read = vi.fn(() => Promise.resolve(2));
    const { value, shown } = setup(read);
    value.want();
    await settle();

    value.reset();
    expect(shown()).toEqual({ status: "loading" });
    expect(value.known).toBe(false);
    value.refresh(true);
    expect(read).toHaveBeenCalledTimes(1);
    value.showLoading();
    expect(shown()).toEqual({ status: "loading" });
  });
});
