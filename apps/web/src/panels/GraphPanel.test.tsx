import { createSignal, flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { Api } from "../api/http";
import type { Graph } from "../gen/Graph";
import type { Session } from "../gen/Session";
import type { Timeline } from "../gen/Timeline";
import type { WorkflowGraph } from "../gen/WorkflowGraph";
import type { WorkflowNodeInfo } from "../gen/WorkflowNodeInfo";
import type { WorkflowRun } from "../gen/WorkflowRun";
import { startRun } from "../lib/workflow";
import { conflict, makeDetail, makeSession } from "../test/fakes";
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
    // Spawned and dispatched edges follow the tree; the others run in lanes over it,
    // straight runs with rounded corners.
    expect(
      find(host, '[data-edge="dispatched:agent:a1:agent:a2"] .gline').getAttribute("d"),
    ).toMatch(/^M[\d.]+,[\d.]+V/);
    const report = find(host, '[data-edge="report:agent:a2:agent:a1"] .gline').getAttribute("d");
    expect(report).toMatch(/^M[\d.]+,[\d.]+L[\d.]+,[\d.]+Q/);
    expect(report).not.toMatch(/C/);
  });

  it("draws many edges over the tree lightly, and brings out those of the picked card or edge", async () => {
    const { host } = await open();
    const layer = find(host, "g.gedges");
    const hot = (): string[] =>
      [...host.querySelectorAll("[data-edge][data-hot]")].map(
        (one) => one.getAttribute("data-edge") ?? "",
      );
    // Six edges over the tree: more than a handful, so they start quiet.
    expect(layer.hasAttribute("data-quiet")).toBe(true);
    expect(layer.hasAttribute("data-focus")).toBe(false);
    expect(hot()).toEqual([]);
    await tap(node(host, "agent:a2"));
    expect(layer.hasAttribute("data-focus")).toBe(true);
    expect(hot().toSorted()).toEqual(
      [
        "changed:agent:a2:repo:api",
        "dispatched:agent:a1:agent:a2",
        "message:agent:a1:agent:a2",
        "message:agent:a2:agent:a1",
        "report:agent:a2:agent:a1",
      ].toSorted(),
    );
    // The others fade, arrowheads included: not by color alone.
    const marker = (id: string): string =>
      find(host, `[data-edge="${id}"] .gline`).getAttribute("marker-end") ?? "";
    expect(marker("changed:agent:sub3:repo:web")).toMatch(/-changed-faint\)$/);
    expect(marker("changed:agent:a2:repo:api")).toMatch(/-changed\)$/);
    await tap(find(host, '[data-edge="changed:agent:sub3:repo:web"] .ghit'));
    expect(hot()).toEqual(["changed:agent:sub3:repo:web"]);
    expect(marker("changed:agent:a2:repo:api")).toMatch(/-changed-faint\)$/);
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

  it("opens a child's subagent's details with its session's title and status, and no model it lacks", async () => {
    const { host } = await open();
    await tap(node(host, "agent:sub3"));
    expect(facts(host)).toEqual({
      Role: "tester",
      Status: "Running",
      Runner: "mock",
      Cost: "$0.00",
      Tokens: "0 in · 0 out",
      Session: "Web: status badge",
      "Session status": "Running",
    });
    expect(find<HTMLAnchorElement>(details(host), "a.btn").getAttribute("href")).toBe("/s/s3");
  });

  it("opens the user's details: where requests come from, and no relations for the tree line", async () => {
    const { host } = await open();
    await tap(node(host, "user"));
    expect(find(details(host), "h2").textContent).toBe("You");
    expect(details(host).textContent).toContain("You: where this session's requests come from.");
    expect(details(host).querySelector('ul[aria-label="Relations"]')).toBeNull();
    expect(details(host).querySelector("dl")).toBeNull();
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

const wfNode = (
  id: string,
  kind: string,
  ports: Array<[string, boolean]>,
  label = id,
): WorkflowNodeInfo => ({
  id,
  kind,
  label,
  ports: ports.map(([name, failure]) => ({ name, failure })),
});

/** The spec's example (workflows spec, section 2). Edge indices are noted for the tests below. */
const wfGraph: WorkflowGraph = {
  nodes: [
    wfNode("start", "start", [["next", false]], ""),
    wfNode(
      "implement",
      "agent",
      [
        ["done", false],
        ["failed", true],
      ],
      "{{task}}",
    ),
    wfNode(
      "tests",
      "check",
      [
        ["pass", false],
        ["fail", true],
      ],
      "cargo test --workspace",
    ),
    wfNode(
      "retry",
      "loop",
      [
        ["repeat", false],
        ["exhausted", true],
      ],
      "up to 3",
    ),
    wfNode(
      "fix",
      "agent",
      [
        ["done", false],
        ["failed", true],
      ],
      "The checks failed",
    ),
    wfNode("review", "agent", [
      ["approve", false],
      ["changes", false],
      ["failed", true],
    ]),
    wfNode(
      "gate",
      "gate",
      [
        ["approve", false],
        ["reject", true],
      ],
      "Open pull requests?",
    ),
    wfNode(
      "ship",
      "ship",
      [
        ["done", false],
        ["failed", true],
      ],
      "Draft pull requests",
    ),
    wfNode("ok", "end", [], ""),
    wfNode("gave-up", "end", [], ""),
  ],
  edges: [
    { from: "start", port: "next", to: "implement" }, // 0
    { from: "implement", port: "done", to: "tests" }, // 1
    { from: "tests", port: "pass", to: "review" }, // 2
    { from: "tests", port: "fail", to: "retry" }, // 3
    { from: "retry", port: "repeat", to: "fix" }, // 4
    { from: "retry", port: "exhausted", to: "gave-up" }, // 5
    { from: "fix", port: "done", to: "tests" }, // 6
    { from: "review", port: "approve", to: "gate" }, // 7
    { from: "review", port: "changes", to: "retry" }, // 8
    { from: "gate", port: "approve", to: "ship" }, // 9
    { from: "ship", port: "done", to: "ok" }, // 10
  ],
};

const wfStarted = {
  type: "workflow_started",
  workflow_id: "implement",
  name: "Implement, test, review",
  graph: wfGraph,
  budget_usd: 5,
} as const;

/** What `GET /sessions/{id}/workflow` adds: the loop's limit and the tree's cost. */
const served: WorkflowRun = {
  ...startRun(wfStarted),
  cost_usd: 1.2,
  nodes: startRun(wfStarted).nodes.map((one) =>
    one.id === "retry" ? { ...one, max: 3, iteration: 0 } : one,
  ),
};

const nodeStarts = (id: string) =>
  ({ type: "workflow_node", node_id: id, phase: "started", port: null, outcome: null }) as const;
const nodeEnds = (id: string, port: string, outcome: string | null = null) =>
  ({ type: "workflow_node", node_id: id, phase: "finished", port, outcome }) as const;

type WorkflowOpened = Opened & { getWorkflow: ReturnType<typeof vi.fn<Api["getWorkflow"]>> };

async function openWorkflow(
  options: {
    /** What the server answers; a promise for a failure. */
    workflowRun?: WorkflowRun | null | Promise<WorkflowRun | null>;
    session?: Session;
  } = {},
): Promise<WorkflowOpened> {
  const answer = options.workflowRun === undefined ? served : options.workflowRun;
  // A rejection made once is handled by whoever reads it; each call gets its own promise.
  if (answer instanceof Promise) answer.catch(() => {});
  const getWorkflow = vi.fn<Api["getWorkflow"]>(() =>
    answer instanceof Promise ? answer : Promise.resolve(answer),
  );
  const getGraph = vi.fn<Api["getGraph"]>(() => Promise.resolve(makeGraph({ at: T0 })));
  const getTimeline = vi.fn<Api["getTimeline"]>(() => Promise.resolve(makeTimeline()));
  const [sessions, setSessions] = createSignal<readonly Session[]>([]);
  const routes: Partial<Api> = { getGraph, getTimeline, getWorkflow };
  const session = options.session;
  if (session) routes.getSession = () => Promise.resolve(makeDetail(session));
  const workspace = await openWorkspace({ routes, sessions });
  const host = mount(() => (
    <GraphPanel store={workspace.store} active={workspace.active()} links={workspace.links} />
  ));
  await pass();
  return {
    ...workspace,
    host,
    getGraph,
    getTimeline,
    getWorkflow,
    setSessions(next) {
      setSessions(next);
      flush();
    },
  };
}

/** Tests fail, the loop is taken, a fix, the tests fail again, the loop again, a fix, then the tests run. */
async function loopTwice(opened: WorkflowOpened): Promise<void> {
  const { emit, caughtUp } = opened;
  emit(wfStarted, null); // e1
  emit(nodeStarts("start"), null); // e2
  emit(nodeEnds("start", "next"), null); // e3
  emit(nodeStarts("implement"), null); // e4
  emit(nodeEnds("implement", "done", "Added the check."), null); // e5
  emit(nodeStarts("tests"), null); // e6
  emit(nodeEnds("tests", "fail", "cargo test --workspace: 2 failed"), null); // e7
  emit(nodeStarts("retry"), null); // e8
  emit(nodeEnds("retry", "repeat"), null); // e9
  emit(nodeStarts("fix"), null); // e10
  emit(nodeEnds("fix", "done"), null); // e11
  emit(nodeStarts("tests"), null); // e12
  emit(nodeEnds("tests", "fail", "cargo test --workspace: 1 failed"), null); // e13
  emit(nodeStarts("retry"), null); // e14
  emit(nodeEnds("retry", "repeat"), null); // e15
  emit(nodeStarts("fix"), null); // e16
  emit(nodeEnds("fix", "done"), null); // e17
  emit(nodeStarts("tests"), null); // e18
  caughtUp();
  await pass();
}

const wfNodeEl = (host: HTMLElement, id: string): SVGGElement =>
  find<SVGGElement>(host, `g.wnode[data-node="${id}"]`);
const corner = (host: HTMLElement, id: string): string =>
  find(wfNodeEl(host, id), ".wcorner").textContent;
const wfEdge = (host: HTMLElement, index: number): SVGGElement =>
  find<SVGGElement>(host, `g.wedge[data-edge="${index}"]`);
const viewButtons = (host: HTMLElement): Array<[string, string | null]> =>
  [...find(host, '[role="group"][aria-label="View"]').querySelectorAll("button")].map((one) => [
    one.textContent,
    one.getAttribute("aria-pressed"),
  ]);

describe("GraphPanel: the workflow", () => {
  it("offers no Workflow view to a session without a workflow", async () => {
    const { host, getWorkflow } = await openWorkflow({ workflowRun: null });
    expect(viewButtons(host)).toEqual([
      ["Graph", "true"],
      ["Timeline", "false"],
    ]);
    expect(getWorkflow).not.toHaveBeenCalled();
  });

  it("adds the Workflow view once a run starts, and opens on it", async () => {
    const opened = await openWorkflow();
    await loopTwice(opened);
    const { host } = opened;
    expect(viewButtons(host)).toEqual([
      ["Graph", "false"],
      ["Timeline", "false"],
      ["Workflow", "true"],
    ]);
    expect(find(host, "svg.graph-canvas").getAttribute("aria-label")).toBe("Workflow graph");
    // The other views still work, and the reader's choice holds.
    await click(button(host, "Graph"));
    await pass();
    expect(find(host, "svg.graph-canvas").getAttribute("aria-label")).toBe("Session graph");
    await click(button(host, "Timeline"));
    await pass();
    expect(viewButtons(host).find(([label]) => label === "Timeline")?.[1]).toBe("true");
    await click(button(host, "Workflow"));
    await pass();
    expect(host.querySelectorAll("g.wnode")).toHaveLength(wfGraph.nodes.length);
  });

  it("opens on the run of a session whose record names a workflow, from the server alone", async () => {
    const session = makeSession("s1", 1000, { status: "running", workflow: "implement" });
    const { host, getWorkflow } = await openWorkflow({ session });
    await pass();
    expect(getWorkflow).toHaveBeenCalledWith("s1");
    expect(viewButtons(host).at(-1)).toEqual(["Workflow", "true"]);
    expect(find(host, '[data-part="workflow-header"]').textContent).toContain(
      "Implement, test, review",
    );
  });

  it("heads the drawing with the name, the status in words and the budget spent", async () => {
    const opened = await openWorkflow();
    await loopTwice(opened);
    const header = () => find(opened.host, '[data-part="workflow-header"]');
    expect(header().textContent).toContain("Implement, test, review");
    expect(find(header(), "[data-status]").textContent).toBe("Running");
    expect(header().textContent).toContain("spent $1.20 of $5.00");
    opened.emit(nodeEnds("tests", "fail", "1 failed"), null);
    opened.emit(nodeStarts("retry"), null);
    opened.emit(nodeEnds("retry", "exhausted"), null);
    opened.emit(
      { type: "workflow_finished", status: "failed", reason: "retry left through exhausted" },
      null,
    );
    await pass();
    expect(find(header(), "[data-status]").textContent).toBe("Failed");
    expect(header().textContent).toContain("retry left through exhausted");
  });

  it("says a finished run succeeded", async () => {
    const opened = await openWorkflow();
    opened.emit(wfStarted, null);
    opened.emit({ type: "workflow_finished", status: "success", reason: null }, null);
    opened.caughtUp();
    await pass();
    expect(find(opened.host, '[data-part="workflow-header"] [data-status]').textContent).toBe(
      "Succeeded",
    );
  });

  it("lays the run over the graph: each node's state, its last port, its runs, the loop's count", async () => {
    const opened = await openWorkflow();
    await loopTwice(opened);
    const { host } = opened;
    const state = (id: string) => wfNodeEl(host, id).getAttribute("data-state");
    expect(state("tests")).toBe("active");
    expect(corner(host, "tests")).toBe("Running");
    expect(state("gate")).toBe("idle");
    expect(corner(host, "gate")).toBe("");
    expect(state("implement")).toBe("finished");
    expect(corner(host, "implement")).toBe("→ done");
    expect(corner(host, "fix")).toBe("→ done ×2");
    // The limit comes from the server; the count from the events.
    expect(corner(host, "retry")).toBe("2 of 3");
    expect(wfNodeEl(host, "tests").getAttribute("aria-label")).toBe(
      "tests, Check: Running now (run 3)",
    );
    // Left through a failure port: marked, and said with a cross, not only in color.
    opened.emit(nodeEnds("tests", "fail", "boom"), null);
    await pass();
    expect(wfNodeEl(host, "tests").hasAttribute("data-failed")).toBe(true);
    expect(corner(host, "tests")).toBe("✕ fail ×3");
    expect(wfNodeEl(host, "implement").hasAttribute("data-failed")).toBe(false);
  });

  it("emphasizes the edges taken, with a count past one, and marks failure ports by line and label", async () => {
    const opened = await openWorkflow();
    await loopTwice(opened);
    const { host } = opened;
    expect(wfEdge(host, 1).hasAttribute("data-taken")).toBe(true);
    expect(wfEdge(host, 1).querySelector(".wcount")).toBeNull();
    expect(find(wfEdge(host, 3), ".wcount").textContent).toBe("×2");
    expect(find(wfEdge(host, 6), ".wcount").textContent).toBe("×2");
    for (const untaken of [2, 5, 7, 8, 9, 10]) {
      expect(wfEdge(host, untaken).hasAttribute("data-taken")).toBe(false);
    }
    expect(wfEdge(host, 3).hasAttribute("data-failure")).toBe(true);
    expect(find(wfEdge(host, 3), ".wport").textContent).toBe("✕ fail");
    expect(wfEdge(host, 5).hasAttribute("data-failure")).toBe(true);
    expect(wfEdge(host, 2).hasAttribute("data-failure")).toBe(false);
    expect(find(wfEdge(host, 2), ".wport").textContent).toBe("pass");
    // The two edges that close a cycle go around the outside.
    expect(
      [...host.querySelectorAll("g.wedge[data-back]")].map((one) => one.getAttribute("data-edge")),
    ).toEqual(["6", "8"]);
  });

  it("announces the node now running once each time it changes", async () => {
    const opened = await openWorkflow();
    await loopTwice(opened);
    const region = find(opened.host, '[data-part="now-running"]');
    expect(region.getAttribute("aria-live")).toBe("polite");
    expect(region.textContent).toBe("Now running: tests");
    const said: string[] = [];
    const observer = new MutationObserver(() => said.push(region.textContent));
    observer.observe(region, { childList: true, characterData: true, subtree: true });
    // Things that do not change which node runs say nothing.
    opened.emit({ type: "usage", cost_usd: 0.4, tokens_in: 10, tokens_out: 5 });
    opened.emit({ type: "user_message", text: "hurry" }, null);
    await pass(1000);
    expect(said).toEqual([]);
    opened.emit(nodeEnds("tests", "pass"), null);
    opened.emit(nodeStarts("review"), null);
    await pass();
    expect(region.textContent).toBe("Now running: review");
    expect(said.at(-1)).toBe("Now running: review");
    const before = said.length;
    opened.emit({ type: "usage", cost_usd: 0.5, tokens_in: 12, tokens_out: 6 });
    await pass(1000);
    expect(said).toHaveLength(before);
    observer.disconnect();
  });

  it("reads the run again a moment after a node moves, while the view is on screen", async () => {
    const opened = await openWorkflow();
    await loopTwice(opened);
    const calls = opened.getWorkflow.mock.calls.length;
    opened.emit(nodeEnds("tests", "pass"), null);
    await pass(1000);
    expect(opened.getWorkflow.mock.calls.length).toBeGreaterThan(calls);
  });

  it("opens a node's details: kind, label, state, runs, last port, outcome and ports", async () => {
    const opened = await openWorkflow();
    await loopTwice(opened);
    const { host } = opened;
    opened.emit(nodeEnds("tests", "fail", "cargo test --workspace: 1 failed\nat src/lib.rs"), null);
    await pass();
    await tap(wfNodeEl(host, "tests"));
    expect(find(details(host), "h2").textContent).toBe("tests");
    expect(facts(host)).toEqual({
      Kind: "✓ Check",
      Label: "cargo test --workspace",
      State: "Finished through fail, 3 runs",
      Runs: "3",
      "Last port": "fail",
      Outcome: "cargo test --workspace: 1 failed\nat src/lib.rs",
    });
    const ports = [...find(details(host), 'ul[aria-label="Ports"]').querySelectorAll("li")].map(
      (one) => one.textContent,
    );
    expect(ports).toEqual(["pass leads to review", "fail leads to retry"]);
    expect(wfNodeEl(host, "tests").getAttribute("aria-pressed")).toBe("true");
    // A port's target is one tap away.
    await click(button(details(host), "fail leads to retry"));
    await pass();
    expect(find(details(host), "h2").textContent).toBe("retry");
    expect(facts(host).Loop).toBe("2 of 3");
    await click(find(host, 'button[aria-label="Close details"]'));
    await pass();
    expect(host.querySelector('section[aria-label="Details"]')).toBeNull();
  });

  it("shows a node's newest row in the transcript, for the kinds that have rows", async () => {
    const opened = await openWorkflow();
    await loopTwice(opened);
    const { host } = opened;
    await tap(wfNodeEl(host, "tests"));
    await click(button(details(host), "Show in transcript"));
    // e18: the third start of `tests`, its newest row.
    expect(opened.shownItems).toEqual(["workflow:e18"]);
    await tap(wfNodeEl(host, "fix"));
    await click(button(details(host), "Show in transcript"));
    expect(opened.shownItems.at(-1)).toBe("workflow:e16");
    // A loop's and a ship's work leave no row of theirs to show.
    await tap(wfNodeEl(host, "retry"));
    expect(details(host).textContent).not.toContain("Show in transcript");
    await tap(wfNodeEl(host, "ship"));
    expect(details(host).textContent).not.toContain("Show in transcript");
    // Not run yet: no row either.
    await tap(wfNodeEl(host, "gate"));
    expect(details(host).textContent).not.toContain("Show in transcript");
    expect(facts(host).State).toBe("Not run yet");
  });

  it("selects a focused node with Enter, and lets go with Escape", async () => {
    const opened = await openWorkflow();
    await loopTwice(opened);
    const { host } = opened;
    const first = wfNodeEl(host, "start");
    expect(first.getAttribute("tabindex")).toBe("0");
    expect(wfNodeEl(host, "tests").getAttribute("tabindex")).toBe("-1");
    first.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    await pass();
    const implement = wfNodeEl(host, "implement");
    expect(implement.getAttribute("tabindex")).toBe("0");
    implement.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await pass();
    expect(find(details(host), "h2").textContent).toBe("implement");
    implement.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await pass();
    expect(host.querySelector('section[aria-label="Details"]')).toBeNull();
  });

  it("says the whole run in text: every node with its state and ports, and the path taken", async () => {
    const opened = await openWorkflow();
    await loopTwice(opened);
    const text = find(opened.host, '[data-part="workflow-text"]');
    expect(text.classList.contains("sr-only")).toBe(true);
    const items = [...text.querySelectorAll("ol > li")].map((one) => one.textContent);
    expect(items).toHaveLength(wfGraph.nodes.length);
    expect(items[2]).toBe(
      'tests: Check "cargo test --workspace". Running now (run 3). pass leads to review; fail leads to retry.',
    );
    expect(items[3]).toContain("Loop 2 of 3.");
    expect(items[6]).toBe(
      'gate: Gate "Open pull requests?". Not run yet. approve leads to ship; reject ends the run as failed.',
    );
    expect(find(text, "p").textContent).toBe(
      "Path taken: start → implement → tests → retry → fix → tests → retry → fix → tests.",
    );
  });

  it("says why the run could not be read when the events have not told it either", async () => {
    const session = makeSession("s1", 1000, { status: "running", workflow: "implement" });
    const opened = await openWorkflow({
      session,
      workflowRun: Promise.reject(conflict("the workflow log is unreadable")),
    });
    await pass();
    expect(find(opened.host, '[role="alert"]').textContent).toContain(
      "The workflow could not be loaded: the workflow log is unreadable",
    );
    opened.getWorkflow.mockResolvedValue(served);
    await click(button(opened.host, "Try again"));
    await pass();
    expect(find(opened.host, '[data-part="workflow-header"]').textContent).toContain(
      "Implement, test, review",
    );
  });
});
