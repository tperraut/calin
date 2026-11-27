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
  const allowOn = (p) => p.getByRole("button", { name: /^allow$/i });
  const allow = () => allowOn(page);

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

  // ---- M3: the session graph and timeline, from the real server's data.
  await page.getByRole("tab", { name: /^graph/i }).first().click();
  await page.waitForSelector('[aria-label="Session graph"]', { timeout: 15000 });
  await page.waitForFunction(
    () => /explorer/i.test(document.querySelector('[aria-label="Session graph"]')?.textContent ?? ""),
    null,
    { timeout: 15000 },
  );
  // The graph is SVG, so read its text content rather than innerText.
  const graphText = await page.locator('[aria-label="Session graph"]').first().evaluate((el) => el.textContent ?? "");
  check("the graph shows you, the lead and its subagent", /you/i.test(graphText) && /lead/i.test(graphText) && /explorer/i.test(graphText));
  check("the graph shows the repos", graphText.includes("alpha-svc") && graphText.includes("beta-web"));
  const graph = (await api("GET", `/sessions/${sessionId}/graph`)).body;
  check(
    "the server reports who changed which repo",
    graph.edges.some((e) => e.kind === "changed") && graph.edges.some((e) => e.kind === "spawned"),
    JSON.stringify(graph.edges.map((e) => [e.kind, e.count])),
  );
  await page.waitForTimeout(600);
  await desktop.shot("graph");
  await page.getByRole("group", { name: "View" }).getByRole("button", { name: "Timeline" }).click().catch(() =>
    page.getByRole("button", { name: "Timeline" }).first().click(),
  );
  await page.waitForSelector('[aria-label="Timeline"]', { timeout: 15000 });
  await page.waitForTimeout(600);
  // Lane labels are HTML beside the drawing, so look at the page text.
  const timelineText = await page.evaluate(() => document.body.innerText);
  check("the timeline has a lane per agent", /lead · /i.test(timelineText) && /Waiting for you/.test(timelineText));
  await desktop.shot("timeline");
  check("the desktop console is clean", desktop.errors.length === 0, JSON.stringify(desktop.errors).slice(0, 300));

  // ---- M2: a home repo that declares the others, attach mid-session, ship.
  const home = await repo(
    "home-base",
    {
      ".calin/repos.yaml": "repos:\n  gamma-api: {}\n  delta-web: {}\nsets:\n  pair: [gamma-api, delta-web]\n",
      ".calin/workflows/implement.yaml": [
        "version: 1",
        "name: Implement and ship",
        "nodes:",
        "  - { id: start, type: start }",
        '  - { id: implement, type: agent, prompt: "{{task}}" }',
        '  - { id: tests, type: check, command: "true" }',
        "  - { id: retry, type: loop, max: 2 }",
        '  - { id: fix, type: agent, prompt: "Fix the checks: {{nodes.tests.output}}" }',
        '  - { id: gate, type: gate, message: "Open pull requests?" }',
        "  - { id: ship, type: ship }",
        "  - { id: ok, type: end }",
        "edges:",
        "  - { from: start, to: implement }",
        "  - { from: implement.done, to: tests }",
        "  - { from: tests.pass, to: gate }",
        "  - { from: tests.fail, to: retry }",
        "  - { from: retry.repeat, to: fix }",
        "  - { from: fix.done, to: tests }",
        "  - { from: gate.approve, to: ship }",
        "  - { from: ship.done, to: ok }",
        "",
      ].join("\n"),
      "README.md": "# home\n",
    },
    { under: "code" },
  );
  await repo("gamma-api", { "README.md": "# gamma api\n" }, { under: "code" });
  await repo("delta-web", { "README.md": "# delta web\n" }, { under: "code" });

  const m2 = await open(PHONE, "m2");
  current = m2;
  page = m2.page;
  await page.goto(`${base}/settings`);
  await page.waitForSelector('[aria-label="Settings"]');
  await page.fill('input[placeholder="/home/you/code/home"]', home.path);
  await page.fill('input[placeholder="/home/you/code"]', `${env.scratch}/code`);
  await page.keyboard.press("Enter");
  await page.getByRole("button", { name: /^save/i }).click();
  let catalog = [];
  for (let i = 0; i < 40; i++) {
    catalog = (await api("GET", "/repos")).body;
    if (catalog.filter((r) => r.declared && r.available).length === 2) break;
    await page.waitForTimeout(250);
  }
  check(
    "the home repo's declared repos are found on disk",
    catalog.some((r) => r.home) && catalog.filter((r) => r.declared && r.available).length === 2,
    JSON.stringify(catalog.map((r) => [r.name, r.home, r.declared, r.available])),
  );
  await m2.shot("settings");

  await page.goto(`${base}/`);
  await page.waitForSelector(PROMPT);
  await page.fill(PROMPT, "Tidy the notes");
  await page.waitForTimeout(500);
  await m2.shot("composer");
  await page.getByRole("button", { name: /^start$/i }).click();
  await page.waitForURL(/\/s\//, { timeout: 10000 });
  const homeSession = page.url().split("/s/")[1].split("/")[0];
  for (let i = 0; i < 2; i++) {
    await m2.bodyHas("Allow this?");
    await allowOn(page).click();
    await page.waitForTimeout(600);
  }
  await m2.bodyHas("Review");
  let homeDetail = (await api("GET", `/sessions/${homeSession}`)).body;
  check(
    "a new session starts in the home repo, edited in place",
    homeDetail.repos.length === 1 && homeDetail.repos[0].name === "home-base" && homeDetail.repos[0].mode === "live",
    JSON.stringify(homeDetail.repos.map((r) => [r.name, r.mode, r.primary])),
  );

  const attached = await api("POST", `/sessions/${homeSession}/repos`, {
    repo_id: catalog.find((r) => r.name === "gamma-api").id,
  });
  check("a repo can be attached to a running session", attached.status === 201, JSON.stringify(attached.body).slice(0, 120));
  await m2.bodyHas("gamma-api", 10000);
  check("the transcript says who attached it", /attached by you/i.test(await page.locator(TRANSCRIPT).innerText()));

  // Change something in the attached worktree, then ship.
  const written = await api("PUT", `/sessions/${homeSession}/repos/${attached.body.id}/file?path=NOTES.md`, {
    text: "shipped from e2e\n",
  });
  check("a file can be created in the attached repo", written.status === 200);
  await page.getByRole("tab", { name: /changes/i }).first().click();
  await m2.bodyHas("NOTES.md", 15000);
  await page.getByRole("button", { name: /open pull requests/i }).first().click();
  await page.getByRole("button", { name: /^ship$/i }).click();
  await m2.bodyHas("Shipping", 20000);
  await page.waitForTimeout(500);
  homeDetail = (await api("GET", `/sessions/${homeSession}`)).body;
  check(
    "shipping opens one pull request for the changed worktree only",
    homeDetail.pull_requests.length === 1 && homeDetail.pull_requests[0].url.includes("example.invalid/gamma-api"),
    JSON.stringify(homeDetail.pull_requests.map((p) => [p.url, p.state])),
  );
  check("the pull request is shown", (await page.textContent("body")).includes("#1"));
  await m2.shot("shipping");
  check("the M2 console is clean", m2.errors.length === 0, JSON.stringify(m2.errors).slice(0, 300));

  // ---- M4: a workflow run with a check, a gate and a ship step.
  const workflows = (await api("GET", "/workflows")).body;
  check(
    "the home repo's workflow is listed and valid",
    workflows.some((w) => w.id === "implement" && w.graph && !w.problem) && workflows[0].id === "quick",
    JSON.stringify(workflows.map((w) => [w.id, w.problem])),
  );
  const flow = await api("POST", "/sessions", {
    prompt: "Add a changelog entry",
    workflow: "implement",
    permission_mode: "autonomous",
    repos: [{ repo_id: catalog.find((r) => r.name === "delta-web").id }],
  });
  check("a session can be started with a workflow", flow.status === 201, JSON.stringify(flow.body).slice(0, 120));
  const flowId = flow.body.session.id;
  const m4 = await open(DESKTOP, "m4");
  current = m4;
  page = m4.page;
  await page.goto(`${base}/s/${flowId}`);
  await page.waitForSelector('[aria-label="Workflow gate"]', { timeout: 30000 });
  check("the run stops at its gate and the session waits", (await page.textContent("body")).includes("Open pull requests?"));
  await page.getByRole("tab", { name: /^graph/i }).first().click();
  await page.waitForSelector('[aria-label="Workflow graph"]', { timeout: 15000 });
  await page.waitForTimeout(700);
  await m4.shot("workflow-at-gate");
  await page.getByLabel("Comment (optional)").fill("ship it");
  await page.getByRole("button", { name: /^approve$/i }).click();
  await m4.bodyHas("Shipping", 30000);
  await page.waitForTimeout(700);
  const run = (await api("GET", `/sessions/${flowId}/workflow`)).body;
  check(
    "the run went implement, tests, gate, ship and succeeded",
    run.status === "success" && ["implement", "tests", "gate", "ship"].every((id) => run.nodes.find((n) => n.id === id)?.state === "finished"),
    JSON.stringify(run.nodes.map((n) => [n.id, n.state, n.last_port])),
  );
  check("the loop it never needed stayed idle", run.nodes.find((n) => n.id === "retry")?.state === "idle");
  const flowDetail = (await api("GET", `/sessions/${flowId}`)).body;
  check("the ship step opened a pull request", flowDetail.pull_requests.length === 1);
  await m4.shot("workflow-done");
  check("the M4 console is clean", m4.errors.length === 0, JSON.stringify(m4.errors).slice(0, 300));

  // ---- M6: usage, starting from an issue, automations.
  const m6 = await open(DESKTOP, "m6");
  current = m6;
  page = m6.page;
  await page.goto(`${base}/usage`);
  await m6.bodyHas("By day", 15000);
  const usage = (await api("GET", "/usage?by=session")).body;
  check(
    "usage adds up what the sessions of this run cost",
    usage.totals.cost_usd > 0 && usage.totals.sessions >= 3 && usage.rows.length >= 3,
    JSON.stringify(usage.totals),
  );
  check("the usage page shows the plan's limit window", /5-hour window/i.test(await page.textContent("body")));
  await page.waitForTimeout(400);
  await m6.shot("usage");

  await page.goto(`${base}/`);
  await page.waitForSelector(PROMPT);
  await page.getByText("Issues", { exact: true }).first().click();
  const sample = page.getByRole("button", { name: /sample issue/i }).first();
  await sample.waitFor({ timeout: 15000 });
  await sample.click();
  await page.getByRole("button", { name: /^remove issue #/i }).waitFor({ timeout: 10000 });
  await m6.shot("issue-picked");
  await page.getByRole("button", { name: /^start$/i }).click();
  await page.waitForURL(/\/s\//, { timeout: 10000 });
  const issueSession = page.url().split("/s/")[1].split("/")[0];
  const fromIssue = (await api("GET", `/sessions/${issueSession}`)).body.session;
  check("a session started from an issue remembers it", Boolean(fromIssue.issue?.number), JSON.stringify(fromIssue.issue));
  const chip = await page
    .getByText(`#${fromIssue.issue?.number}`, { exact: false })
    .first()
    .waitFor({ timeout: 10000 })
    .then(() => true, () => false);
  check("the session page shows the issue", chip);
  await api("POST", `/sessions/${issueSession}/stop`);

  const created = await api("POST", "/automations", {
    name: "Nightly tidy",
    enabled: false,
    trigger: { kind: "schedule", cron: "0 3 * * *" },
    session: { prompt: "Tidy the notes for {{date}}", permission_mode: "autonomous", repos: [] },
  });
  check("an automation can be saved", created.status === 201 && /03:00/.test(created.body.trigger_text), JSON.stringify(created.body).slice(0, 160));
  const refused = await api("POST", "/automations", {
    name: "Asks",
    enabled: false,
    trigger: { kind: "schedule", cron: "0 3 * * *" },
    session: { prompt: "x", permission_mode: "ask", repos: [] },
  });
  check("an automation that would stop to ask is refused", refused.status === 400);
  await page.goto(`${base}/automations`);
  await m6.bodyHas("Nightly tidy", 15000);
  await page.getByRole("link", { name: /nightly tidy/i }).first().click();
  await page.getByRole("button", { name: /^run now$/i }).click();
  await page.getByRole("link", { name: /open the started session/i }).waitFor({ timeout: 20000 });
  await m6.shot("automation-run");
  await page.getByRole("link", { name: /open the started session/i }).click();
  await m6.bodyHas("Review", 30000);
  const runs = (await api("GET", `/automations/${created.body.id}/runs`)).body;
  check("running it now starts a session and records the run", runs.length === 1 && runs[0].outcome === "started" && runs[0].cause === "manual");
  check("the M6 console is clean", m6.errors.filter((e) => !/409|400|Failed to load resource/.test(e)).length === 0, JSON.stringify(m6.errors).slice(0, 300));

  // ---- Orchestration, with the mock following directives in the prompt.
  const demo = await api("POST", "/sessions", {
    prompt: "Ship the health check demo: orchestrate demo: note",
    permission_mode: "autonomous",
    repos: [],
  });
  const leadId = demo.body.session.id;
  let tree = [];
  for (let i = 0; i < 80; i++) {
    tree = (await api("GET", "/sessions")).body.filter((x) => x.root_session_id === leadId);
    if (tree.length === 3 && tree.every((x) => x.status === "review")) break;
    await new Promise((r) => setTimeout(r, 250));
  }
  check(
    "a lead dispatches two child sessions and all three come to rest",
    tree.length === 3 && tree.every((x) => x.status === "review"),
    JSON.stringify(tree.map((x) => [x.title, x.status])),
  );
  const demoGraph = (await api("GET", `/sessions/${leadId}/graph`)).body;
  const kinds = (kind) => demoGraph.edges.filter((e) => e.kind === kind).length;
  check(
    "the graph has both dispatches, both reports and the notes",
    kinds("dispatched") === 2 && kinds("report") === 2 && kinds("message") >= 2,
    JSON.stringify(demoGraph.edges.map((e) => [e.kind, e.count])),
  );
  const orch = await open(DESKTOP, "orchestration");
  current = orch;
  page = orch.page;
  await page.goto(`${base}/s/${leadId}/graph`);
  await page.waitForSelector('[aria-label="Session graph"]', { timeout: 15000 });
  await page.waitForFunction(
    () => /part B/.test(document.querySelector('[aria-label="Session graph"]')?.textContent ?? ""),
    null,
    { timeout: 15000 },
  );
  check("the session list nests the children under the lead", (await page.locator("a[href^='/s/']").allInnerTexts()).some((t) => /part A/.test(t)));
  await page.waitForTimeout(800);
  await orch.shot("graph");
  await page.getByRole("group", { name: "View" }).getByRole("button", { name: "Timeline" }).click();
  await page.waitForTimeout(600);
  await orch.shot("timeline");

  // A question from the agent, answered in the page.
  const asking = await api("POST", "/sessions", { prompt: "Which database? demo: ask", permission_mode: "autonomous", repos: [] });
  await page.goto(`${base}/s/${asking.body.session.id}`);
  const question = page.getByRole("region", { name: "Question from the agent" });
  await question.waitFor({ timeout: 20000 });
  check("a question parks the session", (await page.textContent("body")).includes("Waiting"));
  await orch.shot("question");
  await question.getByRole("group", { name: "Suggested answers" }).getByRole("button").first().click();
  await orch.bodyHas("You answered", 20000);
  await orch.bodyHas("Review", 20000);
  check("the answer reaches the agent and the session moves on", true);
  check("the orchestration console is clean", orch.errors.length === 0, JSON.stringify(orch.errors).slice(0, 300));

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
