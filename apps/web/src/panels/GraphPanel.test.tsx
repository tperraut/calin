import { createSignal, flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { Api } from "../api/http";
import type { Graph } from "../gen/Graph";
import type { Session } from "../gen/Session";
import type { Timeline } from "../gen/Timeline";
import { conflict, makeSession } from "../test/fakes";
import { makeEdge, makeGraph, makeTimeline, T0 } from "../test/orchestration";
import { click, find, findByText, mount, unmountAll } from "../test/render";
import { openWorkspace, type Workspace } from "../test/workspace";
import { REFETCH_DELAY_MS, SCRUB_DELAY_MS } from "./graph/data";
import GraphPanel from "./GraphPanel";

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
});
afterEach(() => {
  unmountAll();
  vi.useRealTimers();
});

/** Lets requests answer and the screen follow, with the clock moved on by `ms`. */
async function pass(ms = 0): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
  flush();
  await vi.advanceTimersByTimeAsync(0);
  flush();
}

const root = makeSession("s1", 1000, { title: "Ship the health check", status: "running" });
const childApi = makeSession("s2", 2000, {
  title: "API: add /health",
  status: "review",
  parent_session_id: "s1",
  root_session_id: "s1",
});

type Opened = Workspace & {
  host: HTMLElement;
  getGraph: ReturnType<typeof vi.fn<Api["getGraph"]>>;
  getTimeline: ReturnType<typeof vi.fn<Api["getTimeline"]>>;
  setSessions: (sessions: readonly Session[]) => void;
};

async function open(
  options: { graph?: () => Promise<Graph>; timeline?: () => Promise<Timeline> } = {},
): Promise<Opened> {
  const getGraph = vi.fn<Api["getGraph"]>(
    options.graph ?? ((_id, at) => Promise.resolve(makeGraph({ at: at ?? T0 + 60_000 }))),
  );
  const getTimeline = vi.fn<Api["getTimeline"]>(
    options.timeline ?? (() => Promise.resolve(makeTimeline())),
  );
  const [sessions, setSessions] = createSignal<readonly Session[]>([root, childApi]);
  const workspace = await openWorkspace({ routes: { getGraph, getTimeline }, sessions });
  const host = mount(() => (
    <GraphPanel store={workspace.store} active={workspace.active()} links={workspace.links} />
  ));
  await pass();
  return {
    ...workspace,
    host,
    getGraph,
    getTimeline,
    setSessions(next) {
      setSessions(next);
      flush();
    },
  };
}

const node = (host: HTMLElement, id: string): SVGGElement =>
  find<SVGGElement>(host, `[data-node="${id}"]`);
const tap = async (element: Element): Promise<void> => {
  element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  await pass();
};
const details = (host: HTMLElement): HTMLElement => find(host, 'section[aria-label="Details"]');
const facts = (host: HTMLElement): Record<string, string> =>
  Object.fromEntries(
    [...details(host).querySelectorAll("dt")].map((term) => [
      term.textContent,
      term.nextElementSibling?.textContent ?? "",
    ]),
  );
const button = (host: HTMLElement, label: string): HTMLButtonElement =>
  findByText<HTMLButtonElement>(host, "button", label);

describe("GraphPanel: the graph", () => {
  it("reads the graph of the session's tree when it opens, and draws every node and edge", async () => {
    const { host, getGraph } = await open();
    expect(getGraph.mock.calls).toEqual([["s1", undefined]]);
    expect(host.querySelectorAll("[data-node]")).toHaveLength(10);
    // Eleven edges from the server, and the line from the user to the lead that no edge stands for.
    expect(host.querySelectorAll("[data-edge]")).toHaveLength(12);
    expect(find(host, "svg.graph-canvas").getAttribute("aria-label")).toBe("Session graph");
    // The view toggle, with the graph on.
    const toggle = find(host, '[role="group"][aria-label="View"]');
    expect(
      [...toggle.querySelectorAll("button")].map((one) => [
        one.textContent,
        one.getAttribute("aria-pressed"),
      ]),
    ).toEqual([
      ["Graph", "true"],
      ["Timeline", "false"],
    ]);
  });

  it("marks the open session's agents, and gives each node a name that says its status in words", async () => {
    const { host } = await open();
    const own = [...host.querySelectorAll("[data-own]")].map((one) =>
      one.getAttribute("data-node"),
    );
    expect(own).toEqual(["agent:a1", "agent:sub1", "agent:sub2"]);
    expect(node(host, "agent:a1").getAttribute("aria-label")).toBe(
      "lead, Running, $0.0421, in the open session",
    );
    expect(node(host, "agent:a2").getAttribute("aria-label")).toBe(
      "worker (API: add /health), Review",
    );
    expect(node(host, "repo:docs").getAttribute("aria-label")).toBe("Repo docs (Read only)");
    expect(node(host, "user").getAttribute("data-kind")).toBe("user");
    // On the card: the status as a shape with its word, and the cost.
    const lead = node(host, "agent:a1");
    expect(find(lead, ".gmark").getAttribute("data-tone")).toBe("active");
    expect(find(lead, ".gsub").textContent).toBe("Running");
    expect(find(lead, ".gcost").textContent).toBe("$0.0421");
    expect(node(host, "agent:sub1").querySelector(".gcost")).toBeNull();
    expect(find(node(host, "agent:sub1"), ".gsub").textContent).toBe("Done");
  });

  it("shows an edge's count when it is above one", async () => {
    const { host } = await open();
    const count = (id: string) =>
      host.querySelector(`[data-edge="${id}"] .gcount text`)?.textContent;
    expect(count("message:agent:a2:agent:a1")).toBe("2");
    expect(count("changed:agent:a2:repo:api")).toBe("4");
    expect(count("message:agent:a1:agent:a2")).toBeUndefined();
    // Spawned and dispatched edges follow the tree; the others are curves over it.
    expect(
      find(host, '[data-edge="dispatched:agent:a1:agent:a2"] .gline').getAttribute("d"),
    ).toMatch(/^M[\d.]+,[\d.]+V/);
    expect(find(host, '[data-edge="report:agent:a2:agent:a1"] .gline').getAttribute("d")).toMatch(
      /C/,
    );
  });

  it("keeps a text alternative in sync: who is under whom, the repos, a sentence per edge", async () => {
    const { host, emit, getGraph } = await open();
    const text = find(host, '[data-part="graph-text"]');
    const lead = [...text.querySelectorAll("li")].find((item) =>
      item.textContent.startsWith("lead, Running"),
    );
    expect(lead?.parentElement?.parentElement?.firstChild?.textContent).toBe("You");
    expect(
      [...(lead?.querySelector("ul")?.children ?? [])].map((item) => item.firstChild?.textContent),
    ).toEqual([
      "explorer, Done, in the open session",
      "reviewer, Running, in the open session",
      "worker (API: add /health), Review",
      "worker (Web: status badge), Running",
    ]);
    expect(text.textContent).toContain("Repo api (Worktree)");
    expect(text.textContent).toContain("lead, $0.0421".slice(0, 4));
    expect([...text.querySelectorAll("h4 + ul")].at(-1)?.textContent).toContain(
      "worker (API: add /health) sent 2 notes to lead",
    );
    expect(text.textContent).toContain("worker (API: add /health) changed 4 files in api");
    expect(text.textContent).toContain("lead dispatched worker (Web: status badge)");
    expect(text.textContent).toContain("worker (API: add /health) sent 1 report to lead");

    getGraph.mockResolvedValueOnce(makeGraph({ nodes: makeGraph().nodes.slice(0, 2), edges: [] }));
    emit({ type: "user_message", text: "again" }, null);
    await pass(REFETCH_DELAY_MS);
    expect(text.querySelectorAll("li")).toHaveLength(2);
    expect(text.textContent).not.toContain("explorer");
  });

  it("opens an agent's details: role, status, runner, model, cost, tokens, its session", async () => {
    const { host, shownAgents } = await open();
    await tap(node(host, "agent:a2"));
    expect(find(details(host), "h2").textContent).toBe("worker");
    expect(facts(host)).toEqual({
      Role: "lead",
      Status: "Idle",
      Runner: "mock",
      Model: "mock-1",
      Cost: "$0.00",
      Tokens: "0 in · 0 out",
      Session: "API: add /health",
      "Session status": "Review",
    });
    expect(find<HTMLAnchorElement>(details(host), "a.btn").getAttribute("href")).toBe("/s/s2");
    // Not an agent of the open session: its rows are in another transcript.
    expect(details(host).textContent).not.toContain("Show in transcript");
    expect(node(host, "agent:a2").getAttribute("aria-pressed")).toBe("true");

    // A subagent of the open session: its session is the lead's.
    await tap(node(host, "agent:sub1"));
    expect(facts(host)).toMatchObject({
      Role: "explorer",
      Status: "Done",
      Session: "Ship the health check",
    });
    await click(button(details(host), "Show in transcript"));
    expect(shownAgents).toEqual(["sub1"]);

    await click(find(details(host), 'button[aria-label="Close details"]'));
    expect(host.querySelector('section[aria-label="Details"]')).toBeNull();
  });

  it("opens a repo's details: name, mode, who changed it and how much, and the way to its changes", async () => {
    const { host, panels } = await open();
    await tap(node(host, "repo:api"));
    expect(facts(host)).toEqual({ Repo: "api", Mode: "Worktree" });
    const changers = find(details(host), 'ul[aria-label="Changed by"]');
    expect([...changers.querySelectorAll("li")].map((item) => item.textContent)).toEqual([
      "worker (API: add /health)4 files",
      "lead1 file",
    ]);
    await click(button(details(host), "Open changes"));
    expect(panels).toEqual(["changes"]);

    await tap(node(host, "repo:docs"));
    expect(facts(host)).toEqual({ Repo: "docs", Mode: "Read only" });
    expect(details(host).textContent).toContain("Nobody changed it yet.");
  });

  it.each([
    ["spawned:agent:a1:agent:sub1", "Spawned subagent", "lead spawned explorer", "1", undefined],
    [
      "dispatched:agent:a1:agent:a2",
      "Dispatched child session",
      "lead dispatched worker (API: add /health)",
      "1",
      "Add GET /health to the API.",
    ],
    [
      "message:agent:a2:agent:a1",
      "Notes",
      "worker (API: add /health) sent 2 notes to lead",
      "2",
      "Done, tests next.",
    ],
    [
      "report:agent:a2:agent:a1",
      "Report",
      "worker (API: add /health) sent 1 report to lead",
      "1",
      "GET /health returns the version.",
    ],
    [
      "changed:agent:a2:repo:api",
      "Changed files",
      "worker (API: add /health) changed 4 files in api",
      "4",
      "tests/health.rs",
    ],
  ])(
    "opens the details of the edge %s: its kind in words, count, last time and label",
    async (id, kind, sentence, count, label) => {
      const { host } = await open();
      await tap(find(host, `[data-edge="${id}"] .ghit`));
      expect(find(details(host), "h2").textContent).toBe(kind);
      expect(details(host).textContent).toContain(sentence);
      const shown = facts(host);
      expect(shown.Kind).toBe(kind);
      expect(shown.Count).toBe(count);
      expect(shown.Last).toMatch(/\(.+\)$/);
      expect(shown.Latest).toBe(label);
      expect(find(host, `[data-edge="${id}"]`).hasAttribute("data-selected")).toBe(true);
    },
  );

  it("reaches an edge from the details of either of its ends", async () => {
    const { host } = await open();
    await tap(node(host, "agent:a1"));
    const relations = find(details(host), 'ul[aria-label="Relations"]');
    expect(relations.querySelectorAll("button").length).toBe(8);
    await click(button(relations, "sent 1 report to lead"));
    expect(find(details(host), "h2").textContent).toBe("Report");
  });

  it("moves focus along the tree with the arrows, and opens details with Enter", async () => {
    const { host } = await open();
    const press = async (key: string): Promise<void> => {
      document.activeElement?.dispatchEvent(
        new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
      );
      await pass();
    };
    // One tab stop: the first node.
    const stops = [...host.querySelectorAll('[data-node][tabindex="0"]')];
    expect(stops.map((one) => one.getAttribute("data-node"))).toEqual(["user"]);
    node(host, "user").focus();
    await press("ArrowDown");
    expect(document.activeElement).toBe(node(host, "agent:a1"));
    await press("ArrowRight");
    expect(document.activeElement).toBe(node(host, "agent:sub1"));
    await press("ArrowDown");
    await press("ArrowDown");
    await press("ArrowDown");
    await press("ArrowRight");
    expect(document.activeElement).toBe(node(host, "agent:sub3"));
    await press("ArrowLeft");
    expect(document.activeElement).toBe(node(host, "agent:a3"));
    expect(node(host, "agent:a3").getAttribute("tabindex")).toBe("0");
    await press("End");
    expect(document.activeElement).toBe(node(host, "repo:docs"));
    await press("Enter");
    expect(find(details(host), "h2").textContent).toBe("docs");
    await press("Escape");
    expect(host.querySelector('section[aria-label="Details"]')).toBeNull();
  });

  it("reads again, once, a moment after the open session records an event", async () => {
    const { emit, getGraph } = await open();
    emit({ type: "user_message", text: "one" }, null);
    emit({ type: "user_message", text: "two" }, null);
    await pass(REFETCH_DELAY_MS - 50);
    expect(getGraph).toHaveBeenCalledTimes(1);
    await pass(50);
    expect(getGraph).toHaveBeenCalledTimes(2);
  });

  it("reads again when a session of the tree is upserted, and not for a session outside it", async () => {
    const { getGraph, setSessions } = await open();
    setSessions([root, childApi, makeSession("other", 3000)]);
    await pass(REFETCH_DELAY_MS + 50);
    expect(getGraph).toHaveBeenCalledTimes(1);
    setSessions([root, { ...childApi, head_seq: 12 }]);
    await pass(REFETCH_DELAY_MS);
    expect(getGraph).toHaveBeenCalledTimes(2);
    // A new child of the tree.
    setSessions([
      root,
      childApi,
      makeSession("s3", 4000, { parent_session_id: "s1", root_session_id: "s1" }),
    ]);
    await pass(REFETCH_DELAY_MS);
    expect(getGraph).toHaveBeenCalledTimes(3);
  });

  it("reads nothing while another panel shows or the tab is in the background, then catches up", async () => {
    const { emit, getGraph, setActive } = await open();
    setActive(false);
    emit({ type: "user_message", text: "unseen" }, null);
    await pass(REFETCH_DELAY_MS * 3);
    expect(getGraph).toHaveBeenCalledTimes(1);
    setActive(true);
    await pass();
    expect(getGraph).toHaveBeenCalledTimes(2);

    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    emit({ type: "user_message", text: "in the background" }, null);
    await pass(REFETCH_DELAY_MS * 3);
    expect(getGraph).toHaveBeenCalledTimes(2);
    visibility.mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    await pass();
    expect(getGraph).toHaveBeenCalledTimes(3);
  });

  it("sends a pulse along an edge whose count went up, and none when motion is to be reduced", async () => {
    const busier = (): Graph => {
      const graph = makeGraph();
      return {
        ...graph,
        edges: graph.edges
          .map((edge) => (edge.kind === "report" ? { ...edge, count: 2 } : edge))
          .concat(makeEdge("changed", "agent:a3", "repo:web")),
      };
    };
    const first = await open();
    expect(first.host.querySelectorAll(".gpulse")).toHaveLength(0);
    first.getGraph.mockResolvedValueOnce(busier());
    first.emit({ type: "user_message", text: "more" }, null);
    await pass(REFETCH_DELAY_MS);
    // The report that came again, and the edge that is new.
    expect(first.host.querySelectorAll(".gpulse")).toHaveLength(2);
    unmountAll();

    const media = vi.spyOn(window, "matchMedia").mockImplementation(
      (query) =>
        ({
          matches: query.includes("reduce"),
          addEventListener() {},
          removeEventListener() {},
        }) as unknown as MediaQueryList,
    );
    const calm = await open();
    calm.getGraph.mockResolvedValueOnce(busier());
    calm.emit({ type: "user_message", text: "more" }, null);
    await pass(REFETCH_DELAY_MS);
    expect(calm.host.querySelectorAll(".gpulse")).toHaveLength(0);
    expect(calm.host.querySelectorAll("[data-edge]")).toHaveLength(13);
    media.mockRestore();
  });

  it("asks for the graph at the slider's moment, and comes back to the present with Live", async () => {
    const { host, getGraph } = await open();
    const slider = find<HTMLInputElement>(host, 'input[type="range"][aria-label="Moment"]');
    expect([slider.min, slider.max, slider.value]).toEqual([
      String(T0),
      String(T0 + 60_000),
      String(T0 + 60_000),
    ]);
    const live = button(host, "Live");
    expect(live.getAttribute("aria-pressed")).toBe("true");

    slider.value = String(T0 + 20_000);
    slider.dispatchEvent(new InputEvent("input", { bubbles: true }));
    slider.value = String(T0 + 30_000);
    slider.dispatchEvent(new InputEvent("input", { bubbles: true }));
    await pass();
    expect(live.getAttribute("aria-pressed")).toBe("false");
    expect(getGraph).toHaveBeenCalledTimes(1);
    await pass(SCRUB_DELAY_MS);
    expect(getGraph.mock.calls.at(-1)).toEqual(["s1", T0 + 30_000]);
    expect(getGraph).toHaveBeenCalledTimes(2);
    expect(find(host, "output").getAttribute("title")).toBe(
      new Date(T0 + 30_000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }),
    );

    await click(live);
    await pass();
    expect(button(host, "Live").getAttribute("aria-pressed")).toBe("true");
    expect(getGraph.mock.calls.at(-1)).toEqual(["s1", undefined]);
  });

  it("replays with Play, which becomes Pause, and stops where it is paused", async () => {
    const { host, getGraph } = await open();
    await click(find(host, 'button[aria-label="Play"]'));
    await pass();
    expect(getGraph.mock.calls.at(-1)).toEqual(["s1", T0]);
    expect(host.querySelector('button[aria-label="Play"]')).toBeNull();
    await pass(500);
    expect(getGraph.mock.calls.at(-1)).toEqual(["s1", T0 + 1500]);
    await click(find(host, 'button[aria-label="Pause"]'));
    await pass(3000);
    expect(getGraph.mock.calls.at(-1)).toEqual(["s1", T0 + 1500]);
    expect(find(host, 'button[aria-label="Play"]')).toBeTruthy();
    expect(button(host, "Live").getAttribute("aria-pressed")).toBe("false");
  });

  it("says so while loading, when there is nothing yet, and when the server refuses, with its reason", async () => {
    const empty = await open({ graph: () => Promise.resolve(makeGraph({ nodes: [], edges: [] })) });
    expect(empty.host.textContent).toContain("Nothing to draw yet");
    unmountAll();

    let fail = true;
    const refused = await open({
      graph: () =>
        fail
          ? Promise.reject(conflict("the session's log is being rebuilt"))
          : Promise.resolve(makeGraph()),
    });
    const alert = find(refused.host, '[role="alert"]');
    expect(alert.textContent).toContain(
      "The graph could not be loaded: the session's log is being rebuilt",
    );
    fail = false;
    await click(button(alert, "Try again"));
    await pass();
    expect(refused.host.querySelectorAll("[data-node]")).toHaveLength(10);
    // A later failure keeps the graph on screen, under what went wrong.
    fail = true;
    refused.emit({ type: "user_message", text: "x" }, null);
    await pass(REFETCH_DELAY_MS);
    expect(find(refused.host, '[role="alert"]').textContent).toContain(
      "The graph could not be updated: the session's log is being rebuilt",
    );
    expect(refused.host.querySelectorAll("[data-node]")).toHaveLength(10);
  });
});

describe("GraphPanel: the timeline", () => {
  async function timeline(options: Parameters<typeof open>[0] = {}) {
    const opened = await open(options);
    await click(button(find(opened.host, '[aria-label="View"]'), "Timeline"));
    await pass();
    return opened;
  }
  const span = (host: HTMLElement, key: string): SVGGElement =>
    find<SVGGElement>(host, `[data-span="${key}"]`);

  it("is read when first shown, with one lane per agent in the server's order, indented by depth", async () => {
    const opened = await open();
    expect(opened.getTimeline).not.toHaveBeenCalled();
    await click(button(find(opened.host, '[aria-label="View"]'), "Timeline"));
    await pass();
    expect(opened.getTimeline.mock.calls).toEqual([["s1"]]);
    const lanes = [...opened.host.querySelectorAll<HTMLElement>("[data-lane]")];
    expect(
      lanes.map((lane) => [
        lane.textContent,
        lane.style.paddingLeft,
        lane.hasAttribute("data-own"),
      ]),
    ).toEqual([
      ["lead · Ship the health check", "6px", true],
      ["explorer", "16px", true],
      ["lead · API: add /health", "16px", false],
    ]);
    expect(find(opened.host, 'svg[aria-label="Timeline"]')).toBeTruthy();
  });

  it("draws turns, tools inside them by kind, waits hatched, and open spans up to now", async () => {
    const { host } = await timeline();
    expect(host.querySelectorAll("[data-span]")).toHaveLength(6);
    expect(find(span(host, "a1/t1"), "rect.tl-tool").getAttribute("data-series")).toBe("run");
    expect(find(span(host, "a2/c1"), "rect.tl-tool").getAttribute("data-series")).toBe("write");
    expect(span(host, "a1/wait-1").querySelector("rect.tl-waiting")).not.toBeNull();
    expect(find(host, "pattern").id).toBe("tl-hatch");
    expect(
      [...host.querySelectorAll("[data-open]")].map((one) => one.getAttribute("data-span")),
    ).toEqual(["a2/turn-a2", "a2/c1"]);
    // Rows follow lanes: 44px each.
    expect(span(host, "a2/turn-a2").getAttribute("transform")).toMatch(/,88\)$/);
    expect(span(host, "sub1/turn-sub1").getAttribute("transform")).toMatch(/,44\)$/);
    expect(span(host, "a2/c1").getAttribute("aria-label")).toMatch(
      /^lead · API: add \/health: Tool call: src\/health.rs, .+ so far, Running$/,
    );
  });

  it("draws an arrow per message, dispatch and spawn, with a marker per kind", async () => {
    const { host } = await timeline();
    const arrows = [...host.querySelectorAll<SVGGElement>("[data-arrow]")];
    expect(arrows.map((arrow) => arrow.getAttribute("data-kind"))).toEqual([
      "message",
      "spawned",
      "dispatched",
      "report",
    ]);
    const shapes = arrows.map((arrow) => find(arrow, "path.tl-marker").getAttribute("d"));
    expect(new Set(shapes).size).toBe(4);
    // From the user: it comes down from the axis into the lead's lane.
    expect(find(arrows[0], "line").getAttribute("y1")).toBe("-6");
    expect(find(arrows[0], "line").getAttribute("y2")).toBe("22");
    // The report climbs from the child's lane to the lead's.
    expect(find(arrows[3], "line").getAttribute("y1")).toBe("110");
    expect(find(arrows[3], "line").getAttribute("y2")).toBe("22");
  });

  it("shows a span's title, kind, start, duration and status, and leads a tool call to the transcript", async () => {
    const { host, emit, shownItems } = await timeline();
    emit({
      type: "item_started",
      item: {
        id: "t1",
        status: "running",
        content: {
          kind: "tool",
          name: "Bash",
          tool_kind: "execute",
          title: "cargo test --workspace",
          input: {},
          output: null,
          locations: [],
        },
      },
    });
    await tap(span(host, "a1/t1"));
    expect(find(details(host), "h2").textContent).toBe("cargo test --workspace");
    const shown = facts(host);
    expect(shown).toMatchObject({
      Kind: "Tool call",
      Tool: "execute",
      Agent: "lead · Ship the health check",
      Duration: "12 s",
      Status: "Done",
    });
    expect(shown.Start).toContain(
      new Date(T0 + 5000).toLocaleTimeString(undefined, {
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        hour12: false,
      }),
    );
    await click(button(details(host), "Show in transcript"));
    expect(shownItems).toEqual(["t1"]);

    // A turn has no item; a child's tool call is in another transcript.
    await tap(span(host, "a1/turn-1"));
    expect(facts(host)).toMatchObject({ Kind: "Turn", Duration: "40 s", Status: "Ended" });
    expect(details(host).textContent).not.toContain("Show in transcript");
    await tap(span(host, "a2/c1"));
    expect(facts(host).Duration).toMatch(/so far$/);
    expect(facts(host).Status).toBe("Running");
    expect(details(host).textContent).not.toContain("Show in transcript");
    await tap(span(host, "a1/wait-1"));
    expect(facts(host)).toMatchObject({ Kind: "Waiting for you", Duration: "10 s" });
  });

  it("shows an arrow's kind, label and time", async () => {
    const { host } = await timeline();
    await tap(find(host, '[data-arrow="ar3"]'));
    expect(find(details(host), "h2").textContent).toBe("Dispatched child session");
    expect(facts(host)).toMatchObject({
      Kind: "Dispatched child session",
      From: "lead · Ship the health check",
      To: "lead · API: add /health",
      Label: "API: add /health",
    });
    await tap(find(host, '[data-arrow="ar1"]'));
    expect(facts(host)).toMatchObject({ Kind: "Note", From: "You", Label: "Ship it" });
  });

  it("only holds the spans near the viewport once zoomed in, and all of them again with Fit", async () => {
    const { host } = await timeline();
    const zoomIn = find<HTMLButtonElement>(host, 'button[aria-label="Zoom in"]');
    for (let turn = 0; turn < 8; turn += 1) await click(zoomIn);
    const held = [...host.querySelectorAll("[data-span]")].map((one) =>
      one.getAttribute("data-span"),
    );
    expect(held).toContain("a1/turn-1");
    expect(held).not.toContain("a2/c1");
    expect(host.querySelector('[data-arrow="ar4"]')).toBeNull();
    await click(button(host, "Fit"));
    expect(host.querySelectorAll("[data-span]")).toHaveLength(6);
    expect(host.querySelectorAll("[data-arrow]")).toHaveLength(4);
  });

  it("keeps a text alternative: per lane, the spans in order with their times and durations", async () => {
    const { host } = await timeline();
    const text = find(host, '[data-part="timeline-text"]');
    const lanes = [...(text.querySelector("ul")?.children ?? [])];
    expect(lanes.map((lane) => lane.firstChild?.textContent)).toEqual([
      "lead · Ship the health check",
      "explorer",
      "lead · API: add /health",
    ]);
    const lead = lanes[0].textContent;
    expect(lead).toMatch(/Ship the health check, \d\d:\d\d:\d\d, 40 s/);
    expect(lead).toMatch(/Tool call: cargo test --workspace, \d\d:\d\d:\d\d, 12 s, Done/);
    expect(lead).toMatch(/Waiting for you: Question: header or footer\?, \d\d:\d\d:\d\d, 10 s/);
    expect(lanes[2].textContent).toMatch(/API: add \/health, \d\d:\d\d:\d\d, .+ so far/);
    expect(text.textContent).toMatch(
      /Report from lead · API: add \/health to lead · Ship the health check: Done/,
    );
    expect(text.textContent).toMatch(/Note from you to lead · Ship the health check: Ship it/);
  });

  it("moves between spans with the arrows and opens one with Enter", async () => {
    const { host } = await timeline();
    const press = async (key: string): Promise<void> => {
      document.activeElement?.dispatchEvent(
        new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
      );
      await pass();
    };
    expect(
      [...host.querySelectorAll('[data-span][tabindex="0"]')].map((one) =>
        one.getAttribute("data-span"),
      ),
    ).toEqual(["a1/turn-1"]);
    span(host, "a1/turn-1").focus();
    await press("ArrowRight");
    expect(document.activeElement).toBe(span(host, "a1/t1"));
    await press("ArrowDown");
    expect(document.activeElement).toBe(span(host, "sub1/turn-sub1"));
    await press("ArrowDown");
    expect(document.activeElement).toBe(span(host, "a2/turn-a2"));
    await press("ArrowUp");
    await press("Enter");
    expect(find(details(host), "h2").textContent).toBe("Explore");
  });

  it("says so when there is no lane yet, and when the server refuses", async () => {
    const empty = await timeline({
      timeline: () => Promise.resolve(makeTimeline({ lanes: [], arrows: [] })),
    });
    expect(empty.host.textContent).toContain("a lane appears when an agent starts");
    unmountAll();
    const refused = await timeline({
      timeline: () => Promise.reject(conflict("no timeline for a deleted session")),
    });
    expect(find(refused.host, '[role="alert"]').textContent).toContain(
      "The timeline could not be loaded: no timeline for a deleted session",
    );
  });
});
