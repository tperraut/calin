import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { canvasView, describeCanvasMoves, nextFrame } from "../../test/canvas";
import { makeGraph } from "../../test/orchestration";
import { find, mount, unmountAll } from "../../test/render";
import { GraphView, type GraphSelection } from "./GraphView";
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
