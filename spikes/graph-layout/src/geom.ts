// Polyline helpers shared by both renderers. An edge is a flattened Float32Array of x,y pairs.

const r1 = (v: number) => Math.round(v * 10) / 10;

/** A direct S-curve between two node boxes, sampled to 13 points, for edges ELK did not route. */
export function routeDirect(
  sx: number, sy: number, sw: number, sh: number,
  tx: number, ty: number, tw: number, th: number,
): Float32Array {
  const y0 = sy + sh / 2, y3 = ty + th / 2;
  let x0: number, x3: number, c1: number, c2: number;
  if (tx >= sx + sw) {
    x0 = sx + sw; x3 = tx;
    const d = Math.max(20, (x3 - x0) / 2);
    c1 = x0 + d; c2 = x3 - d;
  } else if (tx + tw <= sx) {
    x0 = sx; x3 = tx + tw;
    const d = Math.max(20, (x0 - x3) / 2);
    c1 = x0 - d; c2 = x3 + d;
  } else {
    // Same column (siblings): leave and enter on the right, bulging outwards.
    x0 = sx + sw; x3 = tx + tw;
    const d = 24 + Math.min(60, Math.abs(y3 - y0) / 6);
    c1 = x0 + d; c2 = x3 + d;
  }
  const out = new Float32Array(26);
  for (let i = 0; i <= 12; i++) {
    const t = i / 12, u = 1 - t;
    out[i * 2] = u * u * u * x0 + 3 * u * u * t * c1 + 3 * u * t * t * c2 + t * t * t * x3;
    out[i * 2 + 1] = u * u * u * y0 + 3 * u * u * t * y0 + 3 * u * t * t * y3 + t * t * t * y3;
  }
  return out;
}

/** `src` resampled by arc length to `n` points, so two routes can be interpolated point by point. */
export function resample(src: Float32Array, n: number): Float32Array {
  const m = src.length / 2;
  if (m === n) return src.slice();
  const out = new Float32Array(n * 2);
  let total = 0;
  for (let i = 1; i < m; i++) total += Math.hypot(src[i * 2] - src[i * 2 - 2], src[i * 2 + 1] - src[i * 2 - 1]);
  let seg = 1, acc = 0;
  for (let k = 0; k < n; k++) {
    const want = (total * k) / (n - 1);
    let len = Math.hypot(src[seg * 2] - src[seg * 2 - 2], src[seg * 2 + 1] - src[seg * 2 - 1]);
    while (seg < m - 1 && acc + len < want) {
      acc += len;
      seg++;
      len = Math.hypot(src[seg * 2] - src[seg * 2 - 2], src[seg * 2 + 1] - src[seg * 2 - 1]);
    }
    const t = len > 0 ? Math.min(1, Math.max(0, (want - acc) / len)) : 0;
    out[k * 2] = src[seg * 2 - 2] + (src[seg * 2] - src[seg * 2 - 2]) * t;
    out[k * 2 + 1] = src[seg * 2 - 1] + (src[seg * 2 + 1] - src[seg * 2 - 1]) * t;
  }
  return out;
}

/** The point at fraction `f` of the polyline's length, written to `out`. */
export function pointAt(p: Float32Array, f: number, out: { x: number; y: number }): void {
  const m = p.length / 2;
  let total = 0;
  for (let i = 1; i < m; i++) total += Math.hypot(p[i * 2] - p[i * 2 - 2], p[i * 2 + 1] - p[i * 2 - 1]);
  let want = total * f;
  for (let i = 1; i < m; i++) {
    const len = Math.hypot(p[i * 2] - p[i * 2 - 2], p[i * 2 + 1] - p[i * 2 - 1]);
    if (want <= len || i === m - 1) {
      const t = len > 0 ? Math.min(1, want / len) : 0;
      out.x = p[i * 2 - 2] + (p[i * 2] - p[i * 2 - 2]) * t;
      out.y = p[i * 2 - 1] + (p[i * 2 + 1] - p[i * 2 - 1]) * t;
      return;
    }
    want -= len;
  }
}

const RADIUS = 6;

/** SVG path data. Short routes (ELK's orthogonal ones) get rounded corners; sampled curves do not need them. */
export function pathD(p: Float32Array): string {
  const m = p.length / 2;
  let d = `M${r1(p[0])},${r1(p[1])}`;
  const round = m <= 8;
  for (let i = 1; i < m - 1; i++) {
    const x = p[i * 2], y = p[i * 2 + 1];
    if (!round) {
      d += `L${r1(x)},${r1(y)}`;
      continue;
    }
    const ax = p[i * 2 - 2] - x, ay = p[i * 2 - 1] - y, bx = p[i * 2 + 2] - x, by = p[i * 2 + 3] - y;
    const la = Math.hypot(ax, ay) || 1, lb = Math.hypot(bx, by) || 1;
    const r = Math.min(RADIUS, la / 2, lb / 2);
    d += `L${r1(x + (ax / la) * r)},${r1(y + (ay / la) * r)}Q${r1(x)},${r1(y)},${r1(x + (bx / lb) * r)},${r1(y + (by / lb) * r)}`;
  }
  return d + `L${r1(p[m * 2 - 2])},${r1(p[m * 2 - 1])}`;
}

/** The same shape as `pathD`, traced on a canvas path. */
export function trace(ctx: CanvasRenderingContext2D, p: Float32Array): void {
  const m = p.length / 2;
  ctx.moveTo(p[0], p[1]);
  const round = m <= 8;
  for (let i = 1; i < m - 1; i++) {
    const x = p[i * 2], y = p[i * 2 + 1];
    if (!round) {
      ctx.lineTo(x, y);
      continue;
    }
    const ax = p[i * 2 - 2] - x, ay = p[i * 2 - 1] - y, bx = p[i * 2 + 2] - x, by = p[i * 2 + 3] - y;
    const la = Math.hypot(ax, ay) || 1, lb = Math.hypot(bx, by) || 1;
    const r = Math.min(RADIUS, la / 2, lb / 2);
    ctx.lineTo(x + (ax / la) * r, y + (ay / la) * r);
    ctx.quadraticCurveTo(x, y, x + (bx / lb) * r, y + (by / lb) * r);
  }
  ctx.lineTo(p[m * 2 - 2], p[m * 2 - 1]);
}
