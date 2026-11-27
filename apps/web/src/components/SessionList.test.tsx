import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vite-plus/test";
import type { Session } from "../gen/Session";
import { makeSession } from "../test/fakes";
import { click, find, mount, unmountAll } from "../test/render";
import { upsertSession } from "../state/sessionsList";
import { SessionList } from "./SessionList";

afterEach(unmountAll);

const child = (id: string, at: number, parent: string, patch: Partial<Session> = {}): Session =>
  makeSession(id, at, { parent_session_id: parent, root_session_id: parent, ...patch });

function setup(initial: Session[], openId?: string) {
  const [sessions, setSessions] = createSignal<readonly Session[]>(
    initial.reduce<Session[]>(upsertSession, []),
  );
  const host = mount(() => <SessionList sessions={sessions()} now={10_000} openId={openId} />);
  return {
    host,
    hrefs: () => [...host.querySelectorAll("a")].map((link) => link.getAttribute("href")),
    toggle: (title: string) =>
      find<HTMLButtonElement>(host, `button[aria-label="Child sessions of ${title}"]`),
    upsert: (session: Session) => {
      setSessions((list) => upsertSession(list, session));
      flush();
    },
  };
}

describe("SessionList", () => {
  it("nests children under their parent, indented, in the order they were dispatched", () => {
    const { host, hrefs, toggle } = setup([
      makeSession("old", 100),
      makeSession("root", 500, { title: "Ship it" }),
      child("c2", 700, "root", { status: "running" }),
      child("c1", 600, "root", { status: "review" }),
    ]);
    expect(hrefs()).toEqual(["/s/root", "/s/c1", "/s/c2", "/s/old"]);
    const nested = find(host, 'ul[aria-label="Child sessions of Ship it"]');
    expect(nested.className).toContain("ml-4");
    expect([...nested.querySelectorAll("a")].map((link) => link.getAttribute("href"))).toEqual([
      "/s/c1",
      "/s/c2",
    ]);
    expect(nested.textContent).toContain("Child session");
    expect(toggle("Ship it").getAttribute("aria-expanded")).toBe("true");
    expect(toggle("Ship it").textContent).toContain("2");
    // A session with no children has nothing to fold.
    expect(host.querySelectorAll("button")).toHaveLength(1);
  });

  it("folds the children away once all are settled, and unfolds on request", async () => {
    const { hrefs, toggle } = setup([
      makeSession("root", 500, { title: "Ship it", status: "review" }),
      child("c1", 600, "root", { status: "done" }),
      child("c2", 700, "root", { status: "cancelled" }),
    ]);
    expect(hrefs()).toEqual(["/s/root"]);
    expect(toggle("Ship it").getAttribute("aria-expanded")).toBe("false");
    await click(toggle("Ship it"));
    expect(hrefs()).toEqual(["/s/root", "/s/c1", "/s/c2"]);
    await click(toggle("Ship it"));
    expect(hrefs()).toEqual(["/s/root"]);
  });

  it("follows the children's status until the reader chooses", async () => {
    const { hrefs, toggle, upsert } = setup([
      makeSession("root", 500, { title: "Ship it" }),
      child("c1", 600, "root", { status: "running" }),
    ]);
    expect(hrefs()).toEqual(["/s/root", "/s/c1"]);
    upsert(child("c1", 600, "root", { status: "review" }));
    expect(hrefs()).toEqual(["/s/root"]);
    // A new child that runs opens the group again.
    upsert(child("c2", 700, "root", { status: "running" }));
    expect(hrefs()).toEqual(["/s/root", "/s/c1", "/s/c2"]);
    await click(toggle("Ship it"));
    upsert(child("c3", 800, "root", { status: "running" }));
    expect(hrefs()).toEqual(["/s/root"]);
  });

  it("keeps the children in view while one of them is the open session", () => {
    const { hrefs } = setup(
      [makeSession("root", 500, { status: "done" }), child("c1", 600, "root", { status: "done" })],
      "c1",
    );
    expect(hrefs()).toEqual(["/s/root", "/s/c1"]);
  });

  it("shows a child whose parent is not listed at the top level", () => {
    const { host, hrefs } = setup([
      makeSession("a", 100),
      child("orphan", 200, "gone", { title: "Orphan" }),
    ]);
    expect(hrefs()).toEqual(["/s/orphan", "/s/a"]);
    expect(host.querySelector('ul[aria-label^="Child sessions"]')).toBeNull();
    // It is not marked as a child of anything on screen.
    expect(find(host, 'a[href="/s/orphan"]').textContent).toContain("mock");
  });
});

describe("SessionList: the queue", () => {
  it("says a queued session waits for a free slot; a child or a running one does not", () => {
    const { host } = setup([
      makeSession("waiting", 300, { title: "Waiting" }),
      makeSession("busy", 200, { status: "running", title: "Busy" }),
      child("kid", 400, "busy", { status: "queued" }),
    ]);
    const queued = [...host.querySelectorAll('[data-part="queued"]')];
    expect(queued.map((one) => one.textContent)).toEqual(["Queued: waiting for a free slot"]);
    expect(queued[0].closest("a")?.getAttribute("href")).toBe("/s/waiting");
  });
});
