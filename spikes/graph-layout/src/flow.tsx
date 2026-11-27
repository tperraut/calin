// @ts-nocheck: a quick trial of @dschz/solid-flow 1.0.1, typed loosely on purpose.
//
// The same synthetic graph on the library's built-in node and edge types, positioned by the
// hand-written tree layout (no ELK), with the same scenarios as scene.ts so the numbers compare.
// What it does not have: ELK routes (the library draws its own beziers between handles) and
// travelling pulses ("pulses" here turns on the library's animated dashed edges instead).

import { createEdgeStore, createNodeStore, SolidFlow, useSolidFlow } from "@dschz/solid-flow";
import "@dschz/solid-flow/style.css";
import { render } from "@solidjs/web";
import { onSettled } from "solid-js";
import { generate, prefix } from "./gen.ts";
import { treeLayout } from "./treelayout.ts";

const q = new URLSearchParams(location.search);
const N = Number(q.get("n") ?? 200), E = Number(q.get("e") ?? 300);
const steps = generate(N, E);
const VW = window.innerWidth, VH = window.innerHeight;

function build(count: number, animated = false) {
  const g = prefix(steps, count);
  const out = treeLayout(g.nodes, "top");
  const nodes = g.nodes.map((n) => ({
    id: n.id,
    type: "default",
    data: { label: n.label },
    position: { ...out.pos.get(n.id)! },
    width: n.w,
    height: n.h,
    sourcePosition: "right",
    targetPosition: "left",
    draggable: false,
  }));
  let pulses = 0;
  const edges = g.edges.map((e) => ({
    id: e.id,
    source: e.from,
    target: e.to,
    animated: animated && e.kind === "message" && pulses++ < 20,
  }));
  return { nodes, edges, out };
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
function summary(xs: number[]) {
  const s = [...xs].sort((a, b) => a - b);
  const at = (p: number) => (s.length ? s[Math.min(s.length - 1, Math.floor(p * s.length))] : 0);
  const r = (v: number) => Math.round(v * 10) / 10;
  return { n: s.length, median: r(at(0.5)), p95: r(at(0.95)), max: r(s.at(-1) ?? 0) };
}

function App() {
  const first = build(N);
  let bbox = { w: first.out.w, h: first.out.h };
  const [nodes, setNodes] = createNodeStore(first.nodes);
  const [edges, setEdges] = createEdgeStore(first.edges);

  function Driver() {
    const { commands } = useSolidFlow();
    const center = (cx: number, cy: number, k: number) =>
      commands.setViewport({ x: VW / 2 - cx * k, y: VH / 2 - cy * k, zoom: k });
    const show = (count: number, animated = false) => {
      const next = build(count, animated);
      bbox = { w: next.out.w, h: next.out.h };
      setNodes(() => next.nodes);
      setEdges(() => next.edges);
      return next;
    };

    async function bench(name: string, ms: number) {
      let count = name === "grow" ? N - 10 : N;
      show(count, name === "pulses");
      await sleep(300);
      const fit = Math.max(0.2, Math.min(1.2, VW / (bbox.w + 40)));
      const k = name === "pan-close" ? 1 : fit;
      const cx = bbox.w / 2, cy = bbox.h / 2;
      center(cx, cy, k);
      await sleep(300);
      const dt: number[] = [];
      let last = 0, running = true;
      const start = performance.now();
      const ax = Math.min(200 / k, bbox.w / 2), ay = Math.min(500 / k, bbox.h / 2);
      // Position tween for "grow": the moving nodes are written into the store draft every frame.
      let tween: { from: Map<string, any>; to: Map<string, any>; t0: number } | null = null;
      const loop = (ts: number) => {
        if (!running) return;
        requestAnimationFrame(loop);
        if (last) dt.push(ts - last);
        last = ts;
        const now = performance.now();
        if (name === "pan-close" || name === "pan-far") {
          const t = (now - start) / 1000;
          center(cx + ax * Math.sin((2 * Math.PI * t) / 3.1), cy + ay * Math.sin((2 * Math.PI * t) / 2), k);
        }
        if (tween) {
          const raw = Math.min(1, (now - tween.t0) / 300);
          const tw = tween;
          setNodes((draft: any[]) => {
            for (const n of draft) {
              const a = tw.from.get(n.id), b = tw.to.get(n.id);
              if (!a || !b || (a.x === b.x && a.y === b.y)) continue;
              n.position.x = a.x + (b.x - a.x) * raw;
              n.position.y = a.y + (b.y - a.y) * raw;
            }
          });
          if (raw >= 1) tween = null;
        }
      };
      requestAnimationFrame(loop);
      let timer = 0;
      if (name === "grow") {
        timer = window.setInterval(() => {
          if (count >= N) return;
          const before = build(count);
          const next = build(++count);
          const from = new Map(before.nodes.map((n) => [n.id, n.position]));
          const to = new Map(next.nodes.map((n) => [n.id, n.position]));
          const added = next.nodes.at(-1)!;
          setNodes((draft: any[]) => void draft.push(added));
          setEdges(() => next.edges);
          tween = { from, to, t0: performance.now() };
        }, 500);
      }
      await sleep(ms);
      running = false;
      clearInterval(timer);
      return {
        scenario: name, zoom: Math.round(k * 100) / 100, frames: dt.length,
        fps: Math.round((dt.length * 10000) / ms) / 10, frame: summary(dt),
        over33Pct: Math.round((dt.filter((x) => x > 33).length * 1000) / Math.max(1, dt.length)) / 10,
        domElements: document.querySelectorAll("*").length,
      };
    }

    onSettled(() => {
      (window as any).__flow = { bench, ready: false };
      setTimeout(() => ((window as any).__flow.ready = true), 1500);
    });
    return null;
  }

  return (
    <div style="width:100vw;height:100vh">
      <SolidFlow nodes={nodes} edges={edges} fitView minZoom={0.1} colorMode="dark" nodesDraggable={false}
        onlyRenderVisibleElements={q.get("visible") === "1"}>
        <Driver />
      </SolidFlow>
    </div>
  );
}

render(() => <App />, document.getElementById("root")!);
