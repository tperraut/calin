import { describe, expect, it, vi } from "vite-plus/test";
import {
  clampZoom,
  contains,
  cornersOf,
  createScene,
  edgeShown,
  fitTo,
  grown,
  homeView,
  labelPoint,
  lanesFrom,
  lerp,
  LOD_BELOW,
  MAX_ZOOM,
  MIN_ZOOM,
  newNode,
  nodeShown,
  overlaps,
  PULSE_MS,
  pulsePoint,
  reach,
  revealShift,
  routeOf,
  TWEEN_MS,
  tweenProgress,
  visibleArea,
  viewTransform,
  zoomAbout,
  type SceneLayout,
} from "./scene";

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
const treeEdge = { id: "e1", from: "a", to: "b", route: "tree" } as const;
/** An edge over the tree, out of `a`'s right side, along the lane at `x`, into `b`'s right side. */
const laneEdge = (x: number) =>
  ({
    id: "o1",
    from: "a",
    to: "b",
    route: "curve",
    points: [
      { x: 100, xn: "a", y: 20, yn: "a" },
      { x, y: 20, yn: "a" },
      { x, y: 20, yn: "b" },
      { x: 100, xn: "b", y: 20, yn: "b" },
    ],
    label: { distance: 10 },
  }) as const;

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

  it("draws an edge over the tree along its lane, and moves it to a new lane over the tween", () => {
    const { scene, frame } = setup();
    const line = path();
    const count = g();
    scene.edgeRef("o1", "path", line);
    scene.edgeRef("o1", "label", count);
    scene.setLayout(layout([node("a", 0, 0), node("b", 0, 64)], [laneEdge(130)]), false);
    expect(line.getAttribute("d")).toBe("M100,20L124,20Q130,20,130,26L130,78Q130,84,124,84L100,84");
    // The count sits on the line, a set distance from where it leaves its card.
    expect(count.getAttribute("transform")).toBe("translate(110,20)");
    // The lanes were packed again: the line slides over, it does not jump.
    scene.setLayout(layout([node("a", 0, 0), node("b", 0, 64)], [laneEdge(110)]), true);
    expect(line.getAttribute("d")).toMatch(/^M100,20L124,20Q130,20/);
    frame(TWEEN_MS / 2);
    const middle = Number(/L([\d.]+),20Q/.exec(line.getAttribute("d") ?? "")?.[1]);
    expect(middle).toBeGreaterThan(104);
    expect(middle).toBeLessThan(124);
    frame(TWEEN_MS);
    expect(line.getAttribute("d")).toBe("M100,20L105,20Q110,20,110,25L110,79Q110,84,105,84L100,84");
    expect(scene.busy()).toBe(false);
  });

  it("follows its cards with the corners tied to them, and snaps when motion is to be reduced", () => {
    const moving = setup();
    const line = path();
    moving.scene.edgeRef("o1", "path", line);
    moving.scene.setLayout(layout([node("a", 0, 0), node("b", 0, 64)], [laneEdge(130)]), false);
    moving.scene.setLayout(layout([node("a", 0, 0), node("b", 0, 128)], [laneEdge(130)]), true);
    moving.frame(TWEEN_MS / 2);
    const end = Number(/L100,([\d.]+)$/.exec(line.getAttribute("d") ?? "")?.[1]);
    expect(end).toBeGreaterThan(84);
    expect(end).toBeLessThan(148);
    expect(moving.scene.position("b")?.y).toBeCloseTo(end - 20);

    const calm = setup(true);
    const still = path();
    calm.scene.edgeRef("o1", "path", still);
    calm.scene.setLayout(layout([node("a", 0, 0), node("b", 0, 64)], [laneEdge(130)]), false);
    calm.scene.setLayout(layout([node("a", 0, 0), node("b", 0, 128)], [laneEdge(110)]), true);
    expect(still.getAttribute("d")).toBe(
      "M100,20L105,20Q110,20,110,25L110,143Q110,148,105,148L100,148",
    );
    expect(calm.pending()).toBe(0);
  });

  it("sends a pulse along an edge's lanes, corner by corner", () => {
    const { scene, frame } = setup();
    scene.setLayout(layout([node("a", 0, 0), node("b", 0, 64)], [laneEdge(130)]), false);
    scene.pulse("o1");
    const dot = circle();
    scene.pulseRef("pulse-1", dot);
    expect(dot.getAttribute("transform")).toBe("translate(100,20)");
    // Halfway along: down the lane, level between the two cards.
    frame(PULSE_MS / 2);
    expect(dot.getAttribute("transform")).toBe("translate(130,52)");
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

  it("brings a node in from the right and from above, and leaves one already in view alone", () => {
    const { scene } = setup();
    const nodes = [node("a", 0, 0), node("right", 500, 0), node("mid", 100, 100)];
    // Narrow enough to be homed at full size.
    scene.setLayout({ ...layout(nodes), width: 200 }, false);
    scene.setViewport({ w: 300, h: 300 });
    expect(scene.view()).toEqual({ x: 16, y: 60, k: 1 });
    // Its right edge lands 16 px inside the viewport's right edge.
    scene.reveal("right");
    expect(scene.view()).toEqual({ x: -316, y: 60, k: 1 });
    scene.panBy(332, 0);
    // Its top edge lands 16 px under the viewport's top edge.
    scene.panBy(0, -200);
    scene.reveal("a");
    expect(scene.view()).toEqual({ x: 16, y: 16, k: 1 });
    scene.reveal("mid");
    expect(scene.view()).toEqual({ x: 16, y: 16, k: 1 });
    // A node the scene does not hold moves nothing.
    scene.reveal("nope");
    expect(scene.view()).toEqual({ x: 16, y: 16, k: 1 });
  });

  it("fits the whole drawing, legibly under the floating buttons when asked, and not before the pane is measured", () => {
    const { scene } = setup();
    scene.setLayout(layout([node("a", 0, 0), node("b", 300, 360)]), false);
    scene.fit();
    expect(scene.view()).toEqual({ x: 0, y: 0, k: 1 });
    scene.setViewport({ w: 200, h: 200 });
    scene.fit(true);
    expect(scene.view().k).toBe(0.5);
    expect(scene.view().y).toBe(60);
    // A drawing that fits is centred, but never under the buttons.
    scene.setViewport({ w: 2000, h: 480 });
    scene.fit(true);
    expect(scene.view()).toEqual({ x: 800, y: 60, k: 1 });
    scene.fit();
    expect(scene.view()).toEqual({ x: 800, y: 40, k: 1 });
  });

  it("homes again when the pane changes size, until the reader pans or zooms", () => {
    const { scene } = setup();
    scene.setLayout({ ...layout([node("a", 0, 0)]), width: 1000, homeWidth: 500 }, false);
    // Not measured yet: nothing to home on.
    expect(scene.view()).toEqual({ x: 0, y: 0, k: 1 });
    scene.setViewport({ w: 432, h: 300 });
    // Readable across the home width, not the whole drawing.
    expect(scene.view()).toEqual({ x: 16, y: 60, k: 0.8 });
    scene.setViewport({ w: 282, h: 300 });
    expect(scene.view()).toEqual({ x: 16, y: 60, k: 0.6 });
    scene.panBy(5, 5);
    scene.setViewport({ w: 532, h: 300 });
    expect(scene.view()).toEqual({ x: 21, y: 65, k: 0.6 });
  });

  it("places a node's element that arrives after its layout, hidden when it is far outside", () => {
    const { scene } = setup();
    scene.setLayout(layout([node("a", 0, 0), node("far", 0, 3000)]), false);
    scene.setViewport({ w: 300, h: 300 });
    const near = g();
    const far = g();
    scene.nodeRef("a", near);
    scene.nodeRef("far", far);
    expect(near.getAttribute("transform")).toBe("translate(0,0)");
    expect(near.style.display).toBe("");
    expect(far.getAttribute("transform")).toBe("translate(0,3000)");
    expect(far.style.display).toBe("none");
  });

  it("forgets the element of a node or an edge that left before a layout placed it", () => {
    const { scene } = setup();
    const gone = g();
    const goneLine = path();
    const kept = path();
    scene.nodeRef("x", gone);
    scene.edgeRef("ex", "path", goneLine);
    scene.edgeRef("e1", "path", kept);
    scene.setLayout(layout([node("a", 0, 0), node("b", 28, 64)], [treeEdge]), false);
    scene.setLayout(
      layout(
        [node("a", 0, 0), node("b", 28, 64), node("x", 0, 128)],
        [treeEdge, { id: "ex", from: "a", to: "x", route: "tree" }],
      ),
      false,
    );
    expect(kept.getAttribute("d")).toMatch(/H28$/);
    expect(gone.getAttribute("transform")).toBeNull();
    expect(goneLine.getAttribute("d")).toBeNull();
  });

  it("draws a curve with no corners straight between the right sides of its cards, its count halfway", () => {
    const { scene } = setup();
    const line = path();
    const count = g();
    scene.edgeRef("c", "path", line);
    scene.edgeRef("c", "label", count);
    scene.setLayout(
      layout(
        [node("a", 0, 0), node("b", 0, 64)],
        [{ id: "c", from: "a", to: "b", route: "curve" }],
      ),
      false,
    );
    expect(line.getAttribute("d")).toBe("M100,20L100,84");
    expect(count.getAttribute("transform")).toBe("translate(100,52)");
  });

  it("puts a curve's count at a fraction of its length", () => {
    const { scene } = setup();
    const count = g();
    scene.edgeRef("o1", "label", count);
    scene.setLayout(
      layout([node("a", 0, 0), node("b", 0, 64)], [{ ...laneEdge(130), label: { fraction: 0.5 } }]),
      false,
    );
    expect(count.getAttribute("transform")).toBe("translate(130,52)");
  });

  it("draws nothing for an edge to a node it does not hold, and its pulse ends at once", () => {
    const { scene, frame, pulses } = setup();
    const line = path();
    scene.edgeRef("e1", "path", line);
    scene.setLayout(layout([node("a", 0, 0)], [treeEdge]), false);
    expect(line.getAttribute("d")).toBeNull();
    scene.pulse("e1");
    const dot = circle();
    scene.pulseRef("pulse-1", dot);
    expect(dot.getAttribute("transform")).toBeNull();
    // A pulse that already ended takes no element.
    frame(16);
    expect(pulses).toHaveBeenLastCalledWith([]);
    scene.pulseRef("pulse-1", circle());
    expect(scene.busy()).toBe(false);
    expect(scene.position("nope")).toBeUndefined();
  });

  it("runs one frame at a time, and nothing once disposed", () => {
    const { scene, frame, pending } = setup();
    const a = g();
    scene.nodeRef("a", a);
    scene.setLayout(layout([node("a", 0, 0), node("b", 0, 64)], [treeEdge]), false);
    scene.setLayout(layout([node("a", 0, 64), node("b", 0, 64)], [treeEdge]), true);
    scene.pulse("e1");
    scene.panBy(1, 1);
    expect(pending()).toBe(1);
    scene.dispose();
    frame(TWEEN_MS);
    expect(a.getAttribute("transform")).toBe("translate(0,0)");
    scene.panBy(1, 1);
    expect(pending()).toBe(0);
  });

  it("cancels the frame it asked for when disposed", () => {
    const cancel = vi.fn<(handle: number) => void>();
    const scene = createScene({
      reducedMotion: () => false,
      onPulses: () => {},
      requestFrame: () => 7,
      cancelFrame: cancel,
    });
    scene.dispose();
    expect(cancel).not.toHaveBeenCalled();
    scene.panBy(1, 1);
    expect(cancel).not.toHaveBeenCalled();
    const live = createScene({
      reducedMotion: () => false,
      onPulses: () => {},
      requestFrame: () => 7,
      cancelFrame: cancel,
    });
    live.panBy(1, 1);
    live.dispose();
    expect(cancel).toHaveBeenCalledWith(7);
  });
});

describe("scene arithmetic", () => {
  const card = { x: 0, y: 0, w: 100, h: 40 };
  const line = {
    kind: "poly",
    points: [
      { x: 0, y: 0 },
      { x: 100, y: 0 },
    ],
  } as const;

  it("tweens from one value to another, clamped to the tween's length", () => {
    expect(lerp(10, 20, 0.25)).toBe(12.5);
    expect(tweenProgress(TWEEN_MS / 2 + 100, 100)).toBe(0.5);
    expect(tweenProgress(TWEEN_MS * 3, 0)).toBe(1);
  });

  it("tells areas that contain or overlap one another, edges included or not", () => {
    const area = { x0: 0, y0: 0, x1: 10, y1: 10 };
    expect(contains(area, area)).toBe(true);
    expect(contains(area, { ...area, x1: 11 })).toBe(false);
    expect(contains(area, { ...area, y0: -1 })).toBe(false);
    expect(overlaps(area, { x0: 9, y0: 9, x1: 20, y1: 20 })).toBe(true);
    // Touching is not overlapping.
    expect(overlaps(area, { x0: 10, y0: 0, x1: 20, y1: 10 })).toBe(false);
    expect(overlaps(area, { x0: 0, y0: 10, x1: 10, y1: 20 })).toBe(false);
  });

  it("finds what the viewport shows, and grows it by half on every side", () => {
    expect(visibleArea({ x: -50, y: 20, k: 2 }, { w: 300, h: 200 })).toEqual({
      x0: 25,
      y0: -10,
      x1: 175,
      y1: 90,
    });
    expect(grown({ x0: 0, y0: 0, x1: 100, y1: 40 })).toEqual({ x0: -50, y0: -20, x1: 150, y1: 60 });
  });

  it("keeps a node in the drawing where it is, where it goes, or when it holds the tab stop", () => {
    const flying = newNode({ id: "a", ...card }, { x: 500, y: 500 }, undefined);
    const far = { x0: 1000, y0: 1000, x1: 1100, y1: 1100 };
    expect(nodeShown(flying, far, undefined)).toBe(false);
    expect(nodeShown(flying, far, "a")).toBe(true);
    expect(nodeShown(flying, { x0: 550, y0: 520, x1: 600, y1: 600 }, undefined)).toBe(true);
    expect(nodeShown(flying, { x0: 50, y0: 20, x1: 60, y1: 30 }, undefined)).toBe(true);
  });

  it("keeps an edge in the drawing until it is drawn, while it moves, or when it reaches in", () => {
    const route = { kind: "elbow", x0: 0, y0: 0, x1: 10, y1: 10 } as const;
    const far = { x0: 1000, y0: 1000, x1: 1100, y1: 1100 };
    expect(edgeShown({ current: undefined, moving: false }, far)).toBe(true);
    expect(edgeShown({ current: route, moving: true }, far)).toBe(true);
    expect(edgeShown({ current: route, moving: false }, far)).toBe(false);
    expect(edgeShown({ current: route, moving: false }, { x0: 5, y0: 5, x1: 6, y1: 6 })).toBe(true);
  });

  it("bounds a route by its ends, its corners and its control points", () => {
    const corners = [
      { x: 0, y: 5 },
      { x: 10, y: -5 },
      { x: 4, y: 20 },
    ];
    expect(reach({ kind: "poly", points: corners })).toEqual({ x0: 0, y0: -5, x1: 10, y1: 20 });
    expect(reach({ kind: "elbow", x0: 5, y0: 0, x1: 0, y1: 10 })).toEqual({
      x0: 0,
      y0: 0,
      x1: 5,
      y1: 10,
    });
    const cubic = {
      kind: "cubic",
      x0: 0,
      y0: 0,
      c1x: -5,
      c1y: 3,
      c2x: 20,
      c2y: -2,
      x1: 10,
      y1: 10,
    };
    expect(reach({ ...cubic, kind: "cubic" })).toEqual({ x0: -5, y0: -2, x1: 20, y1: 10 });
  });

  it("moves a curve's free corners over the tween while the tied ones follow their card", () => {
    const below = { ...card, y: 64 };
    const points = [
      { x: 100, xn: "a", y: 20, yn: "a" },
      { x: 130, y: 20, yn: "a" },
      { x: 130, y: 50 },
    ];
    const at = (id: string) => (id === "a" ? { x: 0, y: 10 } : undefined);
    const start = [
      { x: 100, y: 20 },
      { x: 110, y: 20 },
      { x: 110, y: 60 },
    ];
    const straight = [
      { x: 100, y: 20 },
      { x: 100, y: 84 },
    ];
    expect(cornersOf({}, card, below, at, 0.5)).toEqual(straight);
    expect(cornersOf({ points: points.slice(0, 1) }, card, below, at, 0.5)).toEqual(straight);
    expect(cornersOf({ points, start }, card, below, at, 0.5)).toEqual([
      { x: 100, y: 30 },
      { x: 120, y: 30 },
      { x: 120, y: 55 },
    ]);
    const target = [
      { x: 100, y: 30 },
      { x: 130, y: 30 },
      { x: 130, y: 50 },
    ];
    expect(cornersOf({ points, start }, card, below, at, 1)).toEqual(target);
    expect(cornersOf({ points }, card, below, at, 0.5)).toEqual(target);
    expect(cornersOf({ points, start: start.slice(0, 1) }, card, below, at, 0.5)).toEqual(target);
  });

  it("routes an edge between the cards it joins, and none when one is missing", () => {
    const boxes = new Map([
      ["a", card],
      ["b", { ...card, y: 64 }],
    ]);
    expect(routeOf({ from: "a", to: "x", route: "tree" }, boxes, "indented", 1)).toBeUndefined();
    expect(routeOf({ from: "x", to: "b", route: "tree" }, boxes, "indented", 1)).toBeUndefined();
    expect(routeOf({ from: "a", to: "b", route: "tree" }, boxes, "indented", 1)?.kind).toBe(
      "elbow",
    );
    expect(routeOf({ from: "a", to: "b", route: "curve" }, boxes, "indented", 1)).toEqual({
      kind: "poly",
      points: [
        { x: 100, y: 20 },
        { x: 100, y: 84 },
      ],
    });
  });

  it("puts a count halfway, at a distance or at a fraction of the line", () => {
    expect(labelPoint(undefined, line)).toEqual({ x: 50, y: 0 });
    expect(labelPoint({ distance: 10 }, line)).toEqual({ x: 10, y: 0 });
    expect(labelPoint({ fraction: 0.25 }, line)).toEqual({ x: 25, y: 0 });
  });

  it("starts a curve from where it is drawn only when its lanes move", () => {
    const points = [
      { x: 100, xn: "a", y: 20, yn: "a" },
      { x: 130, y: 20 },
    ];
    const targets = (id: string) => (id === "a" ? { x: 0, y: 0 } : undefined);
    const still = [
      { x: 100, y: 20 },
      { x: 130, y: 20 },
    ];
    const moved = [
      { x: 100, y: 20 },
      { x: 110, y: 20 },
    ];
    expect(lanesFrom(points, still, targets)).toBeUndefined();
    expect(lanesFrom(points, moved, targets)).toBe(moved);
    expect(lanesFrom(points, [{ x: 100, y: 21 }, ...still.slice(1)], targets)).toEqual([
      { x: 100, y: 21 },
      { x: 130, y: 20 },
    ]);
    expect(lanesFrom(points, still.slice(0, 1), targets)).toEqual(still.slice(0, 1));
    expect(lanesFrom(points, undefined, targets)).toBeUndefined();
    expect(lanesFrom(undefined, still, targets)).toBeUndefined();
  });

  it("places a pulse along its route until it ends", () => {
    expect(pulsePoint(line, 0.25)).toEqual({ x: 25, y: 0 });
    expect(pulsePoint(line, 1)).toBeUndefined();
    expect(pulsePoint(undefined, 0.25)).toBeUndefined();
  });

  it("clamps the zoom and writes the view as a transform", () => {
    expect(clampZoom(100)).toBe(MAX_ZOOM);
    expect(clampZoom(0)).toBe(MIN_ZOOM);
    expect(clampZoom(1)).toBe(1);
    expect(viewTransform({ x: 1, y: 2, k: 0.5 })).toBe("translate(1,2) scale(0.5)");
  });

  it("homes readable across the home width, once the pane and the drawing have a size", () => {
    const pane = { w: 432, h: 300 };
    expect(homeView({ w: 0, h: 300 }, { w: 1000, h: 400 }, 0)).toBeUndefined();
    expect(homeView({ w: 432, h: 0 }, { w: 1000, h: 400 }, 0)).toBeUndefined();
    expect(homeView(pane, { w: 0, h: 400 }, 0)).toBeUndefined();
    expect(homeView(pane, { w: 1000, h: 400 }, 500)).toEqual({ x: 16, y: 60, k: 0.8 });
    expect(homeView(pane, { w: 1000, h: 400 }, 0)).toEqual({ x: 16, y: 60, k: 0.6 });
    expect(homeView(pane, { w: 100, h: 400 }, 0)).toEqual({ x: 16, y: 60, k: 1 });
    expect(homeView(pane, { w: 100, h: 400 }, 500)).toEqual({ x: 16, y: 60, k: 1 });
  });

  it("fits the drawing, legibly under the buttons when asked", () => {
    const fitted = fitTo({ w: 400, h: 400 }, { w: 200, h: 200 }, false);
    expect(fitted.k).toBeCloseTo(0.42);
    expect(fitted.y).toBeCloseTo(16);
    expect(fitTo({ w: 400, h: 400 }, { w: 200, h: 200 }, true)).toEqual({ x: 16, y: 60, k: 0.5 });
  });

  it("zooms about a point, within the zoom's bounds", () => {
    expect(zoomAbout({ x: 0, y: 0, k: 1 }, 100, 50, 2)).toEqual({ x: -100, y: -50, k: 2 });
    expect(zoomAbout({ x: 0, y: 0, k: 1 }, 100, 50, 10)).toEqual({ x: -150, y: -75, k: MAX_ZOOM });
  });

  it("pans a box just inside the viewport's padding, on each side", () => {
    const pane = { w: 300, h: 300 };
    expect(revealShift(card, { x: 16, y: 16, k: 1 }, pane)).toEqual({ dx: 0, dy: 0 });
    expect(revealShift(card, { x: -50, y: 0, k: 1 }, pane)).toEqual({ dx: 66, dy: 16 });
    expect(revealShift(card, { x: 250, y: 280, k: 1 }, pane)).toEqual({ dx: -66, dy: -36 });
    expect(revealShift(card, { x: 100, y: 100, k: 2 }, pane)).toEqual({ dx: -16, dy: 0 });
  });

  it("starts a node where its origin is, or at its place when it has none", () => {
    const element = g();
    const input = { id: "a", x: 10, y: 20, w: 100, h: 40 };
    expect(newNode(input, { x: 5, y: 6 }, element)).toEqual({
      ...input,
      x: 5,
      y: 6,
      fx: 5,
      fy: 6,
      tx: 10,
      ty: 20,
      moving: true,
      shown: true,
      el: element,
    });
    expect(newNode(input, undefined, undefined)).toEqual({
      ...input,
      fx: 10,
      fy: 20,
      tx: 10,
      ty: 20,
      moving: false,
      shown: true,
      el: undefined,
    });
  });
});
