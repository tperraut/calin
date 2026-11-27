// Replays the synthetic session one node at a time through each ELK configuration and
// measures how far the nodes that were already there move. Runs in Node (no browser):
//   node scripts/stability.ts [nodes] [edges] [config,config,...]
// Displacement is deterministic, so it does not need the phone setup; timings here are
// desktop Node timings and are only useful relative to one another.

import { writeFileSync, mkdirSync } from "node:fs";
import ELK from "elkjs/lib/elk.bundled.js";
import { CONFIGS, runLayout, type LayoutOut, type Pos } from "../src/elkopts.ts";
import { generate, prefix } from "../src/gen.ts";

const nNodes = Number(process.argv[2] ?? 200);
const nEdges = Number(process.argv[3] ?? 300);
const only = process.argv[4]?.split(",");
const steps = generate(nNodes, nEdges);
const elk = new ELK();

const median = (xs: number[]) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? s[Math.floor(s.length / 2)] : 0;
};
const mean = (xs: number[]) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);
const pct = (xs: number[], p: number) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? s[Math.min(s.length - 1, Math.floor(p * s.length))] : 0;
};
const r1 = (x: number) => Math.round(x * 10) / 10;

const rows = [];
for (const config of CONFIGS) {
  if (only && !only.includes(config.name)) continue;
  let prev = new Map<string, Pos>();
  const rawMean: number[] = [], rawMax: number[] = [], compMean: number[] = [], compMax: number[] = [];
  const moved: number[] = [], times: number[] = [];
  let failed = "";
  let last: LayoutOut | undefined;
  let lastNodes: { id: string; w: number; h: number }[] = [];
  for (let i = 1; i <= steps.length; i++) {
    const { nodes, edges } = prefix(steps, i);
    const t0 = performance.now();
    try {
      last = await runLayout(elk, nodes, edges, config, prev);
      lastNodes = nodes;
    } catch (error) {
      failed = `step ${i}: ${String(error).slice(0, 120)}`;
      break;
    }
    times.push(performance.now() - t0);
    const next: Map<string, Pos> = last!.pos;
    const dx: number[] = [], dy: number[] = [];
    for (const [id, p] of prev) {
      const q = next.get(id)!;
      dx.push(q.x - p.x);
      dy.push(q.y - p.y);
    }
    if (dx.length >= 2) {
      // "Compensated" removes the one translation the viewport could absorb by panning with the graph.
      const mx = median(dx), my = median(dy);
      const raw = dx.map((d, k) => Math.hypot(d, dy[k]));
      const comp = dx.map((d, k) => Math.hypot(d - mx, dy[k] - my));
      rawMean.push(mean(raw));
      rawMax.push(Math.max(...raw));
      compMean.push(mean(comp));
      compMax.push(Math.max(...comp));
      moved.push(comp.filter((d) => d > 2).length / comp.length);
    }
    prev = next;
  }
  // Quality of the final picture: size, overlapping node pairs, total routed edge length.
  let overlaps = 0, edgeLen = 0;
  const w = last?.w ?? 0, h = last?.h ?? 0;
  if (last) {
    const cs = lastNodes.map((n) => ({ ...last!.pos.get(n.id)!, width: n.w, height: n.h }));
    for (let a = 0; a < cs.length; a++) {
      for (let b = a + 1; b < cs.length; b++) {
        if (cs[a].x < cs[b].x + cs[b].width && cs[b].x < cs[a].x + cs[a].width &&
            cs[a].y < cs[b].y + cs[b].height && cs[b].y < cs[a].y + cs[a].height) overlaps++;
      }
    }
    for (const pts of last.routes.values()) {
      for (let k = 2; k < pts.length; k += 2) edgeLen += Math.hypot(pts[k] - pts[k - 2], pts[k + 1] - pts[k - 1]);
    }
  }
  const row = {
    config: config.name,
    failed,
    rawMeanPx: r1(mean(rawMean)),
    rawMaxMeanPx: r1(mean(rawMax)),
    compMeanPx: r1(mean(compMean)),
    compMaxMeanPx: r1(mean(compMax)),
    compMaxP95Px: r1(pct(compMax, 0.95)),
    compMaxWorstPx: r1(Math.max(0, ...compMax)),
    movedPct: r1(mean(moved) * 100),
    stepsQuietPct: r1((compMax.filter((d) => d <= 2).length / Math.max(1, compMax.length)) * 100),
    layoutMsMean: r1(mean(times)),
    layoutMsLast: r1(times.at(-1) ?? 0),
    bbox: `${Math.round(w)}x${Math.round(h)}`,
    overlaps,
    routedEdgeLenK: r1(edgeLen / 1000),
  };
  rows.push(row);
  console.error(JSON.stringify(row));
}
console.table(rows);
mkdirSync(new URL("../results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../results/stability-${nNodes}-${nEdges}${only ? "-" + only.join("+") : ""}.json`, import.meta.url), JSON.stringify(rows, null, 1));
