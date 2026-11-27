import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { App } from "./App";
import { DayChart } from "./components/DayChart";
import { HttpError, type Api, type UsageQuery } from "./api/http";
import type { Health } from "./gen/Health";
import type { RunnerLimits } from "./gen/RunnerLimits";
import type { Usage } from "./gen/Usage";
import { dayKey } from "./lib/usage";
import { createAppState } from "./state/app";
import { deferred, fakeApi, FakeSocket, immediately, settle } from "./test/fakes";
import { makeLimits, makeUsage, makeWindow, row } from "./test/integrations";
import { click, find, findByText, mount, unmountAll } from "./test/render";

afterEach(unmountAll);

const health: Health = {
  name: "calin",
  version: "1",
  dry_run: false,
  runners: ["claude"],
  github_problem: null,
};
const DAY = 86_400_000;
const daysAgo = (n: number): string => dayKey(Date.now() - n * DAY);

function answers(byDay: Usage["rows"], extra: Partial<Record<Usage["by"], Usage["rows"]>> = {}) {
  return (query?: UsageQuery): Promise<Usage> => {
    const by = query?.by ?? "day";
    return Promise.resolve(makeUsage(by, by === "day" ? byDay : (extra[by] ?? [])));
  };
}

async function open(path: string, routes: Partial<Api> = {}) {
  history.replaceState(null, "", path);
  const socket = new FakeSocket();
  const getUsage = vi.fn(routes.getUsage ?? answers([row(daysAgo(1), 2), row(daysAgo(0), 1)]));
  const api = fakeApi({
    health: () => Promise.resolve(health),
    listRepos: () => Promise.resolve([]),
    ...routes,
    getUsage,
  });
  const state = createAppState({ api, socket, schedule: immediately });
  const host = mount(() => <App state={state} />);
  socket.setState("open");
  const main = () => find(host, "main");
  // The page's code arrives on its first visit (router.ts).
  const waiting = () => {
    const shown = host.querySelector("main")?.textContent;
    return shown === undefined || shown === "Loading…";
  };
  for (let i = 0; i < 50 && waiting(); i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 5));
    flush();
  }
  await settle();
  flush();
  return { host, main, getUsage };
}

const bars = (root: ParentNode) => [...root.querySelectorAll<SVGElement>('[data-part="hit"]')];
const meters = (root: ParentNode) =>
  [...root.querySelectorAll('[data-part="meter"]')].map((meter) =>
    (meter.textContent ?? "").replace(/\s+/g, " ").trim(),
  );

describe("Usage page", () => {
  it("asks for the last 30 days by default, in four breakdowns, and shows the totals", async () => {
    const { main, getUsage } = await open("/usage");
    const queries = getUsage.mock.calls.map(([query]) => query);
    expect(queries.map((query) => query?.by)).toEqual(["day", "session", "repo", "model"]);
    const span = (queries[0]?.to ?? 0) - (queries[0]?.from ?? 0);
    expect(span).toBeGreaterThan(29 * DAY);
    expect(span).toBeLessThan(30 * DAY);
    expect(find(main(), '[aria-label="Range"] [aria-pressed="true"]').textContent).toBe("30 days");
    const totals = find(main(), '[data-part="totals"]').textContent;
    expect(totals).toContain("$3.00");
    expect(totals).toContain("Sessions2");
    expect(main().textContent).toContain(
      "As reported by the agent CLI. On a subscription plan this estimates the value used; it is not a bill.",
    );
  });

  it("keeps the range in the URL and reads it from there", async () => {
    const { main, getUsage } = await open("/usage?days=90");
    expect(find(main(), '[aria-label="Range"] [aria-pressed="true"]').textContent).toBe("90 days");
    const first = getUsage.mock.calls[0]?.[0];
    expect((first?.to ?? 0) - (first?.from ?? 0)).toBeGreaterThan(89 * DAY);

    await click(findByText(main(), "button", "7 days"));
    await settle();
    flush();
    expect(location.search).toBe("?days=7");
    const last = getUsage.mock.lastCall?.[0];
    expect((last?.to ?? 0) - (last?.from ?? 0)).toBeLessThan(7 * DAY);
    expect(find(main(), '[aria-label="Range"] [aria-pressed="true"]').textContent).toBe("7 days");
  });

  it("draws every day, empty ones included, and mirrors them in a hidden table", async () => {
    const { main } = await open("/usage", {
      getUsage: answers([row(daysAgo(4), 1), row(daysAgo(2), 0), row(daysAgo(0), 120)]),
    });
    const hits = bars(main());
    expect(hits).toHaveLength(5);
    expect(main().querySelectorAll('[data-part="bar"]')).toHaveLength(2);
    expect(main().querySelectorAll('[data-part="empty"]')).toHaveLength(3);
    // The tallest bar fills the plot up to the top tick of a "nice" axis.
    const axis = [...main().querySelectorAll('[data-part="value-axis"] text')].map(
      (t) => t.textContent,
    );
    expect(axis).toEqual(["$0", "$50", "$100", "$150"]);
    // Today is marked and is the one tab stop.
    expect(hits.at(-1)?.getAttribute("tabindex")).toBe("0");
    expect(hits.at(-1)?.getAttribute("aria-label")).toContain("(today): $120.00");
    expect(main().textContent).toContain("Today");
    const tableRows = [...main().querySelectorAll('[data-part="day-table"] tbody tr')];
    expect(tableRows).toHaveLength(5);
    expect(tableRows.map((one) => one.querySelector("td")?.textContent)).toEqual([
      "$1.00",
      "$0.00",
      "$0.00",
      "$0.00",
      "$120.00",
    ]);
  });

  it("marks the server's last day as today, whatever the viewer's date", async () => {
    // A server a day ahead of the viewer: its last day is tomorrow here.
    const { main } = await open("/usage", {
      getUsage: answers([row(daysAgo(0), 1), row(daysAgo(-1), 3)]),
    });
    const hits = bars(main());
    expect(hits).toHaveLength(2);
    expect(hits.at(-1)?.getAttribute("aria-label")).toContain("(today): $3.00");
    expect(hits.at(0)?.getAttribute("aria-label")).not.toContain("(today)");
    expect(main().querySelectorAll("[data-today]")).toHaveLength(1);
    expect(find(main(), '[data-part="day-zone"]').textContent).toContain(
      "Days are the server's local days.",
    );
  });

  it("marks no day as today when the range does not end now", () => {
    const host = mount(() => (
      <DayChart days={[row("2026-11-26", 1), row("2026-11-27", 2)]} today={undefined} />
    ));
    expect(host.querySelectorAll("[data-today]")).toHaveLength(0);
    expect(host.textContent).not.toContain("Today");
    expect(host.textContent).not.toContain("(today)");
  });

  it("moves between bars with the arrow keys and says the focused day", async () => {
    const { main } = await open("/usage", {
      getUsage: answers([row(daysAgo(1), 2), row(daysAgo(0), 1)]),
    });
    const [yesterday, today] = bars(main());
    today?.dispatchEvent(new FocusEvent("focus"));
    today?.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
    yesterday?.dispatchEvent(new FocusEvent("focus"));
    flush();
    expect(yesterday?.getAttribute("tabindex")).toBe("0");
    expect(find(main(), '[data-part="day-detail"]').textContent).toContain("$2.00");
  });

  it("lists the most expensive sessions with links, and notes the server's cap", async () => {
    const sessions = Array.from({ length: 200 }, (_, i) => row(`s${i}`, 200 - i, `Task ${i}`));
    const { main } = await open("/usage", {
      getUsage: answers([row(daysAgo(0), 1)], {
        session: sessions,
        repo: [row("api", 3, "api"), row("", 1, "")],
        model: [row("opus", 2, "opus")],
      }),
    });
    const links = [...main().querySelectorAll<HTMLAnchorElement>('[data-part="by-session"] a')];
    expect(links).toHaveLength(200);
    expect(links[0]?.getAttribute("href")).toBe("/s/s0");
    expect(links[0]?.textContent).toBe("Task 0");
    expect(main().textContent).toContain(
      "The 200 most expensive sessions, the most the server lists.",
    );
    const repoRows = [...main().querySelectorAll('[data-part="by-repo"] tbody tr')].map((one) =>
      one.textContent?.replace(/\s+/g, " "),
    );
    expect(repoRows).toEqual(["api$3.003k / 300", "No repo$1.001k / 100"]);
    expect(find(main(), '[data-part="by-model"]').textContent).toContain("opus$2.00");
  });

  it("shows no cap note below the cap", async () => {
    const { main } = await open("/usage", {
      getUsage: answers([row(daysAgo(0), 1)], { session: [row("s1", 1, "One")] }),
    });
    expect(main().textContent).not.toContain("most expensive sessions, the most");
  });

  it("says when nothing was used, while loading, and the server's message on a 400", async () => {
    const empty = await open("/usage", { getUsage: answers([row(daysAgo(0), 0)]) });
    expect(empty.main().textContent).toContain("Nothing was used in this range.");
    expect(bars(empty.main())).toHaveLength(0);
    unmountAll();

    const pending = deferred<Usage>();
    const loading = await open("/usage", { getUsage: () => pending.promise });
    expect(loading.main().textContent).toContain("Loading usage…");
    unmountAll();

    const refused = await open("/usage", {
      getUsage: () => Promise.reject(new HttpError(400, "the range is at most 366 days", true)),
    });
    expect(find(refused.main(), '[role="alert"]').textContent).toContain(
      "The usage could not be loaded: the range is at most 366 days",
    );
  });

  describe("plan limits", () => {
    const now = Date.now();
    const limits: RunnerLimits[] = [
      makeLimits(
        "claude",
        [
          makeWindow("five_hour", { used: 0.62, resets_at: now + 60_000 }),
          makeWindow("seven_day", { used: 1, refused: true, resets_at: now + 2 * 60_000 }),
          makeWindow("burst_quota", { used: 0.1 }),
        ],
        now - 3 * 60_000,
      ),
    ];

    it("shows one meter per window in words: used, reset, refused, unknown name, no reset", async () => {
      const { main } = await open("/usage", { getUsageLimits: () => Promise.resolve(limits) });
      const clock = (ts: number) =>
        new Date(ts).toLocaleTimeString(undefined, {
          hour: "2-digit",
          minute: "2-digit",
          hourCycle: "h23",
        });
      const [five, seven, burst] = meters(main());
      expect(five).toContain(`5-hour window: 62% used, resets at ${clock(now + 60_000)}`);
      expect(five).toContain("Room left");
      expect(five).toContain("reported 3 min ago");
      expect(seven).toContain("7-day window: 100% used");
      expect(seven).toContain(`Requests are being refused until ${clock(now + 2 * 60_000)}.`);
      expect(seven).toContain("Refusing requests");
      expect(burst).toContain("burst_quota: 10% used");
      expect(burst).not.toContain("resets");
    });

    it("says when no limits were reported yet", async () => {
      const { main } = await open("/usage");
      expect(main().textContent).toContain(
        "No limits reported yet. They appear after an agent has run.",
      );
    });
  });
});
