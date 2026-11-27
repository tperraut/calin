// Starts the real server and a headless browser, and gives scenarios a small API.
//
// The server runs with CALIN_DRY_RUN=1 on a free port with a throwaway data
// directory, so nothing here needs an agent login or touches ~/.calin.

import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, mkdirSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { chromium } from "playwright-core";

export const PHONE = { width: 390, height: 844 };
export const DESKTOP = { width: 1440, height: 900 };

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

async function waitFor(url, what, timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // not up yet
    }
    if (Date.now() > deadline) throw new Error(`${what} did not answer at ${url}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

/** The command that launches a Chromium-family browser, or null when none is installed. */
function browserCommand() {
  const override = process.env.CALIN_E2E_BROWSER;
  if (override) return override.split(" ").filter(Boolean);
  for (const name of ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "brave-browser"]) {
    if (spawnSync("sh", ["-c", `command -v ${name}`]).status === 0) return [name];
  }
  // A Flatpak browser cannot read the profile directory unless it is granted.
  for (const app of ["com.brave.Browser", "org.chromium.Chromium", "com.google.Chrome"]) {
    if (spawnSync("flatpak", ["info", app]).status === 0) {
      return ["flatpak", "run", "--filesystem=home", "--filesystem=/tmp", app];
    }
  }
  return null;
}

export class NoBrowser extends Error {}

export async function start({ binary, shots }) {
  const command = browserCommand();
  if (!command) throw new NoBrowser("no Chromium-family browser found; set CALIN_E2E_BROWSER");

  const scratch = mkdtempSync(join(tmpdir(), "calin-e2e-"));
  mkdirSync(shots, { recursive: true });
  const children = [];
  const stop = () => {
    for (const child of children) {
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {
        // already gone
      }
    }
    rmSync(scratch, { recursive: true, force: true });
  };

  try {
    const port = await freePort();
    const base = `http://127.0.0.1:${port}`;
    const server = spawn(binary, ["serve", "--home", join(scratch, "home"), "--bind", `127.0.0.1:${port}`], {
      env: { ...process.env, CALIN_DRY_RUN: "1", CALIN_MOCK_DELAY_MS: process.env.CALIN_MOCK_DELAY_MS ?? "30" },
      stdio: "ignore",
      detached: true,
    });
    children.push(server);
    await waitFor(`${base}/api/v1/health`, "the calin server");

    const debugPort = await freePort();
    const browserProcess = spawn(
      command[0],
      [
        ...command.slice(1),
        "--headless=new",
        "--disable-gpu",
        `--remote-debugging-port=${debugPort}`,
        `--user-data-dir=${join(scratch, "profile")}`,
        "about:blank",
      ],
      { stdio: "ignore", detached: true },
    );
    children.push(browserProcess);
    await waitFor(`http://127.0.0.1:${debugPort}/json/version`, "the browser");
    const browser = await chromium.connectOverCDP(`http://127.0.0.1:${debugPort}`);

    const results = [];
    const check = (name, ok, detail = "") => {
      results.push({ name, ok: Boolean(ok), detail });
      console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? `  ${detail}` : ""}`);
    };

    /** A fresh page at a viewport. Console errors and uncaught exceptions are collected. */
    async function open(viewport, label) {
      const context = await browser.newContext({ viewport, deviceScaleFactor: 2 });
      const page = await context.newPage();
      const errors = [];
      page.on("console", (message) => {
        if (message.type() === "error") errors.push(message.text());
      });
      page.on("pageerror", (error) => errors.push(String(error)));
      const shot = (name) => page.screenshot({ path: join(shots, `${label}-${name}.png`) });
      const bodyHas = (text, timeout = 30000) =>
        page.waitForFunction((t) => document.body.innerText.includes(t), text, { timeout });
      return { page, errors, shot, bodyHas };
    }

    /** Calls the server's REST API directly, for setup and for verifying what the UI did. */
    async function api(method, path, body) {
      const response = await fetch(`${base}/api/v1${path}`, {
        method,
        headers: body === undefined ? {} : { "content-type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      const text = await response.text();
      return { status: response.status, body: text ? JSON.parse(text) : null };
    }

    /**
     * A throwaway git clone with one commit. By default it lives under `repos/`
     * and is registered with the server; pass `under` to put it elsewhere and
     * leave it unregistered, for the server to discover.
     */
    async function repo(name, files, { under } = {}) {
      const dir = join(scratch, under ?? "repos", name);
      mkdirSync(dir, { recursive: true });
      for (const [file, content] of Object.entries(files)) {
        mkdirSync(join(dir, file, ".."), { recursive: true });
        writeFileSync(join(dir, file), content);
      }
      const git = (...args) => {
        const result = spawnSync("git", args, {
          cwd: dir,
          env: {
            ...process.env,
            GIT_CONFIG_GLOBAL: "/dev/null",
            GIT_CONFIG_SYSTEM: "/dev/null",
            GIT_AUTHOR_NAME: "e2e",
            GIT_AUTHOR_EMAIL: "e2e@example.invalid",
            GIT_COMMITTER_NAME: "e2e",
            GIT_COMMITTER_EMAIL: "e2e@example.invalid",
          },
        });
        if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${result.stderr}`);
      };
      git("init", "-q", "-b", "main");
      git("add", "-A");
      git("commit", "-q", "-m", "initial");
      if (under !== undefined) return { name, path: dir };
      const registered = await api("POST", "/repos", { path: dir });
      if (registered.status !== 201) throw new Error(`could not register ${name}: ${JSON.stringify(registered.body)}`);
      return registered.body;
    }

    return {
      base,
      scratch,
      api,
      repo,
      check,
      open,
      results,
      async close() {
        await browser.close().catch(() => {});
        stop();
      },
    };
  } catch (error) {
    stop();
    throw error;
  }
}
