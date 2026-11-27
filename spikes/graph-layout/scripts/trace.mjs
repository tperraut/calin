// Where the time goes per frame, from a Chrome trace: main-thread phases (script, style,
// layout, pre-paint, paint, layerize, commit) and raster-thread work, which requestAnimationFrame
// deltas and CDP CPU throttling do not see.
//
//   node scripts/trace.mjs [n] [e] [rate] [variant,variant,...]
//
// Appends to results/trace.json. Launch code is a copy of measure.mjs's (throwaway spike).

import { spawn, spawnSync } from "node:child_process";
import { createReadStream, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer as createHttpServer } from "node:http";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const DIST = join(ROOT, "dist");
const VARIANTS = {
  "svg-solid": "r=svg&upd=solid",
  "svg-dom": "r=svg&upd=dom",
  "svg-dom-cull": "r=svg&upd=dom&cull=1",
  "svg-dom-lod": "r=svg&upd=dom&lod=1",
  "svg-dom-pancss": "r=svg&upd=dom&pan=css",
  "svg-dom-pulsecss": "r=svg&upd=dom&pulse=css",
  "svg-dom-pulsecanvas": "r=svg&upd=dom&pulse=canvas",
  "svg-best": "r=svg&upd=dom&cull=1&lod=1&pulse=canvas",
  "svg-cull-lod": "r=svg&upd=dom&cull=1&lod=1",
  "canvas-lod": "r=canvas&lod=1",
};
const SCENARIOS = [["pan-close", 3000], ["pan-far", 3000], ["pulses", 3000], ["grow", 5500]];
const MAIN = { script: ["FireAnimationFrame", "RunMicrotasks", "TimerFire", "FunctionCall"], style: ["UpdateLayoutTree"], layout: ["Layout"],
  prepaint: ["PrePaint"], paint: ["Paint"], layerize: ["Layerize"], commit: ["Commit"] };

function freePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

function browserCommand() {
  const override = process.env.CALIN_E2E_BROWSER;
  if (override) return override.split(" ").filter(Boolean);
  for (const name of ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "brave-browser"]) {
    if (spawnSync("sh", ["-c", `command -v ${name}`]).status === 0) return [name];
  }
  return ["flatpak", "run", "--filesystem=home", "--filesystem=/tmp", "com.brave.Browser"];
}

const [nArg = "200", eArg = "300", rateArg = "4", only = "svg-dom,svg-dom-cull,svg-dom-lod,svg-dom-pancss,svg-cull-lod,canvas-lod"] = process.argv.slice(2);
const n = Number(nArg), e = Number(eArg), rate = Number(rateArg);
const scratch = mkdtempSync(join(tmpdir(), "calin-spike-"));
const port = await freePort();
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css" };
const server = createHttpServer((req, res) => {
  let file = join(DIST, normalize(new URL(req.url, "http://x").pathname));
  if (!file.startsWith(DIST) || !existsSync(file) || statSync(file).isDirectory()) file = join(DIST, "index.html");
  res.writeHead(200, { "content-type": TYPES[extname(file)] ?? "application/octet-stream" });
  createReadStream(file).pipe(res);
});
await new Promise((r) => server.listen(port, "127.0.0.1", r));
const debugPort = await freePort();
const command = browserCommand();
const child = spawn(command[0], [...command.slice(1), "--headless=new", `--remote-debugging-port=${debugPort}`,
  `--user-data-dir=${join(scratch, "profile")}`, "about:blank"], { stdio: "ignore", detached: true });
let browser;
try {
  for (let i = 0; ; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${debugPort}/json/version`)).ok) break;
    } catch {
      if (i > 150) throw new Error("the browser did not start");
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${debugPort}`);
  const rows = [];
  for (const variant of only.split(",")) {
    const context = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true });
    const page = await context.newPage();
    const cdp = await context.newCDPSession(page);
    if (rate > 1) await cdp.send("Emulation.setCPUThrottlingRate", { rate });
    await page.goto(`http://127.0.0.1:${port}/?n=${n}&e=${e}&${VARIANTS[variant]}`);
    await page.waitForFunction(() => window.__scene?.ready, null, { timeout: 180000 });
    for (const [scenario, ms] of SCENARIOS) {
      await browser.startTracing(page, { categories: ["cc", "viz", "blink", "devtools.timeline", "disabled-by-default-devtools.timeline", "disabled-by-default-devtools.timeline.frame"] });
      const result = await page.evaluate(([s, t]) => window.__scene.bench(s, t), [scenario, ms]);
      const trace = JSON.parse((await browser.stopTracing()).toString("utf8"));
      const events = trace.traceEvents ?? trace;
      // The renderer of this page is the process that fired the most animation frames.
      const counts = new Map();
      for (const ev of events) if (ev.name === "FireAnimationFrame") counts.set(ev.pid, (counts.get(ev.pid) ?? 0) + 1);
      const pid = [...counts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0];
      const mainTid = events.find((ev) => ev.pid === pid && ev.name === "FireAnimationFrame")?.tid;
      const sum = (names, tidOk) => {
        let total = 0;
        for (const ev of events) {
          if (ev.ph === "X" && ev.pid === pid && tidOk(ev.tid) && names.includes(ev.name)) total += ev.dur ?? 0;
        }
        return total / 1000;
      };
      const frames = Math.max(1, result.frames);
      const per = (v) => Math.round((v / frames) * 100) / 100;
      const row = { n, e, variant, rate, scenario, frames: result.frames, frameMedian: result.frame.median, frameP95: result.frame.p95, over33Pct: result.over33Pct };
      for (const [key, names] of Object.entries(MAIN)) row[`${key}Ms`] = per(sum(names, (tid) => tid === mainTid));
      // Raster runs on compositor worker threads (software here), outside the CPU throttle.
      row.rasterMs = per(sum(["RasterTask"], (tid) => tid !== mainTid));
      row.rasterTasks = events.filter((ev) => ev.ph === "X" && ev.pid === pid && ev.name === "RasterTask").length;
      // Frames the compositor actually produced, from any process (the display compositor is in the GPU process).
      row.drawFrames = events.filter((ev) => ev.name === "DrawFrame" && ev.pid === pid).length;
      rows.push(row);
      console.log(JSON.stringify(row));
    }
    await context.close();
  }
  const path = join(ROOT, "results", "trace.json");
  mkdirSync(join(ROOT, "results"), { recursive: true });
  const all = existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) : [];
  writeFileSync(path, JSON.stringify([...all, ...rows], null, 1));
} finally {
  try {
    const session = await browser?.newBrowserCDPSession();
    await session?.send("Browser.close");
  } catch {
    // already gone
  }
  try {
    process.kill(-child.pid, "SIGTERM");
  } catch {
    // already gone
  }
  server.close();
  await new Promise((r) => setTimeout(r, 1500));
  rmSync(scratch, { recursive: true, force: true });
}
process.exit(0);
