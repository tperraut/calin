// Turns results/*.json into the Markdown tables used in README.md:  node scripts/report.mjs > results/tables.md

import { existsSync, readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const dir = fileURLToPath(new URL("../results/", import.meta.url));
const load = (name) => (existsSync(dir + name) ? JSON.parse(readFileSync(dir + name, "utf8")) : []);
const table = (head, rows) =>
  [`| ${head.join(" | ")} |`, `| ${head.map(() => "---").join(" | ")} |`, ...rows.map((r) => `| ${r.join(" | ")} |`)].join("\n");
const SCENARIOS = ["pan-close", "pan-far", "pulses", "grow"];

for (const file of readdirSync(dir).filter((f) => f.startsWith("render-")).sort((a, b) => parseInt(a.slice(7)) - parseInt(b.slice(7)))) {
  const rows = load(file);
  const keys = [...new Set(rows.map((r) => `${r.variant}|${r.rate}|${r.raster}`))];
  const cell = (key, scenario, f) => {
    const [variant, rate, raster] = key.split("|");
    const r = rows.find((x) => x.variant === variant && String(x.rate) === rate && x.raster === raster && x.scenario === scenario);
    return r ? f(r) : "–";
  };
  const label = (key) => {
    const [variant, rate, raster] = key.split("|");
    return [variant, `${rate}x`, raster];
  };
  console.log(`\n### ${rows[0].n} nodes / ${rows[0].e} edges: frame time (median / p95 ms, % of frames over 33 ms)\n`);
  console.log(table(["renderer", "CPU", "raster", ...SCENARIOS],
    keys.map((k) => [...label(k), ...SCENARIOS.map((s) => cell(k, s, (r) => `${r.frame.median} / ${r.frame.p95}, ${r.over33Pct}%`))])));
  console.log(`\n### ${rows[0].n} nodes / ${rows[0].e} edges: main-thread time per frame (ms: total task, of which script / style / layout)\n`);
  console.log(table(["renderer", "CPU", "raster", ...SCENARIOS, "DOM elements", "JS heap MB"],
    keys.map((k) => [...label(k),
      ...SCENARIOS.map((s) => cell(k, s, (r) => `${r.taskMsPerFrame} (${r.scriptMsPerFrame} / ${r.styleMsPerFrame} / ${r.layoutMsPerFrame})`)),
      cell(k, "pan-far", (r) => r.domElements), cell(k, "grow", (r) => r.heapMb)])));
  const grow = rows.filter((r) => r.scenario === "grow" && r.layouts);
  if (grow.length) {
    console.log(`\nLayouts during "grow" (${rows[0].n} nodes): ` +
      [...new Set(grow.map((r) => `${r.variant} ${r.rate}x: ${r.layouts} layouts, median ${r.layoutMs.median} ms`))].slice(0, 4).join("; "));
  }
  const cold = rows.filter((r) => r.scenario === "idle" && r.coldMs);
  console.log(`\nCold start to first layout applied (worker spawn + ELK compile + layout), ms: ` +
    cold.map((r) => `${r.variant} ${r.rate}x ${r.raster}: ${r.coldMs}`).join("; "));
}

const layout = load("layout.json");
if (layout.length) {
  console.log(`\n### Layout time (ms)\n`);
  console.log(table(["nodes / edges", "configuration", "where", "main-thread CPU", "first run", "full layout (best of 3 warm)", "add one node (median / max of 6)"],
    layout.map((r) => [`${r.n} / ${r.e}`, r.config, r.where, `${r.rate}x`, r.firstMs, r.fullMs, `${r.addNodeMs.median} / ${r.addNodeMs.max}`])));
}

console.log(`\n### Stability: displacement of existing nodes per added node (px)\n`);
const stab = readdirSync(dir).filter((f) => /^stability-\d+-\d+(-.*)?\.json$/.test(f)).sort((a, b) => parseInt(a.slice(10)) - parseInt(b.slice(10)));
const seen = new Set();
const srows = [];
for (const f of stab) {
  const [, n, e] = f.match(/^stability-(\d+)-(\d+)/);
  if (Number(n) < 50) continue;
  for (const r of load(f)) {
    if (seen.has(`${n}|${r.config}`)) continue;
    seen.add(`${n}|${r.config}`);
    srows.push([`${n} / ${e}`, r.config, r.rawMeanPx, r.compMeanPx, r.compMaxMeanPx, r.compMaxP95Px, r.compMaxWorstPx, `${r.movedPct}%`, `${r.stepsQuietPct}%`, r.layoutMsMean, r.bbox, r.overlaps]);
  }
}
console.log(table(["nodes / edges", "configuration", "mean", "mean, pan-compensated", "max per step: mean", "max per step: p95", "max per step: worst", "nodes moved > 2 px", "steps with no movement", "layout ms (Node, mean)", "final size", "overlapping pairs"], srows));
