// Canvas 2D renderer for the same scene: everything visible is redrawn whenever something changed.

import { trace } from "./geom.ts";
import { LOD_BELOW, type Dirty, type Renderer, type Scene } from "./scene.ts";

export const EDGE_COLOR = { spawned: "#6b6f66", message: "#5aa9ff", changed: "#d9a441" } as const;
export const NODE_STROKE = { user: "#c084fc", agent: "#7d8278", sub: "#4b4f47", repo: "#d9a441" } as const;
export const STATUS = ["#4ade80", "#fbbf24", "#8b8f85"];
export const BG = "#141513";
export const CARD = "#20221f";
const KINDS = ["spawned", "changed", "message"] as const;
const NODE_KINDS = ["user", "agent", "sub", "repo"] as const;

export function drawPulses(ctx: CanvasRenderingContext2D, scene: Scene, dpr: number): void {
  const { x, y, k } = scene.view;
  ctx.setTransform(dpr * k, 0, 0, dpr * k, dpr * x, dpr * y);
  ctx.fillStyle = "#ffffff";
  ctx.beginPath();
  for (const p of scene.pulses) {
    ctx.moveTo(p.x + 4, p.y);
    ctx.arc(p.x, p.y, 4, 0, Math.PI * 2);
  }
  ctx.fill();
}

export function createCanvasRenderer(host: HTMLElement, scene: Scene): Renderer {
  const dpr = Math.min(window.devicePixelRatio || 1, scene.opts.cdpr);
  const make = () => {
    const c = document.createElement("canvas");
    c.width = Math.round(scene.vw * dpr);
    c.height = Math.round(scene.vh * dpr);
    c.style.cssText = "position:absolute;inset:0;width:100%;height:100%";
    host.prepend(c);
    return c;
  };
  const overlay = scene.opts.layers === 2 ? make() : null;
  const base = make();
  const ctx = base.getContext("2d", { alpha: false })!;
  const octx = overlay?.getContext("2d") ?? null;

  const drawStatic = () => {
    const { x, y, k } = scene.view;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = BG;
    ctx.fillRect(0, 0, base.width, base.height);
    ctx.setTransform(dpr * k, 0, 0, dpr * k, dpr * x, dpr * y);
    const x0 = -x / k, y0 = -y / k, x1 = (scene.vw - x) / k, y1 = (scene.vh - y) / k;
    const lod = scene.opts.lod && k < LOD_BELOW;

    for (const e of scene.edges) {
      const p = e.pts;
      let ax = Infinity, ay = Infinity, bx = -Infinity, by = -Infinity;
      for (let i = 0; i < p.length; i += 2) {
        if (p[i] < ax) ax = p[i];
        if (p[i] > bx) bx = p[i];
        if (p[i + 1] < ay) ay = p[i + 1];
        if (p[i + 1] > by) by = p[i + 1];
      }
      e.vis = ax < x1 && bx > x0 && ay < y1 && by > y0;
    }
    ctx.lineWidth = 1.5;
    for (const kind of KINDS) {
      ctx.beginPath();
      for (const e of scene.edges) if (e.vis && e.g.kind === kind) trace(ctx, e.pts);
      ctx.strokeStyle = EDGE_COLOR[kind];
      ctx.stroke();
      if (lod) continue;
      ctx.beginPath();
      for (const e of scene.edges) {
        if (!e.vis || e.g.kind !== kind) continue;
        const p = e.pts, m = p.length;
        const dx = p[m - 2] - p[m - 4], dy = p[m - 1] - p[m - 3];
        const l = Math.hypot(dx, dy) || 1, ux = dx / l, uy = dy / l;
        ctx.moveTo(p[m - 2], p[m - 1]);
        ctx.lineTo(p[m - 2] - ux * 7 - uy * 3.5, p[m - 1] - uy * 7 + ux * 3.5);
        ctx.lineTo(p[m - 2] - ux * 7 + uy * 3.5, p[m - 1] - uy * 7 - ux * 3.5);
        ctx.closePath();
      }
      ctx.fillStyle = EDGE_COLOR[kind];
      ctx.fill();
    }

    for (const n of scene.nodes) n.vis = n.x < x1 && n.x + n.g.w > x0 && n.y < y1 && n.y + n.g.h > y0;
    ctx.fillStyle = CARD;
    for (const kind of NODE_KINDS) {
      ctx.beginPath();
      for (const n of scene.nodes) if (n.vis && n.g.kind === kind) ctx.roundRect(n.x, n.y, n.g.w, n.g.h, 8);
      ctx.fill();
      ctx.strokeStyle = NODE_STROKE[kind];
      ctx.stroke();
    }
    if (lod) return;
    for (let s = 0; s < STATUS.length; s++) {
      ctx.beginPath();
      for (const n of scene.nodes) {
        if (!n.vis || n.status !== s) continue;
        ctx.moveTo(n.x + 16, n.y + n.g.h / 2);
        ctx.arc(n.x + 12, n.y + n.g.h / 2, 4, 0, Math.PI * 2);
      }
      ctx.fillStyle = STATUS[s];
      ctx.fill();
    }
    ctx.fillStyle = "#e8e8e3";
    ctx.font = "12px system-ui, sans-serif";
    for (const n of scene.nodes) if (n.vis) ctx.fillText(n.g.label, n.x + 22, n.y + n.g.h / 2 + 4);
  };

  return {
    frame(d: Dirty) {
      if (!octx || !overlay) {
        drawStatic();
        if (scene.pulses.length) drawPulses(ctx, scene, dpr);
        return;
      }
      if (d.view || d.geom || d.structure) drawStatic();
      if (d.pulses || d.view || d.structure) {
        octx.setTransform(1, 0, 0, 1, 0, 0);
        octx.clearRect(0, 0, overlay.width, overlay.height);
        if (scene.pulses.length) drawPulses(octx, scene, dpr);
      }
    },
  };
}
