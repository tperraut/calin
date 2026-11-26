// Usage: node run.mjs <path to calin binary> [screenshot dir]
// Exit codes: 0 every check passed, 1 a check failed or the run broke, 2 no browser available.

import { resolve } from "node:path";
import { DESKTOP, NoBrowser, PHONE, start } from "./harness.mjs";

const binary = resolve(process.argv[2] ?? "../target/debug/calin");
const shots = resolve(process.argv[3] ?? "./shots");

let env;
try {
  env = await start({ binary, shots });
} catch (error) {
  if (error instanceof NoBrowser) {
    console.log(`\nE2E SKIPPED: ${error.message}\nThis is not a pass.\n`);
    process.exit(2);
  }
  throw error;
}

const PROMPT = 'textarea[placeholder="Describe what the agent should do"]';
const MESSAGE = 'textarea[placeholder="Message the agent"]';
const TRANSCRIPT = '[aria-label="Transcript"]';

let failed = false;
try {
  const { base, check, open } = env;

  // A session on a phone: create, stream, reload, second turn, stop.
  const phone = await open(PHONE, "phone");
  let page = phone.page;
  await page.goto(`${base}/`);
  await page.waitForSelector(PROMPT);
  check("dry run is announced", (await page.textContent("body")).includes("Dry run"));
  await page.fill(PROMPT, "Add a health check endpoint to the api service\nand cover it with a test.");
  await page.keyboard.press("Control+Enter");
  await page.waitForURL(/\/s\//, { timeout: 10000 });
  await phone.bodyHas("Review");
  await page.waitForTimeout(300);
  const first = await page.locator(TRANSCRIPT).innerText();
  check("the transcript shows the prompt", first.includes("Add a health check endpoint"));
  await phone.shot("session-review");

  await page.reload();
  await phone.bodyHas("Review");
  await page.waitForTimeout(500);
  const reloaded = await page.locator(TRANSCRIPT).innerText();
  check("the transcript is identical after a reload", first === reloaded, `(${first.length} vs ${reloaded.length} chars)`);

  await page.fill(MESSAGE, "Also add a readiness probe.");
  await page.keyboard.press("Control+Enter");
  await phone.bodyHas("Also add a readiness probe.", 10000);
  await page.waitForTimeout(1200);
  await phone.bodyHas("Review");
  const second = await page.locator(TRANSCRIPT).innerText();
  check("a second turn is appended", second.length > reloaded.length);

  await page.goto(`${base}/`);
  await page.fill(PROMPT, "Refactor the billing module");
  await page.keyboard.press("Control+Enter");
  await page.waitForURL(/\/s\//);
  await page.getByRole("button", { name: /^stop$/i }).click();
  await phone.bodyHas("Cancelled", 10000);
  check("stop cancels the session", true);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  check("no horizontal overflow on a phone", !overflow);
  check("the phone console is clean", phone.errors.length === 0, JSON.stringify(phone.errors).slice(0, 300));

  // The same data on a desktop: list beside the open session.
  const desktop = await open(DESKTOP, "desktop");
  page = desktop.page;
  await page.goto(`${base}/`);
  // The list fills in as `session_upserted` frames arrive, so wait for both rows.
  const listed = await page
    .waitForFunction(() => document.querySelectorAll("a[href^='/s/']").length >= 2, null, { timeout: 10000 })
    .then(() => true, () => false);
  check("the list shows both sessions", listed);
  await page.locator("a[href^='/s/']").last().click();
  await desktop.bodyHas("Review", 15000);
  await desktop.shot("session");
  check("the desktop console is clean", desktop.errors.length === 0, JSON.stringify(desktop.errors).slice(0, 300));

  failed = env.results.some((result) => !result.ok);
} catch (error) {
  console.log(`FAIL  the run broke: ${error.message.split("\n")[0]}`);
  failed = true;
} finally {
  await env.close();
}

console.log(failed ? "\nE2E FAILED" : `\nE2E PASSED (${env.results.length} checks)`);
process.exit(failed ? 1 : 0);
