// Sign-in end to end: the real binary in public mode on loopback, a stand-in
// OpenID Connect provider, and a browser.
//
// Usage: node signin.mjs <path to calin binary> <path to the test-provider example> [screenshot dir]
// Exit codes: 0 every check passed, 1 a check failed or the run broke, 2 no browser available.

import { spawn } from "node:child_process";
import { resolve } from "node:path";
import { NoBrowser, PHONE, start } from "./harness.mjs";

const binary = resolve(process.argv[2] ?? "../target/debug/calin");
const providerBinary = resolve(process.argv[3] ?? "../target/debug/examples/test-provider");
const shots = resolve(process.argv[4] ?? "./shots");
const USER = "alice@example.com";

/** Starts the provider and resolves with its issuer URL, which it prints first. */
function startProvider(children) {
  return new Promise((done, fail) => {
    const child = spawn(providerBinary, ["0", "calin-e2e", USER], { stdio: ["ignore", "pipe", "ignore"], detached: true });
    children.push(child);
    let out = "";
    const timer = setTimeout(() => fail(new Error("the test provider printed no issuer")), 10000);
    child.stdout.on("data", (chunk) => {
      out += chunk;
      const line = out.split("\n")[0].trim();
      if (out.includes("\n") && line) {
        clearTimeout(timer);
        done(line);
      }
    });
    child.once("error", fail);
  });
}

let env;
try {
  env = await start({
    binary,
    shots,
    host: "localhost",
    serverEnv: async (port, { children }) => ({
      CALIN_PUBLIC_URL: `http://localhost:${port}`,
      CALIN_UNSAFE_DEV_ALLOW_HTTP_LOOPBACK: "1",
      CALIN_OIDC_ISSUER: await startProvider(children),
      CALIN_OIDC_CLIENT_ID: "calin-e2e",
      CALIN_ALLOWED_USERS: USER,
    }),
  });
} catch (error) {
  if (error instanceof NoBrowser) {
    console.log(`\nE2E SKIPPED: ${error.message}\nThis is not a pass.\n`);
    process.exit(2);
  }
  throw error;
}

let failed = false;
let current;
try {
  const { base, check, open } = env;
  const status = async (path, init) => (await fetch(`${base}/api/v1${path}`, init)).status;

  check("the API refuses a caller who is not signed in", (await status("/sessions")) === 401);
  const health = await (await fetch(`${base}/api/v1/health`)).json();
  check("health tells a stranger only that sign-in is required", health.auth === "required" && health.runners === undefined);

  const phone = await open(PHONE, "signin");
  current = phone;
  const page = phone.page;
  // Expected refusals show up as console errors; anything else is a real one.
  const realErrors = () => phone.errors.filter((e) => !/401|Failed to load resource/.test(e));

  await page.goto(`${base}/settings`);
  await page.getByRole("button", { name: /^sign in$/i }).waitFor({ timeout: 15000 });
  check("a stranger sees the sign-in screen, not the app", !(await page.textContent("body")).includes("Scan roots"));
  await phone.shot("screen");
  await page.getByRole("button", { name: /^sign in$/i }).click();
  await page.waitForURL(`${base}/settings`, { timeout: 20000 });
  await phone.bodyHas(USER, 15000);
  check("sign-in returns to the page that was asked for", page.url() === `${base}/settings`);
  const me = await page.evaluate(() => fetch("/api/v1/me").then((r) => r.json()));
  check("the browser is signed in as the allowed user", me.mode === "public" && me.identity === USER, JSON.stringify(me));
  const cookies = await page.context().cookies();
  const session = cookies.find((c) => c.name === "__Host-calin_session");
  check(
    "the session cookie is HttpOnly, Secure and SameSite=Lax",
    Boolean(session) && session.httpOnly && session.secure && session.sameSite === "Lax",
    JSON.stringify(session && { httpOnly: session.httpOnly, secure: session.secure, sameSite: session.sameSite }),
  );
  check("scripts cannot read the session cookie", !(await page.evaluate(() => document.cookie)).includes("calin_session"));

  // A device token, shown once, works without cookies.
  await page.getByLabel("New token").fill("e2e laptop");
  await page.getByRole("button", { name: /^create token$/i }).click();
  const box = page.getByRole("region", { name: "New device token" });
  await box.waitFor({ timeout: 10000 });
  const token = ((await box.innerText()).match(/calin_[A-Za-z0-9_-]+/) ?? [])[0];
  check("a new device token is shown", Boolean(token));
  const viaToken = await fetch(`${base}/api/v1/me`, { headers: { authorization: `Bearer ${token}` } }).then((r) => r.json());
  check("the device token authenticates without a cookie", viaToken.identity === USER && viaToken.device_token === true);
  await box.getByRole("button", { name: /^done$/i }).click();
  check("the secret is gone after Done", !(await page.textContent("body")).includes(token));
  await phone.shot("access");

  // The live socket works with the cookie: a session streams to the page.
  await page.goto(`${base}/`);
  await page.fill('textarea[placeholder="Describe what the agent should do"]', "Check the socket");
  await page.getByRole("button", { name: /^start$/i }).click();
  await page.waitForURL(/\/s\//, { timeout: 10000 });
  await phone.bodyHas("Allow this?", 20000);
  check("events stream to a signed-in browser", true);

  // Signing out ends it, for the cookie and for the page.
  await page.goto(`${base}/settings`);
  await page.getByRole("button", { name: /^sign out$/i }).first().click();
  await page.getByRole("button", { name: /^sign in$/i }).waitFor({ timeout: 10000 });
  const after = await page.evaluate(() => fetch("/api/v1/me").then((r) => r.status));
  check("after signing out the browser is refused", after === 401);
  check("the device token still works until it is revoked", (await status("/me", { headers: { authorization: `Bearer ${token}` } })) === 200);
  check("no unexpected console error", realErrors().length === 0, JSON.stringify(realErrors()).slice(0, 300));

  failed = env.results.some((result) => !result.ok);
} catch (error) {
  console.log(`FAIL  the run broke: ${error.message.split("\n")[0]}`);
  await current?.shot("failure").catch(() => {});
  failed = true;
} finally {
  await env.close();
}

console.log(failed ? "\nE2E SIGN-IN FAILED" : `\nE2E SIGN-IN PASSED (${env.results.length} checks)`);
process.exit(failed ? 1 : 0);
