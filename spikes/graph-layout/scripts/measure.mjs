// Measures the built spike (run `vp build` first) in a headless Chromium-family browser at a
// phone viewport, unthrottled and with CDP CPU throttling.
//
//   node scripts/measure.mjs smoke                      one page per renderer, screenshots, sanity checks
//   node scripts/measure.mjs render <n> <e> <rate> [variants]   frame times for every renderer variant
//   node scripts/measure.mjs layout <n> <e> <rate>      ELK layout times, worker and main thread
//
// Results are appended to results/*.json. The browser is the one e2e/harness.mjs finds.

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
const RESULTS = join(ROOT, "results");
const SHOTS = join(ROOT, "shots");
const PHONE = { width: 390, height: 844 };

const VARIANTS = {
  // The design read literally: Solid signals drive every per-frame change.
  "svg-solid": "r=svg&upd=solid",
  "svg-dom": "r=svg&upd=dom",
  "svg-dom-cull": "r=svg&upd=dom&cull=1",
  "svg-dom-lod": "r=svg&upd=dom&lod=1",
  "svg-dom-pancss": "r=svg&upd=dom&pan=css",
  "svg-dom-pulsecss": "r=svg&upd=dom&pulse=css",
  "svg-dom-pulsecanvas": "r=svg&upd=dom&pulse=canvas",
  "svg-best": "r=svg&upd=dom&cull=1&lod=1&pulse=canvas",
  canvas: "r=canvas",
  "canvas-lod": "r=canvas&lod=1",
  "canvas-lod-2layer": "r=canvas&lod=1&layers=2",
  // Backing store at 2x and 1x instead of the phone's 3x: tells whether the canvas cost is per pixel.
  "canvas-lod-dpr2": "r=canvas&lod=1&cdpr=2",
  "canvas-lod-dpr1": "r=canvas&lod=1&cdpr=1",
};
const SCENARIOS = [["idle", 1500], ["pan-close", 3500], ["pan-far", 3500], ["pulses", 3500], ["grow", 5500]];
const LAYOUT_CONFIGS = ["layered-default", "tree-order-noforce", "mrtree-tree", "custom-tree-top"];

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

async function waitFor(url, what, timeoutMs = 30000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      // not up yet
    }
    if (Date.now() > deadline) throw new Error(`${what} did not answer at ${url}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

function browserCommand() {
  const override = process.env.CALIN_E2E_BROWSER;
  if (override) return override.split(" ").filter(Boolean);
  for (const name of ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "brave-browser"]) {
    if (spawnSync("sh", ["-c", `command -v ${name}`]).status === 0) return [name];
  }
  for (const app of ["com.brave.Browser", "org.chromium.Chromium", "com.google.Chrome"]) {
    if (spawnSync("flatpak", ["info", app]).status === 0) {
      return ["flatpak", "run", "--filesystem=home", "--filesystem=/tmp", app];
    }
  }
  throw new Error("no Chromium-family browser found; set CALIN_E2E_BROWSER");
}

const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml" };

async function serveDist() {
  if (!existsSync(join(DIST, "index.html"))) throw new Error("dist/ is missing: run `vp build` first");
  const port = await freePort();
  const server = createHttpServer((req, res) => {
    const path = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname));
    let file = join(DIST, path);
    if (!file.startsWith(DIST) || !existsSync(file) || statSync(file).isDirectory()) file = join(DIST, "index.html");
    res.writeHead(200, { "content-type": TYPES[extname(file)] ?? "application/octet-stream" });
    createReadStream(file).pipe(res);
  });
  await new Promise((r) => server.listen(port, "127.0.0.1", r));
  return { base: `http://127.0.0.1:${port}`, close: () => server.close() };
}

/** A second, raw CDP connection to the page: Playwright does not expose worker sessions. */
async function workerControl(debugPort, targetId, rate) {
  const ws = new WebSocket(`ws://127.0.0.1:${debugPort}/devtools/page/${targetId}`);
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = () => reject(new Error("raw CDP socket failed"));
  });
  let id = 0;
  const pending = new Map();
  const workers = [];
  const send = (method, params = {}, sessionId) =>
    new Promise((resolve) => {
      pending.set(++id, resolve);
      ws.send(JSON.stringify({ id, method, params, sessionId }));
    });
  ws.onmessage = (event) => {
    const message = JSON.parse(event.data);
    if (message.id && pending.has(message.id)) {
      pending.get(message.id)(message);
      pending.delete(message.id);
    } else if (message.method === "Target.attachedToTarget") {
      const { sessionId, targetInfo } = message.params;
      const worker = { sessionId, type: targetInfo.type, url: targetInfo.url, throttled: null };
      workers.push(worker);
      (async () => {
        if (rate > 1) {
          const reply = await send("Emulation.setCPUThrottlingRate", { rate }, sessionId);
          worker.throttled = reply.error ? `error: ${reply.error.message}` : "ok";
        }
        await send("Runtime.runIfWaitingForDebugger", {}, sessionId);
      })();
    }
  };
  await send("Target.setAutoAttach", { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
  return {
    workers,
    async heapMb() {
      let total = 0;
      for (const w of workers) {
        const reply = await send("Runtime.getHeapUsage", {}, w.sessionId);
        total += reply.result?.usedSize ?? 0;
      }
      return Math.round(total / 1e5) / 10;
    },
    close: () => ws.close(),
  };
}

async function main() {
  // `extra` is appended to every page's query, e.g. cfg=custom-tree-top to take ELK out of a run.
  const [mode = "smoke", nArg = "200", eArg = "300", rateArg = "1", only, extra = ""] = process.argv.slice(2);
  const n = Number(nArg), e = Number(eArg), rate = Number(rateArg);
  mkdirSync(RESULTS, { recursive: true });
  mkdirSync(SHOTS, { recursive: true });
  const scratch = mkdtempSync(join(tmpdir(), "calin-spike-"));
  const site = await serveDist();
  const debugPort = await freePort();
  const command = browserCommand();
  const gpuFlags = process.env.SPIKE_DISABLE_GPU ? ["--disable-gpu"] : [];
  const child = spawn(
    command[0],
    [...command.slice(1), "--headless=new", ...gpuFlags, `--remote-debugging-port=${debugPort}`,
      `--user-data-dir=${join(scratch, "profile")}`, "about:blank"],
    { stdio: "ignore", detached: true },
  );
  let browser;
  const stop = async () => {
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
    site.close();
    await new Promise((r) => setTimeout(r, 1500));
    rmSync(scratch, { recursive: true, force: true });
  };

  try {
    await waitFor(`http://127.0.0.1:${debugPort}/json/version`, "the browser");
    const version = await (await fetch(`http://127.0.0.1:${debugPort}/json/version`)).json();
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${debugPort}`);

    /** Opens the page with a query string at phone size, throttled before anything loads. */
    const open = async (query, path = "/", ready = "__scene") => {
      const context = await browser.newContext({ viewport: PHONE, deviceScaleFactor: 3, isMobile: true, hasTouch: true });
      const page = await context.newPage();
      const errors = [];
      page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
      page.on("pageerror", (error) => errors.push(String(error)));
      const cdp = await context.newCDPSession(page);
      const { targetInfo } = await cdp.send("Target.getTargetInfo");
      const workers = await workerControl(debugPort, targetInfo.targetId, rate);
      await cdp.send("Performance.enable");
      if (rate > 1) await cdp.send("Emulation.setCPUThrottlingRate", { rate });
      await page.goto(`${site.base}${path}?n=${n}&e=${e}&${query}&${extra}`);
      await page.waitForFunction((name) => window[name]?.ready, ready, { timeout: 180000 });
      const metrics = async () => {
        const { metrics } = await cdp.send("Performance.getMetrics");
        return Object.fromEntries(metrics.map((m) => [m.name, m.value]));
      };
      return {
        page, errors, workers, metrics,
        async close() {
          workers.close();
          await context.close();
        },
      };
    };

    const append = (file, rows) => {
      const path = join(RESULTS, file);
      const all = existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) : [];
      all.push(...rows);
      writeFileSync(path, JSON.stringify(all, null, 1));
    };
    const r1 = (v) => Math.round(v * 10) / 10;

    if (mode === "smoke") {
      for (const variant of ["svg-dom", "canvas"]) {
        const run = await open(VARIANTS[variant]);
        const info = await run.page.evaluate(() => {
          const s = window.__scene;
          const gl = document.createElement("canvas").getContext("webgl");
          const ext = gl?.getExtension("WEBGL_debug_renderer_info");
          return {
            nodes: s.nodes.length, edges: s.edges.length, coldMs: Math.round(s.coldMs), bbox: s.bbox, zoom: s.view.k,
            dpr: devicePixelRatio, size: [innerWidth, innerHeight], dom: document.querySelectorAll("*").length,
            gpu: ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : "no webgl",
          };
        });
        await run.page.screenshot({ path: join(SHOTS, `${variant}-${n}-overview.png`), scale: "css" });
        await run.page.evaluate(async () => {
          const s = window.__scene;
          const lead = s.nodes.find((x) => x.g.id === "a0");
          s.centerOn(lead.x + 330, lead.y + 60, 1);
          s.setPulses(true);
          await new Promise((r) => setTimeout(r, 400));
        });
        await run.page.screenshot({ path: join(SHOTS, `${variant}-${n}-close.png`), scale: "css" });
        const idle = await run.page.evaluate(() => window.__scene.bench("idle", 1500));
        const layout = await run.page.evaluate(() => window.__scene.layoutTimes("layered-default", "worker"));
        const layoutMain = await run.page.evaluate(() => window.__scene.layoutTimes("layered-default", "main"));
        console.log(JSON.stringify({ variant, rate, browser: version.Browser, info, idle: idle.frame, fps: idle.fps,
          workers: run.workers.workers, workerHeapMb: await run.workers.heapMb(), layout, layoutMain, errors: run.errors }, null, 1));
        await run.close();
      }
    } else if (mode === "render") {
      const names = only ? only.split(",") : Object.keys(VARIANTS);
      for (const variant of names) {
        const run = await open(VARIANTS[variant]);
        const coldMs = await run.page.evaluate(() => Math.round(window.__scene.coldMs));
        const rows = [];
        for (const [scenario, ms] of SCENARIOS) {
          const before = await run.metrics();
          const result = await run.page.evaluate(([s, t]) => window.__scene.bench(s, t), [scenario, ms]);
          const after = await run.metrics();
          const per = (key) => r1(((after[key] - before[key]) * 1000) / Math.max(1, result.frames));
          const row = {
            n, e, variant, rate, raster: gpuFlags.length ? "software" : "gpu", layout: extra.replace("cfg=", "") || "tree-order-noforce", coldMs, ...result,
            // Main-thread time per frame over the whole scenario, in (throttled) milliseconds.
            scriptMsPerFrame: per("ScriptDuration"),
            styleMsPerFrame: per("RecalcStyleDuration"),
            layoutMsPerFrame: per("LayoutDuration"),
            taskMsPerFrame: per("TaskDuration"),
            heapMb: r1(after.JSHeapUsedSize / 1e6),
            workerHeapMb: await run.workers.heapMb(),
            domNodes: after.Nodes,
            errors: run.errors.length,
          };
          rows.push(row);
          console.log(`${variant} x${rate} ${scenario}: median ${row.frame.median} p95 ${row.frame.p95} ms, ` +
            `>33ms ${row.over33Pct}%, fps ${row.fps}, task ${row.taskMsPerFrame} ms/frame`);
        }
        if (run.errors.length) console.log(`  errors: ${run.errors.slice(0, 3).join(" | ")}`);
        append(`render-${n}-${e}.json`, rows);
        await run.close();
      }
    } else if (mode === "layout") {
      const run = await open("r=canvas&cfg=custom-tree-top");
      const rows = [];
      for (const config of LAYOUT_CONFIGS) {
        for (const where of ["worker", "main"]) {
          if (config === "custom-tree-top" && where === "worker") continue;
          const result = await run.page.evaluate(([c, w]) => window.__scene.layoutTimes(c, w), [config, where]);
          const row = { n, e, rate, ...result, workerThrottle: run.workers.workers.map((w) => w.throttled).join(",") };
          rows.push(row);
          console.log(JSON.stringify(row));
        }
      }
      console.log(`worker heap ${await run.workers.heapMb()} MB; errors: ${run.errors.join(" | ") || "none"}`);
      append("layout.json", rows);
      await run.close();
    } else if (mode === "gpuinfo") {
      // What the browser says about its own acceleration, to qualify the canvas numbers.
      const context = await browser.newContext({ viewport: PHONE, deviceScaleFactor: 3 });
      const page = await context.newPage();
      await page.goto("chrome://gpu");
      await page.waitForTimeout(1500);
      const text = await page.evaluate(() => {
        const collect = (root) => [...root.querySelectorAll("*")].map((el) => (el.shadowRoot ? collect(el.shadowRoot) : "")).join("\n") + "\n" + (root.textContent ?? "");
        return collect(document);
      });
      const found = text.match(/(Canvas|Rasterization|Compositing|WebGL2?|Skia Graphite|Direct Rendering Display Compositor)\s*:?\s*(Hardware accelerated[^\n<]{0,40}|Software only[^\n<]{0,60}|Enabled|Disabled|Unavailable)/g) ?? [];
      console.log(`${version.Browser} headless=new${gpuFlags.length ? " --disable-gpu" : ""}: ` + ([...new Set(found.map((l) => l.replace(/\s+/g, " ")))].join("; ") || "no feature status found"));
      await context.close();
    } else if (mode === "flow") {
      // The library trial: @dschz/solid-flow with its built-in nodes and edges (src/flow.tsx).
      for (const [variant, query] of [["solid-flow", ""], ["solid-flow-visible-only", "visible=1"]]) {
        const run = await open(query, "/flow.html", "__flow");
        const rows = [];
        for (const [scenario, ms] of SCENARIOS) {
          const before = await run.metrics();
          const result = await run.page.evaluate(([s, t]) => window.__flow.bench(s, t), [scenario, ms]);
          const after = await run.metrics();
          const per = (key) => r1(((after[key] - before[key]) * 1000) / Math.max(1, result.frames));
          const row = { n, e, variant, rate, raster: gpuFlags.length ? "software" : "gpu", ...result, scriptMsPerFrame: per("ScriptDuration"),
            styleMsPerFrame: per("RecalcStyleDuration"), layoutMsPerFrame: per("LayoutDuration"),
            taskMsPerFrame: per("TaskDuration"), heapMb: r1(after.JSHeapUsedSize / 1e6), domNodes: after.Nodes, errors: run.errors.length };
          rows.push(row);
          console.log(`${variant} x${rate} ${scenario}: median ${row.frame.median} p95 ${row.frame.p95} ms, ` +
            `>33ms ${row.over33Pct}%, fps ${row.fps}, task ${row.taskMsPerFrame} ms/frame, dom ${row.domElements}`);
        }
        if (run.errors.length) console.log(`  errors: ${run.errors.slice(0, 3).join(" | ")}`);
        if (variant === "solid-flow") {
          await run.page.evaluate(() => window.__flow.bench("idle", 300));
          await run.page.screenshot({ path: join(SHOTS, `solid-flow-${n}-overview.png`), scale: "css" });
        }
        append(`render-${n}-${e}.json`, rows);
        await run.close();
      }
    } else {
      throw new Error(`unknown mode ${mode}`);
    }
  } finally {
    await stop();
  }
}

main().then(
  () => process.exit(0),
  (error) => {
    console.error(error);
    process.exit(1);
  },
);
