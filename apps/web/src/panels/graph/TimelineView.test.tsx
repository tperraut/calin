import { createSignal, flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { Timeline } from "../../gen/Timeline";
import { press, wheel } from "../../test/canvas";
import { makeSpan, makeTimeline, T0 } from "../../test/orchestration";
import { find, mount, unmountAll } from "../../test/render";
import { type TimelineSelection, TimelineView } from "./TimelineView";

const NOW = T0 + 60_000;
/** happy-dom lays nothing out: the chart gets 360 px, of which the lane column takes 176. */
const LABELS = 176;
/** The fitted chart: 360 − 176 = 184 px, less 24 of padding, for the 60 s of history. */
const FIT_PX_PER_MS = 160 / 60_000;
const widthAt = (pxPerMs: number): number => Math.ceil(pxPerMs * 60_000) + 24;

beforeEach(() => {
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue(
    DOMRect.fromRect({ x: 0, y: 0, width: 360, height: 300 }),
  );
});
afterEach(() => {
  unmountAll();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function setup(timeline: Timeline = makeTimeline()) {
  const [selection, setSelection] = createSignal<TimelineSelection>();
  const host = mount(() => (
    <TimelineView
      timeline={timeline}
      openSessionId="s1"
      titleOf={() => undefined}
      now={NOW}
      selection={selection()}
      onSelect={(next) => setSelection(next)}
    />
  ));
  const chart = find<SVGSVGElement>(host, 'svg[aria-label="Timeline"]');
  return {
    host,
    chart,
    scroller: find<HTMLDivElement>(host, "div.overflow-auto"),
    selection,
    width: () => Number(chart.getAttribute("width")),
    span: (key: string) => find<SVGGElement>(host, `[data-span="${key}"]`),
    arrow: (id: string) => find<SVGGElement>(host, `[data-arrow="${id}"]`),
    zoomOut: find<HTMLButtonElement>(host, 'button[aria-label="Zoom out"]'),
    fit: find<HTMLButtonElement>(host, 'button[title="Show the whole history"]'),
  };
}

const touches = (target: Element, xs: readonly number[]): Touch[] =>
  xs.map((x, identifier) => new Touch({ identifier, target, clientX: x, clientY: 50 }));
function touch(target: Element, type: "touchstart" | "touchmove", xs: readonly number[]): boolean {
  const event = new TouchEvent(type, { touches: touches(target, xs), cancelable: true });
  target.dispatchEvent(event);
  flush();
  return event.defaultPrevented;
}

describe("TimelineView: zoom", () => {
  it("zooms in with + and =, out with - and _, and stops at the whole history", () => {
    const { chart, width, zoomOut, fit } = setup();
    expect(width()).toBe(widthAt(FIT_PX_PER_MS));
    expect(zoomOut.disabled).toBe(true);
    expect(fit.getAttribute("aria-pressed")).toBe("true");

    expect(press(chart, "+")).toBe(true);
    expect(width()).toBe(widthAt(FIT_PX_PER_MS * 1.5));
    expect(zoomOut.disabled).toBe(false);
    expect(fit.getAttribute("aria-pressed")).toBe("false");
    press(chart, "=");
    expect(width()).toBe(widthAt(FIT_PX_PER_MS * 2.25));
    expect(press(chart, "-")).toBe(true);
    expect(width()).toBe(widthAt(FIT_PX_PER_MS * 1.5));
    press(chart, "_");
    expect(width()).toBe(widthAt(FIT_PX_PER_MS));
    expect(zoomOut.disabled).toBe(true);
    // Below the fit is the fit.
    expect(press(chart, "-")).toBe(true);
    expect(fit.getAttribute("aria-pressed")).toBe("true");
  });

  it("lets keys with Ctrl, Alt or Meta go to the browser", () => {
    const { chart, span, width } = setup();
    for (const modifier of ["ctrlKey", "altKey", "metaKey"] as const) {
      expect(press(chart, "+", { [modifier]: true })).toBe(false);
      expect(press(span("a1/turn-1"), "ArrowRight", { [modifier]: true })).toBe(false);
    }
    expect(width()).toBe(widthAt(FIT_PX_PER_MS));
  });

  it("never makes the chart wider than a browser draws", () => {
    const { chart, width } = setup();
    for (let turn = 0; turn < 30; turn += 1) press(chart, "+");
    expect(width()).toBe(1_500_024);
  });

  it("zooms with Ctrl or Meta and the wheel, keeping the moment under the pointer; a wheel alone scrolls", () => {
    const { scroller, width, fit } = setup();
    const twice = (x: number, init: WheelEventInit = {}): boolean => {
      const event = wheel(scroller, [x, 0], { deltaY: -100 * Math.log(2), ...init });
      flush();
      return event.defaultPrevented;
    };
    expect(twice(LABELS + 100)).toBe(false);
    expect(width()).toBe(widthAt(FIT_PX_PER_MS));

    expect(twice(LABELS + 100, { ctrlKey: true })).toBe(true);
    expect(width()).toBe(widthAt(FIT_PX_PER_MS * 2));
    expect(scroller.scrollLeft).toBeCloseTo(100);

    // Fit keeps the moment at the left edge (a browser then clamps the scroll; happy-dom does not).
    fit.click();
    flush();
    expect(width()).toBe(widthAt(FIT_PX_PER_MS));
    expect(scroller.scrollLeft).toBeCloseTo(50);
    // Over the lane column, the zoom keeps the chart's left edge.
    expect(twice(40, { metaKey: true })).toBe(true);
    expect(width()).toBe(widthAt(FIT_PX_PER_MS * 2));
    expect(scroller.scrollLeft).toBeCloseTo(100);
  });

  it("zooms with two fingers around their middle", () => {
    const { scroller, width } = setup();
    // One finger is a scroll.
    touch(scroller, "touchstart", [LABELS + 50]);
    expect(touch(scroller, "touchmove", [LABELS + 50, LABELS + 150])).toBe(false);
    expect(touch(scroller, "touchmove", [LABELS + 80])).toBe(false);
    expect(width()).toBe(widthAt(FIT_PX_PER_MS));

    touch(scroller, "touchstart", [LABELS + 50, LABELS + 150]);
    expect(touch(scroller, "touchmove", [LABELS, LABELS + 200])).toBe(true);
    expect(width()).toBe(widthAt(FIT_PX_PER_MS * 2));
    expect(scroller.scrollLeft).toBeCloseTo(100);
    // The next move zooms from where the last one left the fingers.
    touch(scroller, "touchmove", [LABELS - 100, LABELS + 300]);
    expect(width()).toBe(widthAt(FIT_PX_PER_MS * 4));
    expect(touch(scroller, "touchmove", [LABELS])).toBe(false);
  });
});

describe("TimelineView: the keyboard", () => {
  it("steps through the arrows with Left and Right, and opens one with Enter or Space", () => {
    const { host, chart, arrow, selection } = setup();
    expect(
      [...host.querySelectorAll('[data-arrow][tabindex="0"]')].map((one) =>
        one.getAttribute("data-arrow"),
      ),
    ).toEqual(["ar1"]);
    arrow("ar1").focus();
    expect(press(arrow("ar1"), "ArrowRight")).toBe(true);
    expect(document.activeElement).toBe(arrow("ar2"));
    expect(press(arrow("ar2"), "ArrowLeft")).toBe(true);
    expect(document.activeElement).toBe(arrow("ar1"));
    // Past either end the browser keeps the key.
    expect(press(arrow("ar1"), "ArrowLeft")).toBe(false);
    expect(press(arrow("ar4"), "ArrowRight")).toBe(false);
    expect(press(arrow("ar2"), "ArrowDown")).toBe(false);
    expect(document.activeElement).toBe(arrow("ar1"));

    expect(press(arrow("ar2"), "Enter")).toBe(true);
    expect(selection()).toEqual({ kind: "arrow", id: "ar2" });
    expect(arrow("ar2").hasAttribute("data-selected")).toBe(true);
    expect(arrow("ar2").getAttribute("aria-pressed")).toBe("true");
    press(arrow("ar3"), " ");
    expect(selection()).toEqual({ kind: "arrow", id: "ar3" });
    expect(arrow("ar2").getAttribute("aria-pressed")).toBe("false");
    // Outside a span or an arrow the keys are the browser's.
    expect(press(chart, "ArrowRight")).toBe(false);
    expect(press(chart, "Enter")).toBe(false);
  });

  it("moves Up and Down to the span nearest in time in the next lane that has any", () => {
    const base = makeTimeline();
    const { host, span, selection } = setup({
      ...base,
      lanes: [
        {
          ...base.lanes[0],
          spans: [makeSpan("early"), makeSpan("late", { start: T0 + 40_000, end: T0 + 42_000 })],
        },
        { ...base.lanes[1], spans: [] },
        {
          ...base.lanes[2],
          spans: [
            makeSpan("p", { start: T0 + 10_000, end: T0 + 12_000 }),
            makeSpan("q", { start: T0 + 45_000, end: T0 + 50_000 }),
          ],
        },
      ],
    });
    const stop = () =>
      [...host.querySelectorAll('[data-span][tabindex="0"]')].map((one) =>
        one.getAttribute("data-span"),
      );
    expect(stop()).toEqual(["a1/early"]);
    span("a1/late").focus();
    flush();
    expect(stop()).toEqual(["a1/late"]);
    expect(press(span("a1/late"), "ArrowDown")).toBe(true);
    expect(document.activeElement).toBe(span("a2/q"));
    expect(press(span("a2/q"), "ArrowDown")).toBe(true);
    expect(document.activeElement).toBe(span("a2/q"));
    press(span("a2/q"), "ArrowLeft");
    expect(document.activeElement).toBe(span("a2/p"));
    expect(press(span("a2/p"), "ArrowLeft")).toBe(true);
    expect(document.activeElement).toBe(span("a2/p"));
    press(span("a2/p"), "ArrowUp");
    expect(document.activeElement).toBe(span("a1/early"));
    expect(press(span("a1/early"), "ArrowUp")).toBe(true);
    expect(document.activeElement).toBe(span("a1/early"));
    expect(stop()).toEqual(["a1/early"]);
    expect(press(span("a1/early"), "x")).toBe(false);
    expect(press(span("a1/early"), " ")).toBe(true);
    expect(selection()).toEqual({ kind: "span", agentId: "a1", id: "early" });
    expect(span("a1/early").getAttribute("aria-pressed")).toBe("true");
  });

  it("gives the tab stop to the first span of the first lane that has one", () => {
    const base = makeTimeline();
    const { host } = setup({ ...base, lanes: [{ ...base.lanes[0], spans: [] }, base.lanes[1]] });
    expect(
      [...host.querySelectorAll('[data-span][tabindex="0"]')].map((one) =>
        one.getAttribute("data-span"),
      ),
    ).toEqual(["sub1/turn-sub1"]);
  });

  it("scrolls what the keys reach into view, and draws it", () => {
    const { chart, scroller, span, arrow } = setup();
    Object.defineProperty(scroller, "clientHeight", { value: 100 });
    for (let turn = 0; turn < 8; turn += 1) press(chart, "+");
    const pxPerMs = FIT_PX_PER_MS * 1.5 ** 8;
    const room = 360 - LABELS;

    span("a1/turn-1").focus();
    press(span("a1/turn-1"), "ArrowRight");
    expect(document.activeElement).toBe(span("a1/t1"));
    // Past the right edge: the span lands a third of the way in.
    expect(scroller.scrollLeft).toBeCloseTo(5000 * pxPerMs - room / 3);
    expect(scroller.scrollTop).toBe(0);
    press(span("a1/t1"), "ArrowDown");
    expect(document.activeElement).toBe(span("sub1/turn-sub1"));
    // Below the bottom edge: the lane's bottom and the axis come into view.
    expect(scroller.scrollTop).toBe(44 + 44 + 28 - 100);
    press(span("sub1/turn-sub1"), "ArrowDown");
    expect(document.activeElement).toBe(span("a2/turn-a2"));
    expect(scroller.scrollTop).toBe(88 + 44 + 28 - 100);
    expect(scroller.scrollLeft).toBeCloseTo(10_000 * pxPerMs - room / 3);
    // Above the top edge: the lane's top; left of the left edge, a third of the way in again.
    press(span("a2/turn-a2"), "ArrowUp");
    expect(document.activeElement).toBe(span("sub1/turn-sub1"));
    expect(scroller.scrollTop).toBe(44);
    expect(scroller.scrollLeft).toBeCloseTo(2000 * pxPerMs - room / 3);
    press(span("sub1/turn-sub1"), "ArrowUp");
    expect(document.activeElement).toBe(span("a1/turn-1"));
    expect(scroller.scrollTop).toBe(0);
    expect(scroller.scrollLeft).toBe(0);
    // Already in view: no scroll.
    scroller.scrollLeft = 300;
    press(span("a1/turn-1"), "ArrowRight");
    expect(document.activeElement).toBe(span("a1/t1"));
    expect(scroller.scrollLeft).toBe(300);

    // The last arrow is culled until the keys reach it.
    expect(chart.querySelector('[data-arrow="ar4"]')).toBeNull();
    press(arrow("ar3"), "ArrowRight");
    expect(document.activeElement).toBe(arrow("ar4"));
    expect(scroller.scrollLeft).toBeCloseTo(55_000 * pxPerMs - room / 3);
  });
});

describe("TimelineView: the scroll", () => {
  it("draws what a scroll brings near the viewport, once per frame", async () => {
    const { chart, scroller, host } = setup();
    for (let turn = 0; turn < 8; turn += 1) press(chart, "+");
    const held = () =>
      [...host.querySelectorAll("[data-span], [data-arrow]")].map(
        (one) => one.getAttribute("data-span") ?? one.getAttribute("data-arrow"),
      );
    expect(held()).toContain("a1/turn-1");
    expect(held()).not.toContain("ar4");
    scroller.scrollLeft = 55_000 * FIT_PX_PER_MS * 1.5 ** 8 - 50;
    scroller.dispatchEvent(new Event("scroll"));
    scroller.dispatchEvent(new Event("scroll"));
    await vi.waitFor(() => {
      flush();
      expect(held()).toContain("ar4");
    });
    expect(held()).not.toContain("a1/turn-1");
    expect(held()).toContain("a2/c1");
  });
});

describe("TimelineView: the pointer", () => {
  it("selects a span or an arrow on click, and nothing on the chart's background", () => {
    const { chart, span, arrow, selection } = setup();
    span("a2/c1").dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flush();
    expect(selection()).toEqual({ kind: "span", agentId: "a2", id: "c1" });
    expect(span("a2/c1").hasAttribute("data-selected")).toBe(true);
    arrow("ar4").dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flush();
    expect(selection()).toEqual({ kind: "arrow", id: "ar4" });
    expect(span("a2/c1").hasAttribute("data-selected")).toBe(false);
    chart.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flush();
    expect(selection()).toBeUndefined();
  });
});

describe("TimelineView: the lane column", () => {
  let resize: ((width: number) => void) | undefined;
  class FakeResizeObserver {
    constructor(callback: (entries: { contentRect: { width: number } }[]) => void) {
      resize = (width) => callback([{ contentRect: { width } }]);
    }
    observe(): void {}
    disconnect(): void {}
  }

  it("is narrower on a narrow screen, and the chart takes the rest", () => {
    vi.stubGlobal("ResizeObserver", FakeResizeObserver);
    const { host, chart, width } = setup();
    const column = () => find(host, ".sticky").style.width;
    resize?.(400);
    flush();
    expect(column()).toBe("104px");
    expect(chart.style.left).toBe("104px");
    expect(width()).toBe(Math.ceil((272 / 60_000) * 60_000) + 24);
    resize?.(800);
    flush();
    expect(column()).toBe("176px");
    expect(width()).toBe(Math.ceil((600 / 60_000) * 60_000) + 24);
  });
});
