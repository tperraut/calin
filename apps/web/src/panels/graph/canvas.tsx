import type { JSX } from "@solidjs/web";
import { createEffect, createSignal, createUniqueId, Show, type Accessor } from "solid-js";
import { createMediaQuery } from "../../lib/media";
import type { Size } from "./layout";
import { createScene, type Scene } from "./scene";

// What the node canvases (the session graph and the workflow) share: a scene
// that honors reduced motion, pan and pinch, the wheel, the keys, and the zoom
// buttons over the drawing.

export const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

/** A data attribute that is there or not: the stylesheets match `[data-x]`. */
export const flag = (on: boolean | undefined): "" | undefined => (on ? "" : undefined);

/**
 * The one node of a canvas the Tab key reaches: the one last focused while it
 * is still drawn, else the first in `order`. The scene keeps it in the drawing.
 */
export function createTabStop(
  scene: Pick<Scene, "pin" | "reveal">,
  nodes: () => { order: readonly string[]; has: (id: string) => boolean },
) {
  const [wanted, setWanted] = createSignal<string>();
  const elements = new Map<string, SVGGElement>();
  const stop = (): string | undefined => {
    const id = wanted();
    const { order, has } = nodes();
    return id !== undefined && has(id) ? id : order.at(0);
  };
  createEffect(stop, (id) => scene.pin(id));
  return {
    stop,
    /** A node took the focus. */
    focused: (id: string): void => {
      setWanted(id);
    },
    element: (id: string, element: SVGGElement): void => {
      elements.set(id, element);
    },
    /** Moves the stop and the focus to `to`, brought into view. */
    move: (to: string | undefined): void => {
      if (to === undefined) return;
      setWanted(to);
      scene.pin(to);
      scene.reveal(to);
      elements.get(to)?.focus({ preventScroll: true });
    },
  };
}

/** A scene with no tween and no pulse while the reader asks for reduced motion. */
export function createCalmScene(onPulses: (ids: readonly string[]) => void): Scene {
  const reducedMotion = createMediaQuery(REDUCED_MOTION_QUERY);
  // A plain mirror: the scene asks from effects and frames, where a signal must not be read.
  // The effect below sets it before the scene first asks: the first layout neither tweens nor pulses.
  let calm = false;
  createEffect(reducedMotion, (reduced) => {
    calm = reduced;
  });
  return createScene({ reducedMotion: () => calm, onPulses });
}

/** How far a pointer moves before a press becomes a drag, in pixels. */
const DRAG_SLOP = 4;

export type Point = { x: number; y: number };

/** The pointers down on a canvas, whether the press became a drag, and the fingers' spread during a pinch. */
type Presses = { pointers: Map<number, Point>; dragged: boolean; pinch: number | undefined };

/** How far apart two fingers are. */
export const spread = (a: Point, b: Point): number => Math.hypot(a.x - b.x, a.y - b.y);
/** Where a pointer, a touch or a mouse event is on the page. */
export const at = (event: { clientX: number; clientY: number }): Point => ({
  x: event.clientX,
  y: event.clientY,
});

function pressDown(presses: Presses, event: PointerEvent): void {
  if (event.pointerType === "mouse" && event.button !== 0) return;
  presses.pointers.set(event.pointerId, at(event));
  if (presses.pointers.size === 1) presses.dragged = false;
  if (presses.pointers.size === 2) {
    const [a, b] = [...presses.pointers.values()];
    presses.pinch = spread(a, b);
    presses.dragged = true;
  }
}

/** One pointer moving alone: a pan once past the slop. True when this move starts the drag. */
function panMove(
  presses: Presses,
  event: PointerEvent,
  dx: number,
  dy: number,
): boolean | undefined {
  if (!presses.dragged && Math.hypot(dx, dy) < DRAG_SLOP) return undefined;
  const starts = !presses.dragged;
  presses.dragged = true;
  presses.pointers.set(event.pointerId, at(event));
  return starts;
}

/** Two fingers: the middle of them on screen, and how much their spread grew, when it can be told. */
function pinchMove(presses: Presses): { middle: Point; factor: number | undefined } | undefined {
  const before = presses.pinch;
  if (presses.pointers.size !== 2 || before === undefined) return undefined;
  const [a, b] = [...presses.pointers.values()];
  const now = spread(a, b);
  presses.pinch = now;
  return {
    middle: { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 },
    factor: before > 0 && now > 0 ? now / before : undefined,
  };
}

/** The zoom a wheel step asks for: lines step further than pixels. */
const wheelFactor = (event: WheelEvent): number =>
  Math.exp(-event.deltaY * (event.deltaMode === 1 ? 0.05 : 0.0015));

export type CanvasGestures = {
  /** True while one pointer drags the drawing. */
  dragging: Accessor<boolean>;
  onPointerDown: (event: PointerEvent) => void;
  onPointerMove: (event: PointerEvent) => void;
  onPointerEnd: (event: PointerEvent) => void;
  onWheel: (event: WheelEvent) => void;
  /** True for a click that is a tap, false for the click that ends a drag or a pinch. Call once per click. */
  tapped: () => boolean;
  /** True for a tap on the canvas's own background. Call once per click. */
  tappedBackground: (event: MouseEvent) => boolean;
};

/** What a pointer's move does to the view: a pan, a zoom around a point on screen, and whether a drag starts. */
type Move = { pan: Point; starts: boolean; zoom: { at: Point; factor: number } | undefined };

function pointerMove(presses: Presses, event: PointerEvent): Move | undefined {
  const last = presses.pointers.get(event.pointerId);
  if (!last) return undefined;
  const dx = event.clientX - last.x;
  const dy = event.clientY - last.y;
  if (presses.pointers.size === 1) {
    const starts = panMove(presses, event, dx, dy);
    return starts === undefined ? undefined : { pan: { x: dx, y: dy }, starts, zoom: undefined };
  }
  presses.pointers.set(event.pointerId, at(event));
  const pinch = pinchMove(presses);
  if (!pinch) return undefined;
  const zoom = pinch.factor === undefined ? undefined : { at: pinch.middle, factor: pinch.factor };
  // Half of this pointer's move is the move of the midpoint.
  return { pan: { x: dx / 2, y: dy / 2 }, starts: false, zoom };
}

/** Lets go of a pointer; true when none is left down. */
function release(presses: Presses, id: number): boolean {
  presses.pointers.delete(id);
  if (presses.pointers.size < 2) presses.pinch = undefined;
  return presses.pointers.size === 0;
}

function tap(presses: Presses): boolean {
  const was = presses.dragged;
  presses.dragged = false;
  return !was;
}

/** A point on the page, from the top-left corner of `canvas`. */
function local(canvas: Element | undefined, point: Point): Point {
  const bounds = canvas?.getBoundingClientRect() ?? { left: 0, top: 0 };
  return { x: point.x - bounds.left, y: point.y - bounds.top };
}

/** Pan with one pointer, pinch with two, zoom with the wheel, on `canvas` (the element whose box is the viewport). */
export function createCanvasGestures(
  scene: Pick<Scene, "panBy" | "zoomAt">,
  canvas: () => Element | undefined,
): CanvasGestures {
  const presses: Presses = { pointers: new Map(), dragged: false, pinch: undefined };
  const [dragging, setDragging] = createSignal(false);
  const zoomAt = (point: Point, factor: number): void => {
    const on = local(canvas(), point);
    scene.zoomAt(on.x, on.y, factor);
  };
  return {
    dragging,
    onPointerDown: (event) => pressDown(presses, event),
    onPointerMove(event) {
      const move = pointerMove(presses, event);
      if (!move) return;
      if (move.starts) {
        setDragging(true);
        // From here the drag is the canvas's, wherever the pointer goes.
        canvas()?.setPointerCapture?.(event.pointerId);
      }
      scene.panBy(move.pan.x, move.pan.y);
      if (move.zoom) zoomAt(move.zoom.at, move.zoom.factor);
    },
    onPointerEnd(event) {
      if (release(presses, event.pointerId)) setDragging(false);
    },
    // Not passive (see `observeCanvas`): the wheel zooms the drawing instead of scrolling the page.
    onWheel(event) {
      event.preventDefault();
      zoomAt(at(event), wheelFactor(event));
    },
    tapped: () => tap(presses),
    tappedBackground: (event) => tap(presses) && event.target === event.currentTarget,
  };
}

/**
 * Tells `onSize` the canvas's size now and whenever it changes, once it has
 * one, and sends its wheel to `onWheel`. Returns what stops both and disposes
 * of the scene.
 */
export function observeCanvas(
  element: SVGSVGElement | undefined,
  scene: Pick<Scene, "dispose">,
  onSize: (size: Size) => void,
  onWheel: (event: WheelEvent) => void,
): () => void {
  if (!element) return () => scene.dispose();
  const measure = (): void => {
    const bounds = element.getBoundingClientRect();
    if (bounds.width > 0 && bounds.height > 0) onSize({ w: bounds.width, h: bounds.height });
  };
  measure();
  const observer = typeof ResizeObserver === "function" ? new ResizeObserver(measure) : undefined;
  observer?.observe(element);
  element.addEventListener("wheel", onWheel, { passive: false });
  return () => {
    observer?.disconnect();
    element.removeEventListener("wheel", onWheel);
    scene.dispose();
  };
}

// --- Keys ---------------------------------------------------------------

/** What a key asks of a canvas of nodes. */
export type KeyCommand =
  | { kind: "zoom"; factor: number }
  | { kind: "fit" }
  | { kind: "select"; id: string | undefined }
  | { kind: "focus"; id: string | undefined };

/** The nodes in reading order, and where the side arrows lead from a node, when they do. */
export type KeyNav = {
  order: readonly string[];
  /** True while a node is selected: Escape lets go of it. */
  selected: boolean;
  /** How much one press of + zooms. */
  step: number;
  side: (key: string, at: string) => KeyCommand | undefined;
};

/** The value of `attribute` on the element a key was pressed on, or on the nearest one around it that has it. */
export const markedAt = (target: EventTarget | null, attribute: string): string | undefined =>
  (target instanceof Element ? target.closest(`[${attribute}]`) : null)?.getAttribute(attribute) ??
  undefined;

/** True for a key held with Ctrl, Alt or Meta: those belong to the browser. */
export const withModifier = (event: KeyboardEvent): boolean =>
  event.altKey || event.ctrlKey || event.metaKey;

/** True for the keys that pick what holds the focus. */
export const choosing = (key: string): boolean => key === "Enter" || key === " ";

const zoomIn: ReadonlySet<string> = new Set(["+", "="]);
const zoomOut: ReadonlySet<string> = new Set(["-", "_"]);

/** How much a zoom key zooms by one `step`: + and = in, - and _ out. */
export function zoomKeyFactor(key: string, step: number): number | undefined {
  if (zoomIn.has(key)) return step;
  return zoomOut.has(key) ? 1 / step : undefined;
}

function zoomKey(key: string, step: number): KeyCommand | undefined {
  if (key === "0") return { kind: "fit" };
  const factor = zoomKeyFactor(key, step);
  return factor === undefined ? undefined : { kind: "zoom", factor };
}

function choiceKey(
  key: string,
  node: string | undefined,
  selected: boolean,
): KeyCommand | undefined {
  if (key === "Escape") return selected ? { kind: "select", id: undefined } : undefined;
  if (choosing(key) && node !== undefined) return { kind: "select", id: node };
  return undefined;
}

/** Up and Down step through `order`, Home and End go to its ends; only from a node in it. */
export function orderKey(
  key: string,
  node: string | undefined,
  order: readonly string[],
): KeyCommand | undefined {
  const index = node === undefined ? -1 : order.indexOf(node);
  if (index === -1) return undefined;
  const to = new Map([
    ["ArrowDown", Math.min(order.length - 1, index + 1)],
    ["ArrowUp", Math.max(0, index - 1)],
    ["Home", 0],
    ["End", order.length - 1],
  ]).get(key);
  return to === undefined ? undefined : { kind: "focus", id: order.at(to) };
}

const sideways = new Map([
  ["ArrowRight", "ArrowDown"],
  ["ArrowLeft", "ArrowUp"],
]);

/** Side arrows for a drawing that reads in one direction: Right and Left step through `order` like Down and Up. */
export const alongOrder =
  (order: readonly string[]) =>
  (key: string, node: string): KeyCommand | undefined =>
    orderKey(sideways.get(key) ?? "", node, order);

/** The command of a key pressed on a canvas; undefined leaves the key to the browser. */
function canvasKey(event: KeyboardEvent, nav: KeyNav): KeyCommand | undefined {
  if (withModifier(event)) return undefined;
  const node = markedAt(event.target, "data-node");
  return (
    zoomKey(event.key, nav.step) ??
    choiceKey(event.key, node, nav.selected) ??
    (node === undefined ? undefined : nav.side(event.key, node)) ??
    orderKey(event.key, node, nav.order)
  );
}

/** What a canvas's keys act on. */
export type KeyActions = {
  zoomBy: (factor: number) => void;
  fit: () => void;
  select: (id: string | undefined) => void;
  focus: (id: string | undefined) => void;
};

/** Acts on a key pressed on a canvas and keeps it from the browser; any other key is left alone. */
export function onCanvasKey(event: KeyboardEvent, nav: KeyNav, act: KeyActions): void {
  const command = canvasKey(event, nav);
  if (!command) return;
  if (command.kind === "zoom") act.zoomBy(command.factor);
  else if (command.kind === "fit") act.fit();
  else if (command.kind === "select") act.select(command.id);
  else act.focus(command.id);
  event.preventDefault();
}

// --- Controls -------------------------------------------------------------

/** The zoom buttons and the legend floating over a canvas; `children` are the legend's items. */
export function CanvasControls(props: {
  step: number;
  zoomBy: (factor: number) => void;
  fit: () => void;
  /** The Fit button's title, with its key. */
  fitTitle: string;
  children: JSX.Element;
}) {
  const [legend, setLegend] = createSignal(false);
  const legendId = createUniqueId();
  return (
    <>
      <div class="absolute top-2 right-2 flex gap-1">
        <button
          type="button"
          class="btn bg-surface px-0 text-lg"
          aria-label="Zoom out"
          title="Zoom out (−)"
          onClick={() => props.zoomBy(1 / props.step)}
        >
          <span aria-hidden="true">−</span>
        </button>
        <button
          type="button"
          class="btn bg-surface px-0 text-lg"
          aria-label="Zoom in"
          title="Zoom in (+)"
          onClick={() => props.zoomBy(props.step)}
        >
          <span aria-hidden="true">+</span>
        </button>
        <button
          type="button"
          class="btn bg-surface px-2.5"
          title={props.fitTitle}
          onClick={() => props.fit()}
        >
          Fit
        </button>
        <button
          type="button"
          class="btn bg-surface px-2.5"
          aria-expanded={legend() ? "true" : "false"}
          aria-controls={legendId}
          onClick={() => setLegend((open) => !open)}
        >
          Legend
        </button>
      </div>
      <Show when={legend()}>
        <div
          id={legendId}
          class="absolute top-14 right-2 z-10 max-w-[calc(100%-1rem)] rounded-md border border-line bg-surface p-2 text-xs shadow-md"
        >
          <ul aria-label="Legend" class="flex flex-col gap-1">
            {props.children}
          </ul>
        </div>
      </Show>
    </>
  );
}
