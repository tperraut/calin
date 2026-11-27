import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { Api } from "../api/http";
import type { CheckState } from "../gen/CheckState";
import type { Health } from "../gen/Health";
import type { PullRequest } from "../gen/PullRequest";
import type { PullRequestState } from "../gen/PullRequestState";
import type { ReviewState } from "../gen/ReviewState";
import type { SessionStatus } from "../gen/SessionStatus";
import { conflict, deferred, makePullRequest, makeSessionRepo } from "../test/fakes";
import { click, find, findByText, mount, unmountAll } from "../test/render";
import { act, openWorkspace } from "../test/workspace";
import { PullRequestChips } from "./PullRequests";
import { PullRequestSection } from "./PullRequestSection";

const repos = [makeSessionRepo("api", { primary: true }), makeSessionRepo("web")];

const MINUTE = 60_000;

async function open(
  pullRequests: PullRequest[],
  options: { status?: SessionStatus; routes?: Partial<Api>; now?: number; health?: Health } = {},
) {
  const workspace = await openWorkspace({
    repos,
    pullRequests,
    status: options.status ?? "shipping",
    routes: options.routes,
    now: options.now,
    health: options.health,
  });
  const drafts: string[] = [];
  const host = mount(() => (
    <>
      <header>
        <PullRequestChips store={workspace.store} />
      </header>
      <PullRequestSection store={workspace.store} onSendBack={(text) => drafts.push(text)} />
    </>
  ));
  await act();
  const section = find(host, '[role="region"][aria-label="Pull requests"]');
  const rows = () =>
    [...section.querySelectorAll<HTMLElement>("ul > li")].map((row) => ({
      row,
      labels: [...row.querySelectorAll("span.rounded-full:not([aria-hidden])")].map(
        (badge) => badge.textContent,
      ),
    }));
  const rollUp = () => find(section, '[data-part="roll-up"]').textContent;
  const chips = () => [...host.querySelectorAll("header li")].map((chip) => chip.textContent);
  return { ...workspace, host, section, rows, rollUp, chips, drafts };
}

const update = (pullRequest: PullRequest) =>
  ({ type: "pull_request_updated", pull_request: pullRequest }) as const;

afterEach(() => {
  vi.useRealTimers();
  unmountAll();
});

describe("the pull request section", () => {
  const states: Array<[PullRequestState, string]> = [
    ["draft", "Draft"],
    ["open", "Open"],
    ["merged", "Merged"],
    ["closed", "Closed"],
  ];
  const checks: Array<[CheckState, string | undefined]> = [
    ["none", undefined],
    ["pending", "Checks pending"],
    ["passing", "Checks passing"],
    ["failing", "Checks failing"],
  ];
  const reviews: Array<[ReviewState, string | undefined]> = [
    ["none", undefined],
    ["approved", "Approved"],
    ["changes_requested", "Changes requested"],
  ];
  const combinations = states.flatMap(([state, stateLabel]) =>
    checks.flatMap(([check, checkLabel]) =>
      reviews.map(([review, reviewLabel]) => ({
        state,
        check,
        review,
        labels: [stateLabel, checkLabel, reviewLabel].filter((label) => label !== undefined),
      })),
    ),
  );

  it("names the state, the checks and the review of every combination in words", async () => {
    expect(combinations).toHaveLength(48);
    const { emit, caughtUp, rows, chips } = await open([makePullRequest("api", 12)]);
    caughtUp();
    for (const { state, check, review, labels } of combinations) {
      emit(update(makePullRequest("api", 12, { state, checks: check, review })));
      const [only] = rows();
      expect([state, check, review, only.labels]).toEqual([state, check, review, labels]);
      // The compact chips say the same, in the same words.
      expect(chips()).toEqual([["api", "#12", ...labels].join(" ")]);
    }
  });

  it("shows each pull request's repo and its number as a link that opens in a new tab", async () => {
    const { rows, section } = await open([
      makePullRequest("web", 7, { state: "draft" }),
      makePullRequest("api", 12, { checks: "passing", title: "Add a health check" }),
    ]);
    // In repo-set order, whatever order the server listed them in.
    expect(rows().map(({ row }) => row.textContent)).toEqual([
      "api #12 Open Checks passing",
      "web #7 Draft",
    ]);
    const link = find<HTMLAnchorElement>(section, 'a[data-pull-request="12"]');
    expect([link.textContent, link.getAttribute("href"), link.target, link.rel]).toEqual([
      "#12",
      "https://github.com/acme/api/pull/12",
      "_blank",
      "noopener noreferrer",
    ]);
    expect(link.getAttribute("aria-label")).toBe("Pull request #12 for api (opens in a new tab)");
  });

  it("shows a pull request that dry run made up, without making it a link", async () => {
    const { section, chips, host } = await open([
      makePullRequest("api", 1, { url: "https://example.invalid/acme/api/pull/1", state: "draft" }),
      makePullRequest("web", 7),
    ]);
    expect(section.querySelector('a[data-pull-request="1"]')).toBeNull();
    const plain = find(section, 'span[data-pull-request="1"]');
    expect(plain.textContent).toBe("#1");
    // The made-up address is shown as what it is: text.
    expect(section.textContent).toContain("https://example.invalid/acme/api/pull/1");
    expect(section.querySelector('a[data-pull-request="7"]')).not.toBeNull();
    expect(chips()).toEqual(["api #1 Draft", "web #7 Open"]);
    expect(host.querySelector('header a[data-pull-request="1"]')).toBeNull();
    expect([...host.querySelectorAll("a")].every((link) => !link.href.includes(".invalid"))).toBe(
      true,
    );
  });

  it("sums the session up in one line that follows the events", async () => {
    const { emit, caughtUp, rollUp } = await open([
      makePullRequest("api", 12, { state: "draft", checks: "pending" }),
      makePullRequest("web", 7, { state: "draft", checks: "pending" }),
    ]);
    caughtUp();
    expect(rollUp()).toBe("2 pull requests · 2 drafts · checks pending on api and web");

    emit(update(makePullRequest("api", 12, { checks: "passing" })));
    emit(update(makePullRequest("web", 7, { checks: "failing" })));
    expect(rollUp()).toBe("2 pull requests · checks failing on web");

    emit(update(makePullRequest("api", 12, { state: "merged", checks: "passing" })));
    expect(rollUp()).toBe("2 pull requests · 1 merged · checks failing on web");

    emit(update(makePullRequest("web", 7, { checks: "passing", review: "changes_requested" })));
    expect(rollUp()).toBe("2 pull requests · 1 merged · changes requested on web");

    emit(update(makePullRequest("web", 7, { state: "merged", checks: "passing" })));
    expect(rollUp()).toBe("2 pull requests · all merged");
  });

  it("says when the pull requests were last seen to change, from the newest of them", async () => {
    const { section } = await open(
      [
        makePullRequest("api", 12, { updated_at: 10 * MINUTE }),
        makePullRequest("web", 7, { updated_at: 25 * MINUTE }),
      ],
      { now: 30 * MINUTE },
    );
    expect(section.textContent).toContain("Updated 5 min ago");
    expect(find<HTMLTimeElement>(section, "time").dateTime).toBe(
      new Date(25 * MINUTE).toISOString(),
    );
  });

  it("asks the server to look again, says so meanwhile, and waits for the events", async () => {
    const answer = deferred();
    const refreshPullRequests = vi.fn<Api["refreshPullRequests"]>().mockReturnValue(answer.promise);
    const { section, rows } = await open([makePullRequest("api", 12)], {
      routes: { refreshPullRequests },
    });
    const refresh = find<HTMLButtonElement>(section, 'button[aria-label="Refresh pull requests"]');
    expect(refresh.textContent).toBe("Refresh");

    await click(refresh);
    expect(refreshPullRequests).toHaveBeenCalledWith("s1");
    expect([refresh.textContent, refresh.disabled]).toEqual(["Refreshing…", true]);
    answer.resolve();
    await act();
    expect([refresh.textContent, refresh.disabled]).toEqual(["Refresh", false]);
    // The answer carries nothing: the rows change with the events only.
    expect(rows().map(({ labels }) => labels)).toEqual([["Open"]]);
  });

  it("shows the server's message when the refresh is refused", async () => {
    const refreshPullRequests = vi
      .fn<Api["refreshPullRequests"]>()
      .mockRejectedValueOnce(conflict("GitHub cannot be reached"))
      .mockResolvedValue(undefined);
    const { section } = await open([makePullRequest("api", 12)], {
      routes: { refreshPullRequests },
    });
    const refresh = find<HTMLButtonElement>(section, 'button[aria-label="Refresh pull requests"]');
    await click(refresh);
    expect(find(section, '[role="alert"]').textContent).toBe(
      "The pull requests could not be refreshed: GitHub cannot be reached",
    );
    await click(refresh);
    expect(section.querySelector('[role="alert"]')).toBeNull();
  });

  it("offers to send failing checks or requested changes back to the agent, while shipping", async () => {
    const { section, emit, caughtUp, drafts } = await open([
      makePullRequest("api", 12, { checks: "passing", review: "approved" }),
      makePullRequest("web", 7, { checks: "pending" }),
    ]);
    caughtUp();
    const offer = () => section.textContent.includes("Send back to the agent");
    expect(offer()).toBe(false);

    emit(update(makePullRequest("web", 7, { checks: "failing" })));
    expect(offer()).toBe(true);
    emit(update(makePullRequest("api", 12, { checks: "passing", review: "changes_requested" })));

    await click(findByText(section, "button", "Send back to the agent"));
    expect(drafts).toEqual([
      [
        "These pull requests need another pass:",
        "- api: changes were requested on https://github.com/acme/api/pull/12",
        "- web: checks are failing on https://github.com/acme/web/pull/7",
        "Please look at what failed and fix it.",
      ].join("\n"),
    ]);

    // A closed pull request asks nothing of the agent.
    emit(update(makePullRequest("web", 7, { state: "closed", checks: "failing" })));
    emit(update(makePullRequest("api", 12, { state: "merged", review: "changes_requested" })));
    expect(offer()).toBe(false);
  });

  it.each(["review", "running", "done"] as const)(
    "does not offer it while the session is %s",
    async (status) => {
      const { section } = await open([makePullRequest("api", 12, { checks: "failing" })], {
        status,
      });
      expect(section.textContent).not.toContain("Send back to the agent");
    },
  );
});
