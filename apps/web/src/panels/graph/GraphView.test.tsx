import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { canvasView, describeCanvasMoves, nextFrame } from "../../test/canvas";
import type { Graph } from "../../gen/Graph";
import { makeEdge, makeGraph } from "../../test/orchestration";
import { find, mount, unmountAll } from "../../test/render";
import { GraphView, type GraphSelection } from "./GraphView";
import { NODE_SIZE } from "./layout";
import type { Scene } from "./scene";

afterEach(unmountAll);

describeCanvasMoves("GraphView", () => {
  const [selection, setSelection] = createSignal<GraphSelection>();
  const host = mount(() => (
    <GraphView
      graph={makeGraph()}
      openSessionId="s1"
      mode="columns"
      selection={selection()}
      onSelect={setSelection}
    />
  ));
  return Promise.resolve({
    host,
    canvas: find<SVGSVGElement>(host, 'svg[aria-label="Session graph"]'),
    node: (id: string) => find<SVGGElement>(host, `g.gnode[data-node="${id}"]`),
    selected: () => selection()?.id,
    sample: "agent:a1",
    step: 1.3,
  });
});

describe("GraphView: its scene", () => {
  it("hands its scene out once on screen, the one that pans and zooms the drawing", async () => {
    const onScene = vi.fn<(scene: Scene) => void>();
    const host = mount(() => (
      <GraphView
        graph={makeGraph()}
        openSessionId="s1"
        mode="columns"
        selection={undefined}
        onSelect={() => {}}
        onScene={onScene}
      />
    ));
    expect(onScene).toHaveBeenCalledTimes(1);
    onScene.mock.calls[0]?.[0].panBy(25, 5);
    await nextFrame();
    expect(canvasView(find<SVGSVGElement>(host, "svg.graph-canvas"))).toEqual({
      x: 25,
      y: 5,
      k: 1,
    });
  });
});

describe("GraphView: the tab stop", () => {
  it("goes back to the first node when the focused one leaves the graph", () => {
    const whole = makeGraph();
    const [graph, setGraph] = createSignal(whole);
    const host = mount(() => (
      <GraphView
        graph={graph()}
        openSessionId="s1"
        mode="columns"
        selection={undefined}
        onSelect={() => {}}
      />
    ));
    const stop = () => host.querySelector('g.gnode[tabindex="0"]')?.getAttribute("data-node");
    const first = stop();
    find(host, 'g.gnode[data-node="agent:sub1"]').dispatchEvent(new FocusEvent("focus"));
    flush();
    expect(stop()).toBe("agent:sub1");
    setGraph({
      ...whole,
      nodes: whole.nodes.filter((node) => node.id !== "agent:sub1"),
      edges: whole.edges.filter((edge) => edge.from !== "agent:sub1" && edge.to !== "agent:sub1"),
    });
    flush();
    expect(stop()).toBe(first);
  });
});

describe("GraphView: the drawing", () => {
  const draw = (graph: Graph = makeGraph()) =>
    mount(() => (
      <GraphView
        graph={graph}
        openSessionId="s1"
        mode="columns"
        selection={undefined}
        onSelect={() => {}}
      />
    ));
  const size = (element: Element) => [
    element.getAttribute("width"),
    element.getAttribute("height"),
  ];
  const { user: USER, agent: AGENT, repo: REPO } = NODE_SIZE;

  it("rings each kind of card for the focus, and puts an agent's cost at its right edge", () => {
    const host = draw();
    const user = find(host, 'g.gnode[data-node="user"]');
    expect(size(find(user, "rect.gfocus"))).toEqual([String(USER.w + 8), String(USER.h + 8)]);
    expect(find(user, "rect.gshape").getAttribute("rx")).toBe(String(USER.h / 2));
    const repo = find(host, 'g.gnode[data-kind="repo"]');
    expect(size(find(repo, "rect.gfocus"))).toEqual([String(REPO.w + 8), String(REPO.h + 6)]);
    const agent = find(host, 'g.gnode[data-node="agent:a1"]');
    expect(size(find(agent, "rect.gfocus"))).toEqual([String(AGENT.w + 10), String(AGENT.h + 10)]);
    expect(size(find(agent, "rect.gown"))).toEqual([String(AGENT.w + 6), String(AGENT.h + 6)]);
    expect(find(agent, "text.gcost").getAttribute("x")).toBe(String(AGENT.w - 10));
  });

  it("leaves every card but the tab stop out of the tab order", () => {
    const host = draw();
    const stops = [...host.querySelectorAll("g.gnode")].map((one) => one.getAttribute("tabindex"));
    expect(stops.filter((one) => one === "0")).toHaveLength(1);
    expect(stops.filter((one) => one === "-1")).toHaveLength(stops.length - 1);
  });

  it("caps an edge's count at 99+", () => {
    const host = draw(
      makeGraph({
        edges: [
          makeEdge("message", "agent:a1", "agent:a2", { count: 100 }),
          makeEdge("report", "agent:sub1", "agent:a1", { count: 99 }),
        ],
      }),
    );
    expect([...host.querySelectorAll("g.gcount text")].map((one) => one.textContent)).toEqual([
      "99+",
      "99",
    ]);
  });

  it("sizes and fades each arrowhead by its tone", () => {
    const host = draw();
    const marker = (suffix: string) => {
      const found = [...host.querySelectorAll("marker")].find((one) => one.id.endsWith(suffix));
      if (!found) throw new Error(`no marker ${suffix}`);
      return [
        found.getAttribute("markerWidth"),
        found.querySelector("path")?.getAttribute("opacity") ?? null,
      ];
    };
    expect(marker("-dispatched")).toEqual(["9", null]);
    expect(marker("-dispatched-quiet")).toEqual([String(9 * 0.8), "0.6"]);
    expect(marker("-dispatched-faint")).toEqual([String(9 * 0.8), "0.22"]);
  });

  it("draws the curves lightly only when there are more than five", () => {
    const pairs: Array<[string, string]> = [
      ["agent:a1", "agent:sub1"],
      ["agent:a1", "agent:sub2"],
      ["agent:a1", "agent:a2"],
      ["agent:a1", "agent:a3"],
      ["agent:sub1", "agent:sub2"],
      ["agent:sub1", "agent:a2"],
    ];
    const curves = (count: number) =>
      makeGraph({
        edges: pairs.slice(0, count).map(([from, to]) => makeEdge("message", from, to)),
      });
    expect(find(draw(curves(5)), "g.gedges").hasAttribute("data-quiet")).toBe(false);
    unmountAll();
    expect(find(draw(curves(6)), "g.gedges").hasAttribute("data-quiet")).toBe(true);
  });

  it("says in text only the sections there is something for", () => {
    const whole = find(draw(), '[data-part="graph-text"]').textContent;
    expect(whole).toContain("Repos");
    expect(whole).toContain("Relations");
    unmountAll();
    const bare = makeGraph({
      nodes: makeGraph().nodes.filter((node) => node.kind !== "repo"),
      edges: [],
    });
    const text = find(draw(bare), '[data-part="graph-text"]').textContent;
    expect(text).not.toContain("Repos");
    expect(text).not.toContain("Relations");
  });
});
