import type { LayoutMode, Point, Size } from "./layout";

export type Box = Point & Size;

/**
 * How an edge runs. `elbow`: down from under the parent, then across to the
 * child (the tree lines of the indented layout). `cubic`: one Bézier curve.
 * `poly`: straight runs with rounded corners (the lanes of `routing.ts`).
 */
export type Route =
  | { readonly kind: "poly"; points: readonly Point[] }
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

/** SVG path data for a route. */
export function routePath(route: Route): string {
  if (route.kind === "poly") return polyPath(route.points);
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
  if (route.kind === "poly") return polyPoint(route.points, at * polyLength(route.points));
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

/** Straight runs through `points`, each corner rounded as far as its two runs allow. */
export function polyPath(points: readonly Point[]): string {
  if (points.length === 0) return "";
  let d = `M${round(points[0].x)},${round(points[0].y)}`;
  for (let index = 1; index < points.length - 1; index += 1) {
    const before = points[index - 1];
    const at = points[index];
    const after = points[index + 1];
    const inLength = Math.hypot(at.x - before.x, at.y - before.y);
    const outLength = Math.hypot(after.x - at.x, after.y - at.y);
    const r = Math.min(CORNER, inLength / 2, outLength / 2);
    if (r < 0.5) {
      d += `L${round(at.x)},${round(at.y)}`;
      continue;
    }
    const ax = at.x - ((at.x - before.x) / inLength) * r;
    const ay = at.y - ((at.y - before.y) / inLength) * r;
    const bx = at.x + ((after.x - at.x) / outLength) * r;
    const by = at.y + ((after.y - at.y) / outLength) * r;
    d += `L${round(ax)},${round(ay)}Q${round(at.x)},${round(at.y)},${round(bx)},${round(by)}`;
  }
  const last = points[points.length - 1];
  if (points.length > 1) d += `L${round(last.x)},${round(last.y)}`;
  return d;
}

export function polyLength(points: readonly Point[]): number {
  let length = 0;
  for (let index = 1; index < points.length; index += 1) {
    length += Math.hypot(
      points[index].x - points[index - 1].x,
      points[index].y - points[index - 1].y,
    );
  }
  return length;
}

/** The point `distance` along `points` from the first (from the last when negative). */
export function polyPoint(points: readonly Point[], distance: number): Point {
  if (points.length === 0) return { x: 0, y: 0 };
  const total = polyLength(points);
  let left = Math.min(total, Math.max(0, distance < 0 ? total + distance : distance));
  for (let index = 1; index < points.length; index += 1) {
    const a = points[index - 1];
    const b = points[index];
    const length = Math.hypot(b.x - a.x, b.y - a.y);
    if (left <= length && length > 0) {
      return { x: a.x + ((b.x - a.x) * left) / length, y: a.y + ((b.y - a.y) * left) / length };
    }
    left -= length;
  }
  return points[points.length - 1];
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
