import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { PermissionDecision } from "../gen/PermissionDecision";
import type { PendingPermission } from "../state/sessionReducer";
import { conflict, deferred } from "../test/fakes";
import { click, control, find, findByText, mount, type, unmountAll } from "../test/render";
import { PermissionBar } from "./PermissionBar";

afterEach(unmountAll);

const pendingRequest = (id: string, seq: number, title = `run ${id}`): PendingPermission => ({
  request: {
    id,
    tool_name: "Bash",
    tool_kind: "execute",
    title,
    input: { command: title, timeout: 60 },
    item_id: null,
  },
  agentId: "a1",
  ts: 1000 + seq,
  seq,
});

type Resolve = (requestId: string, decision: PermissionDecision, message?: string) => Promise<void>;

function setup(initial: PendingPermission[], onResolve: Resolve = () => Promise.resolve()) {
  const [pending, setPending] = createSignal<readonly PendingPermission[]>(initial);
  const resolve = vi.fn(onResolve);
  const host = mount(() => <PermissionBar pending={pending()} onResolve={resolve} />);
  const button = (label: string) => findByText<HTMLButtonElement>(host, "button", label);
  /** What the events do once the server has recorded an answer. */
  const arrive = (next: PendingPermission[]) => {
    setPending(next);
    flush();
  };
  return { host, resolve, button, arrive, bar: () => host.querySelector("section") };
}

describe("PermissionBar", () => {
  it("shows nothing but its live region while no request is pending", () => {
    const { host, bar } = setup([]);
    expect(bar()).toBeNull();
    expect(find(host, '[role="status"]').getAttribute("aria-live")).toBe("polite");
    expect(find(host, '[role="status"]').textContent).toBe("");
  });

  it("shows the oldest request: its kind, its title and its input as JSON", () => {
    const { host, bar } = setup([pendingRequest("p1", 3, "cargo test"), pendingRequest("p2", 5)]);
    const section = find(host, 'section[aria-label="Permission request"]');
    expect(section.textContent).toContain("execute");
    expect(section.textContent).toContain("cargo test");
    expect(section.textContent).not.toContain("run p2");

    const input = find<HTMLDetailsElement>(section, "details");
    expect(input.open).toBe(false);
    expect(find(input, "summary").textContent).toContain("Bash");
    expect(find(input, "pre").textContent).toBe(
      '{\n  "command": "cargo test",\n  "timeout": 60\n}',
    );
    expect(bar()).toBe(section);
  });

  it("sends allow, stays disabled until the event arrives, then leaves", async () => {
    const answer = deferred();
    const { resolve, button, arrive, bar } = setup([pendingRequest("p1", 1)], () => answer.promise);

    await click(button("Allow"));
    expect(resolve.mock.calls).toEqual([["p1", "allow", undefined]]);
    // In flight: neither answer can be given twice.
    expect(button("Allow").disabled).toBe(true);
    expect(button("Deny").disabled).toBe(true);
    expect(bar()?.textContent).toContain("Sending…");

    answer.resolve();
    await click(button("Allow"));
    // The server said yes, but the screen only moves with the session's events.
    expect(bar()).not.toBeNull();
    expect(button("Allow").disabled).toBe(true);
    expect(bar()?.textContent).toContain("Sent, waiting for the agent…");
    expect(resolve).toHaveBeenCalledTimes(1);

    arrive([]);
    expect(bar()).toBeNull();
  });

  it("asks for an optional reason before denying, and sends it", async () => {
    const { host, resolve, button } = setup([pendingRequest("p1", 1)]);
    expect(host.querySelector('input[type="text"]')).toBeNull();

    await click(button("Deny"));
    expect(resolve).not.toHaveBeenCalled();
    const reason = control(host, "Reason for the agent");
    expect(document.activeElement).toBe(reason);

    type(reason, "  Not on main.  ");
    await click(button("Confirm deny"));
    expect(resolve.mock.calls).toEqual([["p1", "deny", "Not on main."]]);
  });

  it("denies without a reason when none is given, and with Enter in the field", async () => {
    const { host, resolve, button } = setup([pendingRequest("p1", 1)]);
    await click(button("Deny"));
    find<HTMLFormElement>(host, "form").dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
    await click(host);
    expect(resolve.mock.calls).toEqual([["p1", "deny", undefined]]);
  });

  it("goes back from the reason field without answering", async () => {
    const { host, resolve, button } = setup([pendingRequest("p1", 1)]);
    await click(button("Deny"));
    await click(button("Back"));
    expect(host.querySelector('input[type="text"]')).toBeNull();
    expect(button("Allow").disabled).toBe(false);
    expect(resolve).not.toHaveBeenCalled();
  });

  it("disables the reason form while a denial is in flight", async () => {
    const answer = deferred();
    const { host, button } = setup([pendingRequest("p1", 1)], () => answer.promise);
    await click(button("Deny"));
    await click(button("Confirm deny"));

    expect(button("Sending…").disabled).toBe(true);
    expect(button("Back").disabled).toBe(true);
    expect(control(host, "Reason for the agent").disabled).toBe(true);
    answer.resolve();
    await click(host);
  });

  it("shows the server's message when an answer is refused, and can be tried again", async () => {
    const onResolve = vi
      .fn<Resolve>()
      .mockRejectedValueOnce(conflict("request was already answered"))
      .mockResolvedValue();
    const { host, resolve, button, bar } = setup([pendingRequest("p1", 1)], onResolve);

    await click(button("Allow"));
    expect(find(host, '[role="alert"]').textContent).toBe("request was already answered");
    // Nothing changed on screen but the message: the request is still there to answer.
    expect(bar()).not.toBeNull();
    expect(button("Allow").disabled).toBe(false);
    expect(button("Deny").disabled).toBe(false);

    await click(button("Allow"));
    expect(resolve).toHaveBeenCalledTimes(2);
    expect(host.querySelector('[role="alert"]')).toBeNull();
  });

  it("keeps the reason field open with its text when a denial is refused", async () => {
    const { host, button } = setup([pendingRequest("p1", 1)], () =>
      Promise.reject(new Error("Cannot reach the server")),
    );
    await click(button("Deny"));
    type(control(host, "Reason for the agent"), "Too risky");
    await click(button("Confirm deny"));

    expect(find(host, '[role="alert"]').textContent).toBe("Cannot reach the server");
    expect(control(host, "Reason for the agent").value).toBe("Too risky");
    expect(button("Confirm deny").disabled).toBe(false);
  });

  it("counts several requests and steps through them", async () => {
    const all = [pendingRequest("p1", 1), pendingRequest("p2", 2), pendingRequest("p3", 3)];
    const { host, resolve, button, arrive, bar } = setup(all);
    const previous = find<HTMLButtonElement>(host, 'button[aria-label="Previous request"]');
    const next = find<HTMLButtonElement>(host, 'button[aria-label="Next request"]');
    expect(bar()?.textContent).toContain("1 of 3");
    expect(bar()?.textContent).toContain("run p1");
    expect(previous.disabled).toBe(true);

    await click(next);
    expect(bar()?.textContent).toContain("2 of 3");
    expect(bar()?.textContent).toContain("run p2");

    await click(button("Allow"));
    expect(resolve.mock.calls).toEqual([["p2", "allow", undefined]]);
    // The others can still be answered while that one is on its way.
    await click(next);
    expect(bar()?.textContent).toContain("run p3");
    expect(button("Allow").disabled).toBe(false);
    expect(next.disabled).toBe(true);

    // p2's answer lands: the bar stays on the request the reader is looking at.
    arrive([all[0], all[2]]);
    expect(bar()?.textContent).toContain("2 of 2");
    expect(bar()?.textContent).toContain("run p3");

    // The one in view is answered elsewhere: back to the oldest.
    arrive([all[0]]);
    expect(bar()?.textContent).toContain("run p1");
    expect(bar()?.textContent).not.toContain(" of ");
  });

  it("announces each request once, when it arrives", () => {
    const p1 = pendingRequest("p1", 1, "cargo test");
    const p2 = pendingRequest("p2", 2, "git push");
    const { host, arrive } = setup([]);
    const live = find(host, '[role="status"]');

    arrive([p1]);
    expect(live.textContent).toBe("Permission requested: execute, cargo test.");

    arrive([p1, p2]);
    expect(live.textContent).toBe("Permission requested: execute, git push. 1 more waiting.");

    // p2 is answered: p1 was already read out, so the region must not change and repeat it.
    const spoken = live.textContent;
    arrive([p1]);
    expect(live.textContent).toBe(spoken);

    arrive([]);
    expect(live.textContent).toBe("");
  });

  it("is operable from the keyboard: every control is a native button or field", async () => {
    const { host, button } = setup([pendingRequest("p1", 1), pendingRequest("p2", 2)]);
    await click(button("Deny"));
    const controls = [...host.querySelectorAll("section button, section input, section summary")];
    expect(controls.length).toBeGreaterThan(4);
    for (const element of controls) {
      expect(element.getAttribute("tabindex")).toBeNull();
    }
    expect(find(host, "section pre").getAttribute("tabindex")).toBe("0");
  });
});
