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
let current;
try {
  const { base, api, check, open, repo } = env;

  await repo("alpha-svc", { "README.md": "# alpha service\n", "src/main.rs": "fn main() {}\n" });
  await repo("beta-web", { "README.md": "# beta web\n" });

  // ---- A two-repo session on a phone, answering permission requests.
  const phone = await open(PHONE, "phone");
  current = phone;
  let page = phone.page;
  const tab = (name) => page.getByRole("tab", { name }).first();
  const allow = () => page.getByRole("button", { name: /^allow$/i });

  await page.goto(`${base}/`);
  await page.waitForSelector(PROMPT);
  check("dry run is announced", (await page.textContent("body")).includes("Dry run"));
  await page.fill(PROMPT, "Add a health check endpoint\nand cover it with a test.");
  await page.getByRole("checkbox", { name: /alpha-svc/ }).check();
  await page.getByRole("checkbox", { name: /beta-web/ }).check();
  await phone.shot("composer");
  await page.getByRole("button", { name: /^start$/i }).click();
  await page.waitForURL(/\/s\//, { timeout: 10000 });
  const sessionUrl = page.url();
  const sessionId = sessionUrl.split("/s/")[1].split("/")[0];

  await phone.bodyHas("Allow this?");
  check("a permission request parks the session", (await page.textContent("body")).includes("Waiting"));
  await phone.shot("permission");
  await allow().click();
  // The mock asks once for its command and once for its edit.
  await page.waitForTimeout(600);
  await phone.bodyHas("Allow this?");
  await allow().click();
  await phone.bodyHas("Review");
  await page.waitForTimeout(300);
  const first = await page.locator(TRANSCRIPT).innerText();
  check("the transcript shows the prompt", first.includes("Add a health check endpoint"));

  const detail = (await api("GET", `/sessions/${sessionId}`)).body;
  check(
    "both repos are attached, each on the session branch",
    detail.repos.length === 2 && detail.repos.every((r) => r.branch && r.branch.startsWith("calin/")),
    JSON.stringify(detail.repos.map((r) => [r.name, r.mode, r.primary])),
  );

  await page.reload();
  await phone.bodyHas("Review");
  await page.waitForTimeout(500);
  const reloaded = await page.locator(TRANSCRIPT).innerText();
  check("the transcript is identical after a reload", first === reloaded, `(${first.length} vs ${reloaded.length} chars)`);

  // ---- Changes: grouped by repo, with a diff.
  await tab(/changes/i).click();
  await phone.bodyHas("calin-mock-notes.md", 15000);
  const changesText = await page.textContent("body");
  check("changes are grouped by repo", changesText.includes("alpha-svc") && changesText.includes("beta-web"));
  await phone.shot("changes");
  await page.locator('[aria-label="Changed files"]').getByText("calin-mock-notes.md").click();
  await page.waitForSelector('[aria-label="Diff"] .cm-content', { timeout: 15000 });
  await page.waitForFunction(
    () => document.querySelector('[aria-label="Diff"]')?.textContent.includes("turn 1"),
    null,
    { timeout: 10000 },
  );
  check("the diff shows what the agent wrote", true);
  await phone.shot("diff");

  // ---- Files: open, edit, save.
  await tab(/files/i).click();
  await page.waitForSelector('[aria-label="File tree"]', { timeout: 15000 });
  await page.getByRole("treeitem", { name: /README\.md/ }).first().click();
  await page.waitForSelector('[aria-label="Editor"] .cm-content', { timeout: 15000 });
  await page.waitForFunction(
    () => document.querySelector('[aria-label="Editor"]')?.textContent.includes("alpha service"),
    null,
    { timeout: 10000 },
  );
  await page.locator('[aria-label="Editor"] .cm-content').click();
  await page.keyboard.press("Control+End");
  await page.keyboard.type("Edited in the browser.");
  await page.keyboard.press("Control+s");
  const primary = detail.repos.find((r) => r.primary);
  let saved = false;
  for (let i = 0; i < 30 && !saved; i++) {
    const file = await api("GET", `/sessions/${sessionId}/repos/${primary.id}/file?path=README.md`);
    saved = file.body.text.includes("Edited in the browser.");
    if (!saved) await page.waitForTimeout(200);
  }
  check("a save from the editor reaches the worktree", saved);
  await phone.shot("editor");

  // ---- Terminal: a real shell in the worktree.
  await tab(/terminal/i).click();
  // With several repos the panel asks where to start; the primary repo is preselected.
  await page.getByRole("button", { name: /^start$/i }).click();
  await page.waitForSelector(".xterm", { timeout: 15000 });
  await page.locator(".xterm").first().click();
  await page.waitForTimeout(800);
  await page.keyboard.type("echo calin-$((6*7)) && pwd\n");
  const printed = await page
    .waitForFunction(() => document.querySelector(".xterm")?.textContent.includes("calin-42"), null, { timeout: 15000 })
    .then(() => true, () => false);
  check("the terminal runs a command", printed);
  const inWorktree = await page.evaluate(() => document.querySelector(".xterm")?.textContent.includes("work/alpha-svc"));
  check("the shell starts in the repo's worktree", inWorktree);
  await phone.shot("terminal");

  // ---- Panels keep their state.
  await tab(/files/i).click();
  const kept = await page.evaluate(() =>
    document.querySelector('[aria-label="Editor"]')?.textContent.includes("Edited in the browser."),
  );
  check("the editor keeps its file across panel switches", kept);
  await tab(/terminal/i).click();
  const alive = await page.evaluate(() => document.querySelector(".xterm")?.textContent.includes("calin-42"));
  check("the terminal keeps its shell across panel switches", alive);

  // ---- A second turn with a denied request.
  await tab(/transcript/i).click();
  await page.fill(MESSAGE, "Also add a readiness probe.");
  await page.keyboard.press("Control+Enter");
  await phone.bodyHas("Allow this?");
  await page.getByRole("button", { name: /^deny$/i }).click();
  await page.getByRole("button", { name: /confirm deny/i }).click();
  await page.waitForTimeout(600);
  await phone.bodyHas("Allow this?");
  await allow().click();
  await phone.bodyHas("Review");
  await page.waitForTimeout(300);
  const second = await page.locator(TRANSCRIPT).innerText();
  check("a second turn is appended", second.includes("Also add a readiness probe."));
  check("the denied request is shown as denied", /denied/i.test(second));

  // ---- Stop, with a request still open.
  await page.goto(`${base}/`);
  await page.fill(PROMPT, "Refactor the billing module");
  await page.keyboard.press("Control+Enter");
  await page.waitForURL(/\/s\//);
  await phone.bodyHas("Allow this?");
  await page.getByRole("button", { name: /^stop$/i }).click();
  await phone.bodyHas("Cancelled", 10000);
  check("stop cancels the session", true);
  const stillAsking = await page.getByRole("button", { name: /^allow$/i }).count();
  check("an unanswered request is closed with the session", stillAsking === 0);

  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  check("no horizontal overflow on a phone", !overflow);
  check("the phone console is clean", phone.errors.length === 0, JSON.stringify(phone.errors).slice(0, 300));

  // ---- The same session on a desktop: list, transcript and the panel side by side.
  const desktop = await open(DESKTOP, "desktop");
  current = desktop;
  page = desktop.page;
  await page.goto(`${base}/`);
  const listed = await page
    .waitForFunction(() => document.querySelectorAll("a[href^='/s/']").length >= 2, null, { timeout: 10000 })
    .then(() => true, () => false);
  check("the list shows both sessions", listed);
  await page.goto(`${sessionUrl.replace(/\/(changes|files|terminal)$/, "")}/changes`);
  await desktop.bodyHas("calin-mock-notes.md", 15000);
  await page.locator('[aria-label="Changed files"]').getByText("calin-mock-notes.md").click();
  await page.waitForSelector('[aria-label="Diff"] .cm-content', { timeout: 15000 });
  const sideBySide = await page.evaluate(
    () => Boolean(document.querySelector('[aria-label="Transcript"]')) && Boolean(document.querySelector('[aria-label="Diff"]')),
  ).catch(() => false);
  check("transcript and diff are visible together on a desktop", sideBySide);
  await desktop.shot("changes");
  check("the desktop console is clean", desktop.errors.length === 0, JSON.stringify(desktop.errors).slice(0, 300));

  failed = env.results.some((result) => !result.ok);
} catch (error) {
  console.log(`FAIL  the run broke: ${error.message.split("\n")[0]}`);
  await current?.shot("failure").catch(() => {});
  failed = true;
} finally {
  await env.close();
}

console.log(failed ? "\nE2E FAILED" : `\nE2E PASSED (${env.results.length} checks)`);
process.exit(failed ? 1 : 0);
