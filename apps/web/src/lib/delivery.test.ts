import { describe, expect, it } from "vite-plus/test";
import type { ChangedFile } from "../gen/ChangedFile";
import type { PullRequest } from "../gen/PullRequest";
import type { RepoChanges } from "../gen/RepoChanges";
import { makePullRequest, makeSessionRepo } from "../test/fakes";
import {
  checkLook,
  mergePullRequests,
  needsAttention,
  pullRequestHref,
  pullRequestStateLook,
  repoNames,
  reviewLook,
  rollUp,
  sendBackMessage,
  shipPlan,
  skippedLine,
  stepLine,
} from "./delivery";

const file = (path: string): ChangedFile => ({
  path,
  old_path: null,
  status: "modified",
  added: 1,
  removed: 1,
  binary: false,
});

const changed = (id: string, files: number, added = 0, removed = 0): RepoChanges => ({
  session_repo_id: id,
  name: id,
  branch: null,
  base_ref: null,
  files: Array.from({ length: files }, (_, index) => file(`f${index}.rs`)),
  added,
  removed,
});

const repos = [
  makeSessionRepo("api", { primary: true }),
  makeSessionRepo("web"),
  makeSessionRepo("docs", { mode: "read" }),
  makeSessionRepo("notes", { mode: "live" }),
];
const nameOf = repoNames(repos);

describe("shipPlan", () => {
  it("ships every changed write repo, in repo-set order, into a new pull request", () => {
    const plan = shipPlan(repos, [changed("web", 1, 5, 0), changed("api", 3, 40, 12)], []);
    expect(plan.steps.map(stepLine)).toEqual([
      "api: 3 files, +40 −12 → new pull request",
      "web: 1 file, +5 −0 → new pull request",
    ]);
  });

  it("updates the pull request a repo already has, while that one is still in play", () => {
    const plan = shipPlan(
      repos,
      [changed("api", 3, 40, 12), changed("web", 2, 1, 1)],
      [makePullRequest("api", 12, { state: "draft" }), makePullRequest("web", 7)],
    );
    expect(plan.steps.map(stepLine)).toEqual([
      "api: 3 files, +40 −12 → updates #12",
      "web: 2 files, +1 −1 → updates #7",
    ]);
  });

  it.each(["merged", "closed"] as const)(
    "opens a new one when the repo's pull request is %s",
    (state) => {
      const plan = shipPlan(
        repos,
        [changed("api", 1, 1, 0)],
        [makePullRequest("api", 12, { state })],
      );
      expect(plan.steps.map(stepLine)).toEqual(["api: 1 file, +1 −0 → new pull request"]);
    },
  );

  it("names what is skipped and why: no changes, edited in place, read only", () => {
    const plan = shipPlan(
      repos,
      // The changes of a repo that is not a worktree never ship, whatever they are.
      [changed("api", 2, 3, 1), changed("web", 0), changed("docs", 4), changed("notes", 1)],
      [],
    );
    expect(plan.steps.map((step) => step.repo.name)).toEqual(["api"]);
    expect(plan.skipped.map(({ repo, reason }) => [repo.name, reason])).toEqual([
      ["web", "no_changes"],
      ["docs", "read"],
      ["notes", "live"],
    ]);
    expect(skippedLine(plan)).toBe(
      "Skipped: web (no changes), docs (read only), notes (edited in place)",
    );
  });

  it("counts a write repo the changes list does not hold as unchanged, and says nothing when all ship", () => {
    const missing = shipPlan([makeSessionRepo("api")], [], []);
    expect(missing.steps).toEqual([]);
    expect(skippedLine(missing)).toBe("Skipped: api (no changes)");
    expect(skippedLine(shipPlan([makeSessionRepo("api")], [changed("api", 1)], []))).toBe("");
  });
});

describe("rollUp", () => {
  const pr = (repo: string, number: number, patch: Partial<PullRequest> = {}) =>
    makePullRequest(repo, number, patch);

  it.each<[string, PullRequest[], string]>([
    ["one just opened", [pr("api", 12)], "1 pull request"],
    [
      "one draft whose checks run",
      [pr("api", 12, { state: "draft", checks: "pending" })],
      "1 pull request · 1 draft · checks pending on api",
    ],
    [
      "one merged, checks failing on the other",
      [pr("api", 12, { state: "merged", checks: "passing" }), pr("web", 7, { checks: "failing" })],
      "2 pull requests · 1 merged · checks failing on web",
    ],
    [
      "both kinds of trouble, each by repo",
      [pr("api", 12, { review: "changes_requested" }), pr("web", 7, { checks: "failing" })],
      "2 pull requests · checks failing on web · changes requested on api",
    ],
    [
      "the same trouble on two repos",
      [pr("api", 12, { checks: "failing" }), pr("web", 7, { checks: "failing" })],
      "2 pull requests · checks failing on api and web",
    ],
    [
      "trouble said, what is fine left out",
      [
        pr("api", 12, { checks: "passing", review: "approved" }),
        pr("web", 7, { checks: "failing", review: "approved" }),
      ],
      "2 pull requests · checks failing on web",
    ],
    [
      "a merged one with failed checks needs nobody",
      [pr("api", 12, { state: "merged", checks: "failing", review: "changes_requested" })],
      "1 pull request · merged",
    ],
    [
      "all merged",
      [pr("api", 12, { state: "merged" }), pr("web", 7, { state: "merged" })],
      "2 pull requests · all merged",
    ],
    [
      "merged and closed",
      [pr("api", 12, { state: "merged" }), pr("web", 7, { state: "closed" })],
      "2 pull requests · 1 merged · 1 closed",
    ],
    [
      "nothing wrong: what is fine is said",
      [
        pr("api", 12, { checks: "passing", review: "approved" }),
        pr("web", 7, { checks: "passing", review: "approved" }),
      ],
      "2 pull requests · checks passing · approved",
    ],
    [
      "one of two approved, checks still running on one",
      [
        pr("api", 12, { checks: "passing", review: "approved" }),
        pr("web", 7, { checks: "pending" }),
      ],
      "2 pull requests · checks pending on web · 1 approved",
    ],
    [
      "two drafts",
      [pr("api", 12, { state: "draft" }), pr("web", 7, { state: "draft" })],
      "2 pull requests · 2 drafts",
    ],
  ])("%s", (_case, pullRequests, expected) => {
    expect(rollUp(pullRequests, nameOf)).toBe(expected);
  });

  it("falls back to the number for a repo the session does not list", () => {
    expect(rollUp([pr("ghost", 3, { checks: "failing" })], nameOf)).toBe(
      "1 pull request · checks failing on #3",
    );
  });
});

describe("pull request helpers", () => {
  it("links a web address, and nothing else", () => {
    expect(pullRequestHref("https://github.com/acme/api/pull/12")).toBe(
      "https://github.com/acme/api/pull/12",
    );
    // api.rs, "Dry run": made-up pull requests lead nowhere.
    expect(pullRequestHref("https://example.invalid/acme/api/pull/1")).toBeUndefined();
    expect(pullRequestHref("https://pulls.example.invalid/1")).toBeUndefined();
    expect(pullRequestHref("javascript:alert(1)")).toBeUndefined();
    expect(pullRequestHref("not a url")).toBeUndefined();
  });

  it("shows a state, a check result or a review a newer server may send under its wire name", () => {
    expect(pullRequestStateLook("queued" as PullRequest["state"]).label).toBe("queued");
    expect(checkLook("skipped" as PullRequest["checks"])?.label).toBe("skipped");
    expect(reviewLook("dismissed" as PullRequest["review"])?.label).toBe("dismissed");
    expect(checkLook("none")).toBeUndefined();
    expect(reviewLook("none")).toBeUndefined();
  });

  it("needs the reader only while the pull request is in play", () => {
    expect(needsAttention(makePullRequest("api", 1, { checks: "failing" }))).toBe(true);
    expect(needsAttention(makePullRequest("api", 1, { review: "changes_requested" }))).toBe(true);
    expect(needsAttention(makePullRequest("api", 1, { checks: "pending" }))).toBe(false);
    expect(needsAttention(makePullRequest("api", 1, { state: "closed", checks: "failing" }))).toBe(
      false,
    );
  });

  it("puts the record's pull requests and the events' in repo-set order, the events' winning once caught up", () => {
    const seed = [makePullRequest("web", 7, { updated_at: 500 }), makePullRequest("api", 12)];
    const older = makePullRequest("web", 7, { state: "draft", updated_at: 100 });
    // Mid-replay the fold passes through old values: the record's newer one stays.
    expect(mergePullRequests(seed, [older], repos, false).map((one) => one.state)).toEqual([
      "open",
      "open",
    ]);
    expect(mergePullRequests(seed, [older], repos, true).map((one) => one.state)).toEqual([
      "open",
      "draft",
    ]);
    const merged = mergePullRequests(
      [],
      [makePullRequest("ghost", 1), makePullRequest("web", 7), makePullRequest("api", 12)],
      repos,
      true,
    );
    expect(merged.map((one) => one.session_repo_id)).toEqual(["api", "web", "ghost"]);
  });

  it("writes the agent a message naming each repo, its pull request and what failed", () => {
    expect(
      sendBackMessage(
        [
          makePullRequest("api", 12, { checks: "failing", review: "changes_requested" }),
          makePullRequest("web", 7, { checks: "failing" }),
          makePullRequest("docs", 3, { checks: "passing" }),
        ],
        nameOf,
      ),
    ).toBe(
      [
        "These pull requests need another pass:",
        "- api: checks are failing and changes were requested on https://github.com/acme/api/pull/12",
        "- web: checks are failing on https://github.com/acme/web/pull/7",
        "Please look at what failed and fix it.",
      ].join("\n"),
    );
    expect(
      sendBackMessage([makePullRequest("api", 12, { review: "changes_requested" })], nameOf),
    ).toBe(
      [
        "This pull request needs another pass:",
        "- api: changes were requested on https://github.com/acme/api/pull/12",
        "Please look at what failed and fix it.",
      ].join("\n"),
    );
    expect(sendBackMessage([makePullRequest("api", 12)], nameOf)).toBe("");
  });
});
