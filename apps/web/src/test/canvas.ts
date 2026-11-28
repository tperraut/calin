import { flush } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { find } from "./render";

type At = readonly [number, number];

/** Sends one pointer event at (`x`, `y`) on the page: a main mouse button unless `init` says otherwise. */
export function pointer(
  target: Element,
  type: "pointerdown" | "pointermove" | "pointerup" | "pointercancel",
  id: number,
  [x, y]: At,
  init: PointerEventInit = {},
): void {
  target.dispatchEvent(
    new PointerEvent(type, {
      pointerId: id,
      pointerType: "mouse",
      button: 0,
      clientX: x,
      clientY: y,
      bubbles: true,
      ...init,
    }),
  );
}

/** Presses a pointer at `from` and moves it to `to`, without letting go. */
export function drag(
  target: Element,
  id: number,
  from: At,
  to: At,
  init: PointerEventInit = {},
): void {
  pointer(target, "pointerdown", id, from, init);
  pointer(target, "pointermove", id, to, init);
}

/** Presses `key` on `target`; true when the page kept the key from the browser. */
export function press(target: Element, key: string, init: KeyboardEventInit = {}): boolean {
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  flush();
  return event.defaultPrevented;
}

/** Waits for the next animation frame: the scene writes pan and zoom then. */
export const nextFrame = (): Promise<void> =>
  new Promise((resolve) => requestAnimationFrame(() => resolve()));

type View = { x: number; y: number; k: number };

/** Pan and zoom of a canvas, as the scene last wrote them on its drawing; none written is the identity. */
export function canvasView(canvas: SVGSVGElement): View {
  const drawing = [...canvas.children].find((child) => child.tagName.toLowerCase() === "g");
  const match = /translate\(([^,]+),([^)]+)\) scale\(([^)]+)\)/.exec(
    drawing?.getAttribute("transform") ?? "",
  );
  if (!match) return { x: 0, y: 0, k: 1 };
  return { x: Number(match[1]), y: Number(match[2]), k: Number(match[3]) };
}

/** A pannable, zoomable canvas of nodes on screen, as `describeCanvasMoves` drives it. */
export type CanvasUnderTest = {
  host: HTMLElement;
  canvas: SVGSVGElement;
  /** The element of a node, by id. */
  node: (id: string) => SVGGElement;
  /** The id of the node selected now, if any. */
  selected: () => string | undefined;
  /** A node to click. */
  sample: string;
  /** How much one press of + zooms. */
  step: number;
};

/** The pan, zoom and tap behaviour every node canvas shares (the session graph, the workflow). */
export function describeCanvasMoves(name: string, setup: () => Promise<CanvasUnderTest>): void {
  describe(`${name}: moving around`, () => {
    beforeEach(() => {
      // A canvas of 400 × 300 at the top-left of the page: happy-dom lays nothing out.
      vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue(
        DOMRect.fromRect({ x: 0, y: 0, width: 400, height: 300 }),
      );
    });

    async function open() {
      const shown = await setup();
      await nextFrame();
      return { ...shown, view: () => canvasView(shown.canvas) };
    }

    it("pans with a drag past a few pixels, and a click that ends a drag selects nothing", async () => {
      const { canvas, view, selected, node, sample } = await open();
      const before = view();
      drag(canvas, 1, [10, 10], [12, 11]);
      await nextFrame();
      expect(view()).toEqual(before);
      expect(canvas.hasAttribute("data-dragging")).toBe(false);

      pointer(canvas, "pointermove", 1, [40, 30]);
      flush();
      expect(canvas.hasAttribute("data-dragging")).toBe(true);
      pointer(canvas, "pointerup", 1, [40, 30]);
      flush();
      expect(canvas.hasAttribute("data-dragging")).toBe(false);
      await nextFrame();
      expect(view()).toEqual({ x: before.x + 30, y: before.y + 20, k: before.k });
      node(sample).dispatchEvent(new MouseEvent("click", { bubbles: true }));
      flush();
      expect(selected()).toBeUndefined();

      node(sample).dispatchEvent(new MouseEvent("click", { bubbles: true }));
      flush();
      expect(selected()).toBe(sample);
      canvas.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      flush();
      expect(selected()).toBeUndefined();
    });

    it("ignores a drag with another mouse button than the main one", async () => {
      const { canvas, view } = await open();
      const before = view();
      drag(canvas, 1, [10, 10], [80, 80], { button: 2 });
      await nextFrame();
      expect(view()).toEqual(before);
      // A pointer that is not down moves nothing either.
      pointer(canvas, "pointermove", 7, [90, 90]);
      await nextFrame();
      expect(view()).toEqual(before);
    });

    it("zooms with two fingers around their middle, and pans with it", async () => {
      const { canvas, view, node, sample, selected } = await open();
      const before = view();
      const touch = { pointerType: "touch" };
      pointer(canvas, "pointerdown", 1, [100, 100], touch);
      pointer(canvas, "pointerdown", 2, [200, 100], touch);
      pointer(canvas, "pointermove", 2, [300, 100], touch);
      await nextFrame();
      const after = view();
      expect(after.k).toBeCloseTo(Math.min(2.5, before.k * 2));
      // The middle of the fingers stays over the same point of the drawing as it moves.
      const under = (one: View, x: number, y: number) => ({
        x: (x - one.x) / one.k,
        y: (y - one.y) / one.k,
      });
      expect(under(after, 200, 100).x).toBeCloseTo(under(before, 150, 100).x);
      expect(under(after, 200, 100).y).toBeCloseTo(under(before, 150, 100).y);
      // Once one finger lifts, the other pans alone.
      pointer(canvas, "pointerup", 2, [300, 100], touch);
      pointer(canvas, "pointermove", 1, [110, 120], touch);
      await nextFrame();
      expect(view()).toEqual({ x: after.x + 10, y: after.y + 20, k: after.k });
      pointer(canvas, "pointercancel", 1, [110, 120], touch);
      flush();
      expect(canvas.hasAttribute("data-dragging")).toBe(false);
      // The tap that ends a pinch selects nothing.
      node(sample).dispatchEvent(new MouseEvent("click", { bubbles: true }));
      flush();
      expect(selected()).toBeUndefined();
    });

    it("zooms with the wheel around the pointer", async () => {
      const { canvas, view } = await open();
      const before = view();
      const wheel = new WheelEvent("wheel", {
        deltaY: -100,
        clientX: 0,
        clientY: 0,
        cancelable: true,
      });
      canvas.dispatchEvent(wheel);
      expect(wheel.defaultPrevented).toBe(true);
      await nextFrame();
      expect(view().k).toBeCloseTo(before.k * Math.exp(0.15));
      canvas.dispatchEvent(
        new WheelEvent("wheel", { deltaY: 3, deltaMode: 1, clientX: 0, clientY: 0 }),
      );
      await nextFrame();
      expect(view().k).toBeCloseTo(before.k);
    });

    it("zooms with + and −, and with the buttons, and shows it whole again with 0 and Fit", async () => {
      const { host, canvas, view, step } = await open();
      const first = view();
      expect(press(canvas, "+")).toBe(true);
      await nextFrame();
      expect(view().k).toBeCloseTo(first.k * step);
      press(canvas, "-");
      press(canvas, "_");
      await nextFrame();
      expect(view().k).toBeCloseTo(first.k / step);
      press(canvas, "=");
      await nextFrame();
      expect(view().k).toBeCloseTo(first.k);
      find<HTMLButtonElement>(host, 'button[aria-label="Zoom in"]').click();
      await nextFrame();
      expect(view().k).toBeCloseTo(first.k * step);
      find<HTMLButtonElement>(host, 'button[aria-label="Zoom out"]').click();
      find<HTMLButtonElement>(host, 'button[aria-label="Zoom out"]').click();
      await nextFrame();
      expect(view().k).toBeCloseTo(first.k / step);
      expect(press(canvas, "0")).toBe(true);
      await nextFrame();
      const whole = view();
      drag(canvas, 1, [10, 10], [60, 60]);
      pointer(canvas, "pointerup", 1, [60, 60]);
      await nextFrame();
      expect(view()).not.toEqual(whole);
      find<HTMLButtonElement>(host, 'button[title^="Show the whole"]').click();
      await nextFrame();
      expect(view()).toEqual(whole);
    });

    it("lets keys with Ctrl, Alt or Meta go to the browser", async () => {
      const { canvas, view } = await open();
      const before = view();
      for (const modifier of ["ctrlKey", "altKey", "metaKey"] as const) {
        expect(press(canvas, "+", { [modifier]: true })).toBe(false);
      }
      await nextFrame();
      expect(view()).toEqual(before);
    });

    it("selects with Enter or Space on a node only, and lets Escape go when nothing is selected", async () => {
      const { canvas, node, selected, sample } = await open();
      expect(press(canvas, "Enter")).toBe(false);
      expect(press(canvas, "Escape")).toBe(false);
      expect(press(node(sample), " ")).toBe(true);
      expect(selected()).toBe(sample);
      expect(press(node(sample), "Escape")).toBe(true);
      expect(selected()).toBeUndefined();
      press(node(sample), "Enter");
      expect(selected()).toBe(sample);
      expect(press(node(sample), "x")).toBe(false);
    });
  });
}
