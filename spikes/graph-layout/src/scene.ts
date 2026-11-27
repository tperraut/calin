// The renderer-independent part: the graph shown so far, its layout, the position animation,
// the view transform, the pulses, the frame loop and the benchmark scenarios.

import { createSignal, type Accessor } from "solid-js";
import { configByName, runLayout, type Config, type ElkLike, type LayoutOut, type Pos } from "./elkopts.ts";
import { generate, prefix, type GEdge, type GNode, type Step } from "./gen.ts";
import { pathD, pointAt, resample, routeDirect } from "./geom.ts";
import { createElk } from "./layout.ts";

export interface Opts {
  n: number;
  e: number;
  renderer: "svg" | "canvas";
  /** How per-frame changes reach the SVG DOM: through Solid signals, or by writing attributes. */
  upd: "solid" | "dom";
  /** Pan/zoom as the `transform` attribute of one group, or as a CSS transform with will-change. */
  pan: "attr" | "css";
  /** Pulses as SVG circles moved every frame, as CSS offset-path animations, or on a canvas overlay. */
  pulse: "svg" | "css" | "canvas";
  cull: boolean;
  lod: boolean;
  /** Canvas renderer: one canvas, or a static one plus an overlay for the pulses. */
  layers: 1 | 2;
  /** Canvas renderer: cap on the backing-store scale (the phone's device pixel ratio is 3). */
  cdpr: number;
  elk: "worker" | "main";
  cfg: string;
}

export function parseOpts(q: URLSearchParams): Opts {
  const pick = <T extends string>(key: string, values: T[]): T => {
    const v = q.get(key) as T | null;
    return v && values.includes(v) ? v : values[0];
  };
  return {
    n: Number(q.get("n") ?? 200),
    e: Number(q.get("e") ?? 300),
    renderer: pick("r", ["svg", "canvas"]),
    upd: pick("upd", ["dom", "solid"]),
    pan: pick("pan", ["attr", "css"]),
    pulse: pick("pulse", ["svg", "css", "canvas"]),
    cull: q.get("cull") === "1",
    lod: q.get("lod") === "1",
    layers: q.get("layers") === "2" ? 2 : 1,
    cdpr: Number(q.get("cdpr") ?? 3),
    elk: pick("elk", ["worker", "main"]),
    cfg: q.get("cfg") ?? "tree-order-noforce",
  };
}

export interface VNode {
  g: GNode;
  x: number; y: number;
  fx: number; fy: number;
  tx: number; ty: number;
  moving: boolean;
  status: number;
  vis: boolean;
  el?: SVGGElement;
  pos: Accessor<string>;
  setPos: (v: string) => void;
}

export interface VEdge {
  g: GEdge;
  pts: Float32Array;
  from: Float32Array;
  to: Float32Array;
  moving: boolean;
  vis: boolean;
  el?: SVGPathElement;
  d: Accessor<string>;
  setD: (v: string) => void;
}

export interface Pulse {
  edge: VEdge;
  phase: number;
  x: number;
  y: number;
  el?: SVGCircleElement;
  t: Accessor<string>;
  setT: (v: string) => void;
}

export interface Dirty {
  view: boolean;
  geom: boolean;
  pulses: boolean;
  structure: boolean;
}

export interface Renderer {
  frame(dirty: Dirty): void;
}

export const LOD_BELOW = 0.45;
export const PULSE_MS = 1600;
const ANIM_MS = 300;

export const tr = (x: number, y: number) => `translate(${Math.round(x * 10) / 10},${Math.round(y * 10) / 10})`;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const frames = (n: number) =>
  new Promise<void>((done) => {
    const step = () => (--n <= 0 ? done() : requestAnimationFrame(step));
    requestAnimationFrame(step);
  });

function summary(xs: number[]) {
  const s = [...xs].sort((a, b) => a - b);
  const at = (p: number) => (s.length ? s[Math.min(s.length - 1, Math.floor(p * s.length))] : 0);
  const r = (v: number) => Math.round(v * 10) / 10;
  return { n: s.length, median: r(at(0.5)), p95: r(at(0.95)), max: r(s.at(-1) ?? 0), mean: r(s.reduce((a, b) => a + b, 0) / Math.max(1, s.length)) };
}

export class Scene {
  readonly opts: Opts;
  readonly steps: Step[];
  readonly cfg: Config;
  nodes: VNode[] = [];
  edges: VEdge[] = [];
  pulses: Pulse[] = [];
  private nodeById = new Map<string, VNode>();
  private edgeById = new Map<string, VEdge>();

  // Structure, for Solid: the arrays are replaced when membership changes.
  readonly nodesSig: Accessor<VNode[]>;
  private setNodesSig: (v: VNode[]) => void;
  readonly edgesSig: Accessor<VEdge[]>;
  private setEdgesSig: (v: VEdge[]) => void;
  readonly pulsesSig: Accessor<Pulse[]>;
  private setPulsesSig: (v: Pulse[]) => void;
  readonly viewT: Accessor<string>;
  setViewT: (v: string) => void;
  readonly hud: Accessor<string>;
  private setHud: (v: string) => void;

  view = { x: 0, y: 0, k: 1 };
  vw = 390;
  vh = 844;
  bbox = { w: 1, h: 1 };
  dirty: Dirty = { view: true, geom: false, pulses: false, structure: false };
  renderer: Renderer | null = null;
  shown = 0;
  layoutLog: { n: number; ms: number; elkMs?: number }[] = [];
  coldMs = 0;
  ready = false;

  private elk: ElkLike | null = null;
  private elkMain: ElkLike | null = null;
  private want = 0;
  private wantAnimate = false;
  private busy: Promise<void> | null = null;
  private animating = false;
  private animStart = 0;
  private animDur = 0;
  private panFn: ((now: number) => void) | null = null;
  private rec: { dt: number[]; js: number[] } | null = null;
  private lastTs = 0;
  private quiet = false;
  private fpsFrames = 0;
  private fpsSince = 0;
  private scratch = { x: 0, y: 0 };

  constructor(opts: Opts) {
    this.opts = opts;
    this.steps = generate(opts.n, opts.e);
    this.cfg = configByName(opts.cfg);
    [this.nodesSig, this.setNodesSig] = createSignal<VNode[]>([]);
    [this.edgesSig, this.setEdgesSig] = createSignal<VEdge[]>([]);
    [this.pulsesSig, this.setPulsesSig] = createSignal<Pulse[]>([]);
    [this.viewT, this.setViewT] = createSignal("");
    [this.hud, this.setHud] = createSignal("laying out…");
  }

  async boot(): Promise<void> {
    const t0 = performance.now();
    if (!this.cfg.custom) this.elk = await createElk(this.opts.elk);
    await this.setCount(this.steps.length, false);
    // Worker start-up and script compile included: what opening a finished session costs.
    this.coldMs = performance.now() - t0;
    this.fit();
    requestAnimationFrame(this.tick);
    await frames(3);
    this.ready = true;
  }

  // ---- graph and layout ------------------------------------------------------------------

  /** Show the first `count` steps. Calls made while a layout is in flight are coalesced. */
  setCount(count: number, animate: boolean): Promise<void> {
    this.want = Math.max(1, Math.min(count, this.steps.length));
    this.wantAnimate = animate;
    if (this.busy) return this.busy;
    const run = (async () => {
      while (this.shown !== this.want) {
        const target = this.want;
        const g = prefix(this.steps, target);
        const prev = new Map<string, Pos>();
        for (const n of this.nodes) prev.set(n.g.id, { x: n.tx, y: n.ty });
        const t0 = performance.now();
        const out = await runLayout(this.elk, g.nodes, g.edges, this.cfg, prev, true);
        this.layoutLog.push({ n: target, ms: performance.now() - t0, elkMs: out.elkMs });
        this.apply(g, out, this.wantAnimate);
        this.shown = target;
      }
    })();
    // Cleared in a continuation: when nothing had to be laid out the body finishes synchronously,
    // and clearing inside it would run before the assignment below.
    this.busy = run.then(() => {
      this.busy = null;
    });
    return this.busy;
  }

  private apply(g: { nodes: GNode[]; edges: GEdge[] }, out: LayoutOut, animate: boolean): void {
    const nodes: VNode[] = [];
    const nodeById = new Map<string, VNode>();
    for (const gn of g.nodes) {
      const p = out.pos.get(gn.id)!;
      let v = this.nodeById.get(gn.id);
      if (!v) {
        const [pos, setPos] = createSignal(tr(p.x, p.y));
        let hash = 0;
        for (let i = 0; i < gn.id.length; i++) hash = (hash * 31 + gn.id.charCodeAt(i)) >>> 0;
        v = { g: gn, x: p.x, y: p.y, fx: p.x, fy: p.y, tx: p.x, ty: p.y, moving: false, status: hash % 3, vis: true, pos, setPos };
      } else {
        v.fx = v.x; v.fy = v.y; v.tx = p.x; v.ty = p.y;
        v.moving = Math.abs(v.tx - v.fx) > 0.25 || Math.abs(v.ty - v.fy) > 0.25;
      }
      nodes.push(v);
      nodeById.set(gn.id, v);
    }
    const edges: VEdge[] = [];
    const edgeById = new Map<string, VEdge>();
    for (const ge of g.edges) {
      const a = nodeById.get(ge.from)!, b = nodeById.get(ge.to)!;
      const to = out.routes.get(ge.id) ?? routeDirect(a.tx, a.ty, a.g.w, a.g.h, b.tx, b.ty, b.g.w, b.g.h);
      let v = this.edgeById.get(ge.id);
      if (!v) {
        const [d, setD] = createSignal(pathD(to));
        v = { g: ge, pts: to.slice(), from: to, to, moving: false, vis: true, d, setD };
      } else {
        v.from = resample(v.pts, to.length / 2);
        v.to = to;
        v.moving = v.pts.length !== to.length;
        for (let i = 0; !v.moving && i < to.length; i++) v.moving = Math.abs(to[i] - v.from[i]) > 0.25;
        if (v.moving) v.pts = v.from.slice();
      }
      edges.push(v);
      edgeById.set(ge.id, v);
    }
    const changed = nodes.length !== this.nodes.length || edges.length !== this.edges.length;
    this.nodes = nodes; this.nodeById = nodeById;
    this.edges = edges; this.edgeById = edgeById;
    this.bbox = { w: out.w, h: out.h };
    if (changed) {
      this.setNodesSig(nodes);
      this.setEdgesSig(edges);
      this.dirty.structure = true;
    }
    this.animating = true;
    this.animStart = performance.now();
    this.animDur = animate ? ANIM_MS : 0;
  }

  setPulses(on: boolean): void {
    const pulses: Pulse[] = [];
    if (on) {
      const messages = this.edges.filter((e) => e.g.kind === "message");
      const pool = messages.length >= 20 ? messages : this.edges;
      const count = Math.min(20, pool.length);
      for (let i = 0; i < count; i++) {
        const [t, setT] = createSignal(tr(0, 0));
        pulses.push({ edge: pool[Math.floor((i * pool.length) / count)], phase: (i * 0.37) % 1, x: 0, y: 0, t, setT });
      }
    }
    this.pulses = pulses;
    this.setPulsesSig(pulses);
    this.dirty.structure = true;
  }

  // ---- view ------------------------------------------------------------------------------

  setView(x: number, y: number, k: number): void {
    this.view.x = x; this.view.y = y; this.view.k = k;
    this.dirty.view = true;
  }

  fitK(): number {
    return Math.max(0.2, Math.min(1.2, this.vw / (this.bbox.w + 40)));
  }

  /** Centre world point (cx, cy) in the viewport at zoom k. */
  centerOn(cx: number, cy: number, k: number): void {
    this.setView(this.vw / 2 - cx * k, this.vh / 2 - cy * k, k);
  }

  fit(): void {
    this.centerOn(this.bbox.w / 2, Math.min(this.bbox.h / 2, this.vh / this.fitK() / 2 - 20), this.fitK());
  }

  /** One-finger pan, two-finger pinch, wheel zoom. */
  attachInput(el: HTMLElement): void {
    this.vw = el.clientWidth || this.vw;
    this.vh = el.clientHeight || this.vh;
    const touches = new Map<number, { x: number; y: number }>();
    const zoomAt = (px: number, py: number, factor: number) => {
      const k = Math.max(0.1, Math.min(3, this.view.k * factor));
      const f = k / this.view.k;
      this.setView(px - (px - this.view.x) * f, py - (py - this.view.y) * f, k);
    };
    el.addEventListener("pointerdown", (e) => {
      if ((e.target as Element).closest("button")) return;
      el.setPointerCapture(e.pointerId);
      touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
    });
    el.addEventListener("pointermove", (e) => {
      const last = touches.get(e.pointerId);
      if (!last) return;
      if (touches.size === 1) {
        this.setView(this.view.x + e.clientX - last.x, this.view.y + e.clientY - last.y, this.view.k);
      } else if (touches.size === 2) {
        const other = [...touches.entries()].find(([id]) => id !== e.pointerId)![1];
        const before = Math.hypot(last.x - other.x, last.y - other.y);
        const after = Math.hypot(e.clientX - other.x, e.clientY - other.y);
        if (before > 0) zoomAt((e.clientX + other.x) / 2, (e.clientY + other.y) / 2, after / before);
      }
      last.x = e.clientX; last.y = e.clientY;
    });
    const up = (e: PointerEvent) => touches.delete(e.pointerId);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
    el.addEventListener("wheel", (e) => {
      e.preventDefault();
      zoomAt(e.clientX, e.clientY, Math.exp(-e.deltaY * 0.0015));
    }, { passive: false });
  }

  // ---- frame loop ------------------------------------------------------------------------

  private tick = (ts: number): void => {
    requestAnimationFrame(this.tick);
    const t0 = performance.now();
    const rec = this.rec;
    if (rec && this.lastTs) rec.dt.push(ts - this.lastTs);
    this.lastTs = ts;
    const d = this.dirty;
    let done = false;
    if (this.animating) {
      const raw = this.animDur ? Math.min(1, (t0 - this.animStart) / this.animDur) : 1;
      const t = raw < 0.5 ? 2 * raw * raw : 1 - (2 - 2 * raw) * (1 - raw);
      for (const n of this.nodes) {
        if (!n.moving) continue;
        n.x = n.fx + (n.tx - n.fx) * t;
        n.y = n.fy + (n.ty - n.fy) * t;
      }
      for (const e of this.edges) {
        if (!e.moving) continue;
        const { pts, from, to } = e;
        for (let i = 0; i < pts.length; i++) pts[i] = from[i] + (to[i] - from[i]) * t;
      }
      d.geom = true;
      done = raw >= 1;
    }
    if (this.panFn) this.panFn(t0);
    if (this.pulses.length && this.opts.pulse !== "css") {
      for (const p of this.pulses) {
        pointAt(p.edge.pts, (t0 / PULSE_MS + p.phase) % 1, this.scratch);
        p.x = this.scratch.x; p.y = this.scratch.y;
      }
      d.pulses = true;
    }
    if (d.view || d.geom || d.pulses || d.structure) this.renderer?.frame(d);
    d.view = d.geom = d.pulses = d.structure = false;
    if (done) {
      this.animating = false;
      for (const n of this.nodes) n.moving = false;
      for (const e of this.edges) e.moving = false;
      d.structure = true; // lets the renderer refresh culling and CSS pulse paths once
    }
    // Queued after anything Solid scheduled during this frame, so its flush is counted.
    if (rec) queueMicrotask(() => rec.js.push(performance.now() - t0));
    this.fpsFrames++;
    if (!this.quiet && t0 - this.fpsSince > 500) {
      const last = this.layoutLog.at(-1);
      this.setHud(
        `${this.nodes.length} nodes · ${this.edges.length} edges · layout ${last ? Math.round(last.ms) : "–"} ms · ` +
          `${Math.round((this.fpsFrames * 1000) / (t0 - this.fpsSince))} fps · zoom ${this.view.k.toFixed(2)}`,
      );
      this.fpsFrames = 0;
      this.fpsSince = t0;
    }
  };

  // ---- benchmarks, driven by scripts/measure.mjs --------------------------------------------

  /** Runs one scenario for `ms` and returns frame statistics from requestAnimationFrame deltas. */
  async bench(name: "idle" | "pan-close" | "pan-far" | "pulses" | "grow", ms: number) {
    this.quiet = true;
    this.panFn = null;
    this.setPulses(false);
    await this.setCount(name === "grow" ? this.steps.length - 10 : this.steps.length, false);
    await frames(3);
    const k = name === "pan-close" ? 1 : this.fitK();
    const cx = this.bbox.w / 2, cy = this.bbox.h / 2;
    this.centerOn(cx, cy, k);
    if (name === "pulses") this.setPulses(true);
    await frames(10);
    if (name === "pan-close" || name === "pan-far") {
      // A Lissajous sweep: about 1500 screen px/s at its fastest, like a brisk drag.
      const ax = Math.min(200 / k, this.bbox.w / 2), ay = Math.min(500 / k, this.bbox.h / 2);
      const start = performance.now();
      this.panFn = (now) => {
        const t = (now - start) / 1000;
        this.centerOn(cx + ax * Math.sin((2 * Math.PI * t) / 3.1), cy + ay * Math.sin((2 * Math.PI * t) / 2), k);
      };
    }
    const logFrom = this.layoutLog.length;
    let timer = 0;
    if (name === "grow") timer = window.setInterval(() => void this.setCount(this.shown + 1, true), 500);
    const rec = { dt: [] as number[], js: [] as number[] };
    this.lastTs = 0;
    this.rec = rec;
    await sleep(ms);
    this.rec = null;
    clearInterval(timer);
    this.panFn = null;
    if (this.busy) await this.busy;
    const layouts = this.layoutLog.slice(logFrom);
    this.quiet = false;
    return {
      scenario: name,
      zoom: Math.round(k * 100) / 100,
      frames: rec.dt.length,
      fps: Math.round((rec.dt.length * 10000) / ms) / 10,
      frame: summary(rec.dt),
      over33Pct: Math.round((rec.dt.filter((x) => x > 33).length * 1000) / Math.max(1, rec.dt.length)) / 10,
      over20Pct: Math.round((rec.dt.filter((x) => x > 20).length * 1000) / Math.max(1, rec.dt.length)) / 10,
      js: summary(rec.js),
      layouts: layouts.length,
      layoutMs: summary(layouts.map((l) => l.ms)),
      domElements: document.querySelectorAll("*").length,
      visibleNodes: this.nodes.filter((n) => n.vis).length,
    };
  }

  /** Full and incremental layout times for one configuration, off-screen (nothing is drawn). */
  async layoutTimes(name: string, where: "worker" | "main") {
    const cfg = configByName(name);
    let elk: ElkLike | null = null;
    if (!cfg.custom) {
      if (where === this.opts.elk && this.elk) elk = this.elk;
      else if (where === "main") elk = this.elkMain ??= await createElk("main");
      else elk = this.elk ??= await createElk("worker");
    }
    const all = prefix(this.steps, this.steps.length);
    const full: number[] = [], fullElk: number[] = [], inc: number[] = [];
    for (let i = 0; i < 4; i++) {
      const t0 = performance.now();
      const out = await runLayout(elk, all.nodes, all.edges, cfg, new Map(), true);
      // The first run warms the JIT; it is reported separately.
      full.push(performance.now() - t0);
      if (out.elkMs !== undefined) fullElk.push(out.elkMs);
    }
    const base = prefix(this.steps, this.steps.length - 6);
    let prev = (await runLayout(elk, base.nodes, base.edges, cfg, new Map())).pos;
    for (let c = this.steps.length - 5; c <= this.steps.length; c++) {
      const g = prefix(this.steps, c);
      const t0 = performance.now();
      prev = (await runLayout(elk, g.nodes, g.edges, cfg, prev)).pos;
      inc.push(performance.now() - t0);
    }
    const r = (v: number) => Math.round(v * 10) / 10;
    return {
      config: name, where,
      firstMs: r(full[0]),
      fullMs: r(Math.min(...full.slice(1))),
      fullElkMs: fullElk.length > 1 ? r(Math.min(...fullElk.slice(1))) : null,
      addNodeMs: summary(inc),
    };
  }
}
