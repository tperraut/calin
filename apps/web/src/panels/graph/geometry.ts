import type { LayoutMode, Point, Size } from "./layout";

export type Box = Point & Size;

/**
 * How an edge runs. `elbow`: down from under the parent, then across to the
 * child (the tree lines of the indented layout). `cubic`: one Bézier curve.
 */
export type Route =
  | { readonly kind: "elbow"; x0: number; y0: number; x1: number; y1: number }
  | {
      readonly kind: "cubic";
      x0: number;
      y0: number;
      c1x: number;
      c1y: number;
      c2x: number;
      c2y: number;
      x1: number;
      y1: number;
    };

const round = (value: number): number => Math.round(value * 10) / 10;

/** Where a tree line leaves its parent in the indented layout, from the card's left edge. */
const TRUNK_INSET = 14;
const CORNER = 6;

/** The line from a parent to its child in the spawn tree. */
export function treeRoute(mode: LayoutMode, parent: Box, child: Box): Route {
  if (mode === "indented") {
    return {
      kind: "elbow",
      x0: parent.x + TRUNK_INSET,
      y0: parent.y + parent.h,
      x1: child.x,
      y1: child.y + child.h / 2,
    };
  }
  const x0 = parent.x + parent.w;
  const y0 = parent.y + parent.h / 2;
  const x1 = child.x;
  const y1 = child.y + child.h / 2;
  const reach = Math.max(16, (x1 - x0) / 2);
  return { kind: "cubic", x0, y0, c1x: x0 + reach, c1y: y0, c2x: x1 - reach, c2y: y1, x1, y1 };
}

/**
 * A curve drawn over the tree, from the side of one card to another. `lane`
 * (0, 1, 2…) tells apart several edges between the same two cards: each bulges
 * a little further and attaches a little lower. `intoTop` ends on the top edge
 * of the target, for a band that lies under the tree.
 */
export function curveRoute(from: Box, to: Box, lane: number, intoTop: boolean): Route {
  const shift = 5 + lane * 4;
  // Leaving above the middle and arriving below it keeps A→B apart from B→A.
  const y0 = from.y + from.h / 2 - shift;
  if (intoTop && to.y >= from.y + from.h) {
    const x0 = from.x + from.w;
    const x1 = to.x + to.w / 2 + (lane - 1) * 10;
    const y1 = to.y;
    const reach = 28 + lane * 10 + Math.min(80, Math.abs(x1 - x0) / 4);
    const drop = Math.max(24, Math.min(120, (y1 - y0) / 2));
    return { kind: "cubic", x0, y0, c1x: x0 + reach, c1y: y0, c2x: x1, c2y: y1 - drop, x1, y1 };
  }
  const y1 = to.y + to.h / 2 + shift;
  if (to.x >= from.x + from.w + 8) {
    const x0 = from.x + from.w;
    const x1 = to.x;
    const reach = Math.max(20, (x1 - x0) / 2);
    return { kind: "cubic", x0, y0, c1x: x0 + reach, c1y: y0, c2x: x1 - reach, c2y: y1, x1, y1 };
  }
  if (to.x + to.w <= from.x - 8) {
    const x0 = from.x;
    const x1 = to.x + to.w;
    const reach = Math.max(20, (x0 - x1) / 2);
    return { kind: "cubic", x0, y0, c1x: x0 - reach, c1y: y0, c2x: x1 + reach, c2y: y1, x1, y1 };
  }
  // The cards overlap sideways (one above the other): out and back in on the right, bulging outwards.
  const x0 = from.x + from.w;
  const x1 = to.x + to.w;
  const out = Math.max(x0, x1) + 24 + lane * 12 + Math.min(72, Math.abs(y1 - y0) / 6);
  return { kind: "cubic", x0, y0, c1x: out, c1y: y0, c2x: out, c2y: y1, x1, y1 };
}

/** SVG path data for a route. */
export function routePath(route: Route): string {
  if (route.kind === "elbow") {
    const { x0, y0, x1, y1 } = route;
    const r = Math.max(0, Math.min(CORNER, Math.abs(y1 - y0), Math.abs(x1 - x0)));
    const down = y1 >= y0 ? 1 : -1;
    const across = x1 >= x0 ? 1 : -1;
    return (
      `M${round(x0)},${round(y0)}V${round(y1 - down * r)}` +
      `Q${round(x0)},${round(y1)},${round(x0 + across * r)},${round(y1)}H${round(x1)}`
    );
  }
  return (
    `M${round(route.x0)},${round(route.y0)}C${round(route.c1x)},${round(route.c1y)},` +
    `${round(route.c2x)},${round(route.c2y)},${round(route.x1)},${round(route.y1)}`
  );
}

/** The point at `t` (0 to 1) along a route: where a pulse is, where a count sits. */
export function routePoint(route: Route, t: number): Point {
  const at = Math.min(1, Math.max(0, t));
  if (route.kind === "elbow") {
    const down = Math.abs(route.y1 - route.y0);
    const across = Math.abs(route.x1 - route.x0);
    const travelled = at * (down + across);
    if (travelled <= down) {
      return { x: route.x0, y: route.y0 + Math.sign(route.y1 - route.y0) * travelled };
    }
    return { x: route.x0 + Math.sign(route.x1 - route.x0) * (travelled - down), y: route.y1 };
  }
  const u = 1 - at;
  const a = u * u * u;
  const b = 3 * u * u * at;
  const c = 3 * u * at * at;
  const d = at * at * at;
  return {
    x: a * route.x0 + b * route.c1x + c * route.c2x + d * route.x1,
    y: a * route.y0 + b * route.c1y + c * route.c2y + d * route.y1,
  };
}

export const translate = (x: number, y: number): string => `translate(${round(x)},${round(y)})`;

/** The view that shows a drawing of `content` whole inside `viewport`, never magnified past `maxZoom`. */
export function fitView(
  content: Size,
  viewport: Size,
  padding: number,
  minZoom: number,
  maxZoom: number,
): { x: number; y: number; k: number } {
  const roomW = Math.max(1, viewport.w - 2 * padding);
  const roomH = Math.max(1, viewport.h - 2 * padding);
  const wanted = Math.min(roomW / Math.max(1, content.w), roomH / Math.max(1, content.h));
  const k = Math.min(maxZoom, Math.max(minZoom, wanted));
  // Centered when it fits; from the top-left corner when it does not.
  const x = content.w * k <= roomW ? (viewport.w - content.w * k) / 2 : padding;
  const y = content.h * k <= roomH ? (viewport.h - content.h * k) / 2 : padding;
  return { x, y, k };
}
