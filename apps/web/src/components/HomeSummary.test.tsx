import { afterEach, describe, expect, it } from "vite-plus/test";
import type { SessionStatus } from "../gen/SessionStatus";
import { makeSession } from "../test/fakes";
import { makeLimits, makeWindow } from "../test/integrations";
import { find, mount, unmountAll } from "../test/render";
import { HomeSummary, summaryParts } from "./HomeSummary";
import { SessionList } from "./SessionList";

afterEach(unmountAll);

const sessions = (...statuses: SessionStatus[]) =>
  statuses.map((status, i) => makeSession(`s${i}`, 1000 - i, { status }));
const words = (...args: Parameters<typeof summaryParts>) =>
  summaryParts(...args)
    .map((part) => part.text)
    .join(" · ");

describe("home summary", () => {
  const limits = [
    makeLimits("claude", [
      makeWindow("five_hour", { used: 0.62 }),
      makeWindow("seven_day", { used: 0.3 }),
    ]),
  ];

  it("says each part only when there is something to say", () => {
    expect(
      words(sessions("waiting", "review", "running", "queued", "queued", "queued"), limits),
    ).toBe("2 need you · 1 running · 3 queued · 5-hour window 62% used");
    expect(words(sessions("review"), [])).toBe("1 needs you");
    expect(words(sessions("preparing", "running", "done", "failed"), [])).toBe("2 running");
    expect(words(sessions("queued", "cancelled"), limits)).toBe(
      "1 queued · 5-hour window 62% used",
    );
    expect(words(sessions("done"), limits)).toBe("5-hour window 62% used");
    expect(words([], [])).toBe("");
    expect(words([], [makeLimits("claude", [])])).toBe("");
  });

  it("links the first part to the first session that needs the user, as the list orders them", () => {
    const list = [
      makeSession("new", 3000, { status: "running" }),
      makeSession("child", 2500, {
        status: "waiting",
        parent_session_id: "old",
        root_session_id: "old",
      }),
      makeSession("mid", 2000, { status: "review" }),
      makeSession("old", 1000, { status: "running" }),
    ];
    // "mid" is newer than "old", so its tree comes first among those that need the user.
    expect(summaryParts(list, [])[0]?.href).toBe("/s/mid");
    const host = mount(() => <HomeSummary sessions={list} limits={limits} />);
    const link = find<HTMLAnchorElement>(host, '[data-part="home-summary"] a');
    expect(link.textContent).toBe("2 need you");
    expect(link.getAttribute("href")).toBe("/s/mid");
    expect(find(host, '[data-part="home-summary"]').textContent).toBe(
      "2 need you·2 running·5-hour window 62% used",
    );
  });

  it("renders nothing when every part is zero or unknown", () => {
    const host = mount(() => <HomeSummary sessions={sessions("done")} limits={[]} />);
    expect(host.querySelector('[data-part="home-summary"]')).toBeNull();
  });
});

describe("session list order", () => {
  it("puts the trees that need the user first, children still under their parent", () => {
    const list = [
      makeSession("a", 5000, { status: "running" }),
      makeSession("b", 4000, { status: "review" }),
      makeSession("c", 3000, { status: "queued" }),
      makeSession("c1", 3500, { status: "waiting", parent_session_id: "c", root_session_id: "c" }),
      makeSession("d", 2000, { status: "waiting" }),
      makeSession("e", 1000, { status: "done" }),
    ];
    const host = mount(() => <SessionList sessions={list} now={6000} />);
    const top = [...host.querySelectorAll(":scope > ul > li")].map(
      (li) => li.querySelector("a span span")?.textContent,
    );
    expect(top).toEqual(["Session b", "Session c", "Session d", "Session a", "Session e"]);
    const nested = find(host, 'ul[aria-label="Child sessions of Session c"]');
    expect(nested.textContent).toContain("Session c1");
  });
});
