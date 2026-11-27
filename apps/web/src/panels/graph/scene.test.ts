import { describe, expect, it, vi } from "vite-plus/test";
import { createScene, LOD_BELOW, PULSE_MS, TWEEN_MS, type SceneLayout } from "./scene";

const SVG = "http://www.w3.org/2000/svg";
const g = (): SVGGElement => document.createElementNS(SVG, "g") as SVGGElement;
const path = (): SVGPathElement => document.createElementNS(SVG, "path") as SVGPathElement;
const circle = (): SVGCircleElement => document.createElementNS(SVG, "circle") as SVGCircleElement;

/** A scene on a clock and a frame queue the test drives. */
function setup(reduced = false) {
  let time = 0;
  let queued: Array<() => void> = [];
  const pulses = vi.fn<(ids: readonly string[]) => void>();
  const scene = createScene({
    reducedMotion: () => reduced,
    onPulses: pulses,
    now: () => time,
    requestFrame: (run) => queued.push(() => run(time)),
    cancelFrame: () => {},
  });
  const frame = (advance: number): void => {
    time += advance;
    const due = queued;
    queued = [];
    for (const run of due) run();
  };
  return { scene, frame, pulses, pending: () => queued.length };
}

const node = (id: string, x: number, y: number, origin?: string) => ({
  id,
  x,
  y,
  w: 100,
  h: 40,
  origin,
});
const layout = (nodes: SceneLayout["nodes"], edges: SceneLayout["edges"] = []): SceneLayout => ({
  mode: "indented",
  nodes,
  edges,
  width: 400,
  height: 400,
});
const treeEdge = { id: "e1", from: "a", to: "b", route: "tree", lane: 0, intoTop: false } as const;

describe("scene", () => {
  it("writes a node's place straight to its element, and runs no frame when nothing moves", () => {
    const { scene, pending } = setup();
    const a = g();
    scene.nodeRef("a", a);
    scene.setLayout(layout([node("a", 10, 20)]), false);
    expect(a.getAttribute("transform")).toBe("translate(10,20)");
    expect(pending()).toBe(0);
  });

  it("tweens what moved to its new place, and flies a newcomer out of its parent", () => {
    const { scene, frame, pending } = setup();
    const a = g();
    const b = g();
    const line = path();
    scene.nodeRef("a", a);
    scene.setLayout(layout([node("a", 0, 0)]), false);
    scene.nodeRef("b", b);
    scene.edgeRef("e1", "path", line);
    scene.setLayout(layout([node("a", 0, 64), node("b", 28, 128, "a")], [treeEdge]), true);
    // Nothing has jumped: the newcomer starts where its parent is.
    expect(a.getAttribute("transform")).toBe("translate(0,0)");
    expect(b.getAttribute("transform")).toBe("translate(0,0)");
    frame(TWEEN_MS / 2);
    const half = scene.position("a")?.y ?? 0;
    expect(half).toBeGreaterThan(0);
    expect(half).toBeLessThan(64);
    expect(line.getAttribute("d")).toMatch(/^M14,/);
    frame(TWEEN_MS);
    expect(a.getAttribute("transform")).toBe("translate(0,64)");
    expect(b.getAttribute("transform")).toBe("translate(28,128)");
    // The line ends on the child's left edge, at its middle.
    expect(line.getAttribute("d")).toMatch(/H28$/);
    expect(scene.busy()).toBe(false);
    expect(pending()).toBe(0);
  });

  it("snaps instead of tweening when motion is to be reduced, and sends no pulse", () => {
    const { scene, pulses, pending } = setup(true);
    const a = g();
    scene.nodeRef("a", a);
    scene.setLayout(layout([node("a", 0, 0), node("b", 0, 64)], [treeEdge]), false);
    scene.setLayout(layout([node("a", 0, 128), node("b", 0, 64)], [treeEdge]), true);
    expect(a.getAttribute("transform")).toBe("translate(0,128)");
    scene.pulse("e1");
    expect(pulses).not.toHaveBeenCalled();
    expect(pending()).toBe(0);
  });

  it("sends a pulse along an edge once, from one end to the other, then removes it", () => {
    const { scene, frame, pulses } = setup();
    scene.setLayout(layout([node("a", 0, 0), node("b", 28, 64)], [treeEdge]), false);
    scene.pulse("e1");
    expect(pulses).toHaveBeenLastCalledWith(["pulse-1"]);
    const dot = circle();
    scene.pulseRef("pulse-1", dot);
    // It starts under the parent, where the line leaves it.
    expect(dot.getAttribute("transform")).toBe("translate(14,40)");
    frame(PULSE_MS / 2);
    const [, x, y] =
      /translate\(([\d.]+),([\d.]+)\)/.exec(dot.getAttribute("transform") ?? "") ?? [];
    expect(Number(x)).toBeGreaterThanOrEqual(14);
    expect(Number(y)).toBeGreaterThan(40);
    frame(PULSE_MS);
    expect(pulses).toHaveBeenLastCalledWith([]);
    expect(scene.busy()).toBe(false);
    // An edge the scene does not hold gets none.
    scene.pulse("nope");
    expect(pulses).toHaveBeenCalledTimes(2);
  });

  it("pans, zooms about a point, fits, and drops the text when zoomed far out", () => {
    const { scene, frame } = setup();
    const view = g();
    scene.mount(view);
    scene.setLayout(layout([node("a", 0, 0), node("b", 300, 360)]), false);
    scene.setViewport({ w: 200, h: 200 });
    frame(16);
    // Home: readable, from the top-left corner, under the floating buttons.
    expect(scene.view()).toEqual({ x: 16, y: 60, k: 0.6 });
    scene.panBy(10, -5);
    scene.zoomAt(100, 100, 2);
    frame(16);
    const zoomed = scene.view();
    expect(zoomed.k).toBeCloseTo(1.2);
    // The point under the cursor did not move.
    expect((100 - zoomed.x) / zoomed.k).toBeCloseTo((100 - 26) / 0.6);
    expect(view.getAttribute("transform")).toBe(
      `translate(${zoomed.x},${zoomed.y}) scale(${zoomed.k})`,
    );
    expect(view.classList.contains("lod")).toBe(false);
    scene.fit();
    frame(16);
    expect(scene.view().k).toBeCloseTo(0.42);
    expect(scene.view().k).toBeLessThan(LOD_BELOW);
    expect(view.classList.contains("lod")).toBe(true);
    for (let turn = 0; turn < 40; turn += 1) scene.zoomBy(0.5);
    expect(scene.view().k).toBe(0.1);
  });

  it("takes what is far outside the viewport out of the drawing, and puts it back when it comes near", () => {
    const { scene, frame } = setup();
    scene.mount(g());
    const near = g();
    const far = g();
    const group = g();
    const line = path();
    group.append(line);
    scene.nodeRef("a", near);
    scene.nodeRef("far", far);
    scene.edgeRef("e9", "path", line);
    const edge = {
      id: "e9",
      from: "far",
      to: "far2",
      route: "tree",
      lane: 0,
      intoTop: false,
    } as const;
    // Before the pane is measured nothing is hidden.
    scene.setLayout(
      layout([node("a", 0, 0), node("far", 0, 3000), node("far2", 28, 3064)], [edge]),
      false,
    );
    expect(far.style.display).toBe("");
    scene.setViewport({ w: 300, h: 300 });
    scene.zoomBy(1 / scene.view().k);
    frame(16);
    expect(near.style.display).toBe("");
    expect(far.style.display).toBe("none");
    expect(group.style.display).toBe("none");
    // The keyboard's tab stop stays in the drawing wherever it is.
    scene.pin("far");
    expect(far.style.display).toBe("");
    scene.pin(undefined);
    expect(far.style.display).toBe("none");
    scene.panBy(0, -2900);
    frame(16);
    expect(far.style.display).toBe("");
    expect(group.style.display).toBe("");
    expect(near.style.display).toBe("none");
    // Focus moving to a hidden node brings it back at once, before any frame.
    scene.reveal("a");
    expect(near.style.display).toBe("");
  });

  it("keeps the anchored node where it is on screen while a layout moves it", () => {
    const { scene, frame } = setup();
    scene.mount(g());
    scene.setLayout(layout([node("a", 0, 0), node("b", 0, 64)]), false);
    scene.setViewport({ w: 400, h: 400 });
    scene.setAnchor("b");
    const before = scene.view();
    const onScreen = 64 * before.k + before.y;
    scene.setLayout(layout([node("a", 0, 0), node("c", 28, 64), node("b", 0, 128)]), true);
    frame(TWEEN_MS / 2);
    frame(TWEEN_MS);
    const after = scene.view();
    expect(128 * after.k + after.y).toBeCloseTo(onScreen);
  });

  it("brings a node into view when focus moves to it", () => {
    const { scene, frame } = setup();
    scene.mount(g());
    scene.setLayout(layout([node("a", 0, 0), node("far", 0, 2000)]), false);
    scene.setViewport({ w: 300, h: 300 });
    scene.zoomBy(1 / scene.view().k);
    frame(16);
    scene.reveal("far");
    const view = scene.view();
    expect(2000 * view.k + view.y + 40 * view.k).toBeLessThanOrEqual(300);
    expect(2000 * view.k + view.y).toBeGreaterThanOrEqual(0);
  });
});
