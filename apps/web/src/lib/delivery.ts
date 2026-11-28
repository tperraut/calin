import type { CheckState } from "../gen/CheckState";
import type { PullRequest } from "../gen/PullRequest";
import type { PullRequestState } from "../gen/PullRequestState";
import type { RepoChanges } from "../gen/RepoChanges";
import type { ReviewState } from "../gen/ReviewState";
import type { SessionRepo } from "../gen/SessionRepo";
import { counted, filesLabel, listInWords } from "./format";
import type { Look } from "./status";

// Keyed by the generated unions, so a value added to the contract fails the type-check here.
const stateLooks: Record<PullRequestState, Look> = {
  draft: { label: "Draft", tone: "neutral" },
  open: { label: "Open", tone: "active" },
  merged: { label: "Merged", tone: "success" },
  closed: { label: "Closed", tone: "neutral" },
};

// `none` is not in these two: nothing reported shows as nothing.
const checkLooks: Record<Exclude<CheckState, "none">, Look> = {
  pending: { label: "Checks pending", tone: "attention" },
  passing: { label: "Checks passing", tone: "success" },
  failing: { label: "Checks failing", tone: "danger" },
};

const reviewLooks: Record<Exclude<ReviewState, "none">, Look> = {
  approved: { label: "Approved", tone: "success" },
  changes_requested: { label: "Changes requested", tone: "attention" },
};

// A value a newer server may send is shown under its wire name instead of breaking the screen.
const lookup = (table: Record<string, Look>, key: string): Look =>
  table[key] ?? { label: key, tone: "neutral" };

export const pullRequestStateLook = (state: PullRequestState): Look => lookup(stateLooks, state);

export const checkLook = (checks: CheckState): Look | undefined =>
  checks === "none" ? undefined : lookup(checkLooks, checks);

export const reviewLook = (review: ReviewState): Look | undefined =>
  review === "none" ? undefined : lookup(reviewLooks, review);

/** Merged or closed: nothing more is expected of the pull request. */
export const isSettled = (pullRequest: PullRequest): boolean =>
  pullRequest.state === "merged" || pullRequest.state === "closed";

/** What the reader has to act on: only a pull request still in play can need it. */
export const needsAttention = (pullRequest: PullRequest): boolean =>
  !isSettled(pullRequest) &&
  (pullRequest.checks === "failing" || pullRequest.review === "changes_requested");

/**
 * The address to link a pull request to, or `undefined` when there is none to
 * follow: dry run records made-up ones under `example.invalid` (api.rs, "Dry
 * run"), and nothing but a web address is ever made a link.
 */
export function pullRequestHref(url: string): string | undefined {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return undefined;
  }
  if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return undefined;
  if (parsed.hostname === "invalid" || parsed.hostname.endsWith(".invalid")) return undefined;
  return url;
}

/**
 * In the order of the session's repo set (primary first); those of a repo it
 * does not list keep their order, after.
 */
export function inRepoSetOrder<T extends { session_repo_id: string }>(
  items: readonly T[],
  repos: readonly SessionRepo[],
): T[] {
  const rank = new Map(repos.map((repo, index) => [repo.id, index]));
  const position = (one: T): number => rank.get(one.session_repo_id) ?? repos.length;
  return items.toSorted((a, b) => position(a) - position(b));
}

/**
 * The session's pull requests: what the REST record listed, brought up to date
 * by the events folded since, in repo-set order. Mid-replay the fold passes
 * through every past value, so the newer of the two is kept until `caughtUp`.
 */
export function mergePullRequests(
  seed: readonly PullRequest[],
  fromEvents: readonly PullRequest[],
  repos: readonly SessionRepo[],
  caughtUp: boolean,
): PullRequest[] {
  const byRepo = new Map(seed.map((one) => [one.session_repo_id, one]));
  for (const one of fromEvents) {
    const listed = byRepo.get(one.session_repo_id);
    if (caughtUp || !listed || one.updated_at >= listed.updated_at) {
      byRepo.set(one.session_repo_id, one);
    }
  }
  return inRepoSetOrder([...byRepo.values()], repos);
}

export type RepoNames = (sessionRepoId: string) => string | undefined;

export const repoNames =
  (repos: readonly SessionRepo[]): RepoNames =>
  (sessionRepoId) =>
    repos.find((repo) => repo.id === sessionRepoId)?.name;

/** Why a repo of the session takes no part in shipping (api.rs, "Shipping"). */
export type SkipReason = "no_changes" | "live" | "read";

export type ShipStep = {
  repo: SessionRepo;
  files: number;
  added: number;
  removed: number;
  /** The pull request new commits go to; absent when one will be opened. */
  updates: PullRequest | undefined;
};

/** What `POST /ship` will do, repo by repo, in repo-set order. */
export type ShipPlan = {
  steps: readonly ShipStep[];
  skipped: ReadonlyArray<{ repo: SessionRepo; reason: SkipReason }>;
};

/**
 * Every `write` repo that differs from its base is shipped: into its pull
 * request while that one is still in play, into a new one otherwise. `changes`
 * is the server's list; a repo it does not hold counts as unchanged.
 */
export function shipPlan(
  repos: readonly SessionRepo[],
  changes: readonly RepoChanges[],
  pullRequests: readonly PullRequest[],
): ShipPlan {
  const steps: ShipStep[] = [];
  const skipped: Array<{ repo: SessionRepo; reason: SkipReason }> = [];
  for (const repo of repos) {
    if (repo.mode !== "write") {
      skipped.push({ repo, reason: repo.mode === "live" ? "live" : "read" });
      continue;
    }
    const changed = changes.find((one) => one.session_repo_id === repo.id);
    if (!changed || changed.files.length === 0) {
      skipped.push({ repo, reason: "no_changes" });
      continue;
    }
    const existing = pullRequests.find((one) => one.session_repo_id === repo.id);
    steps.push({
      repo,
      files: changed.files.length,
      added: changed.added,
      removed: changed.removed,
      updates: existing && !isSettled(existing) ? existing : undefined,
    });
  }
  return { steps, skipped };
}

/** "api: 3 files, +40 −12 → new pull request", or "→ updates #12". */
export const stepLine = (step: ShipStep): string =>
  `${step.repo.name}: ${filesLabel(step.files)}, +${step.added} −${step.removed} → ` +
  (step.updates ? `updates #${step.updates.number}` : "new pull request");

const skipWords: Record<SkipReason, string> = {
  no_changes: "no changes",
  live: "edited in place",
  read: "read only",
};

/** "Skipped: docs (read only), notes (edited in place)"; empty when every repo ships. */
export function skippedLine(plan: ShipPlan): string {
  if (plan.skipped.length === 0) return "";
  const each = plan.skipped.map(({ repo, reason }) => `${repo.name} (${skipWords[reason]})`);
  return `Skipped: ${each.join(", ")}`;
}

const inState = (pullRequests: readonly PullRequest[], state: PullRequestState): number =>
  pullRequests.filter((one) => one.state === state).length;

/** "the part" when `count` is above zero, nothing otherwise: for spreading into a list of parts. */
const partIf = (count: number, part: string): string[] => (count > 0 ? [part] : []);

/** "api and web": the repos of `list` by name, or by number when a repo has none. */
const reposInWords = (list: readonly PullRequest[], nameOf: RepoNames): string =>
  listInWords(list.map((one) => nameOf(one.session_repo_id) ?? `#${one.number}`));

/** How many are merged, closed and drafts, when not all are merged. */
function stateParts(pullRequests: readonly PullRequest[]): string[] {
  const merged = inState(pullRequests, "merged");
  const closed = inState(pullRequests, "closed");
  const drafts = inState(pullRequests, "draft");
  return [
    ...partIf(merged, `${merged} merged`),
    ...partIf(closed, `${closed} closed`),
    ...partIf(drafts, counted(drafts, "draft", "drafts")),
  ];
}

/** What needs the reader on the pull requests still in play, by repo. */
function needParts(inPlay: readonly PullRequest[], nameOf: RepoNames): string[] {
  const failing = inPlay.filter((one) => one.checks === "failing");
  const contested = inPlay.filter((one) => one.review === "changes_requested");
  return [
    ...partIf(failing.length, `checks failing on ${reposInWords(failing, nameOf)}`),
    ...partIf(contested.length, `changes requested on ${reposInWords(contested, nameOf)}`),
  ];
}

/** What is merely fine, or still on its way, when nothing needs the reader. */
function fineParts(inPlay: readonly PullRequest[], nameOf: RepoNames): string[] {
  const pending = inPlay.filter((one) => one.checks === "pending");
  const checks =
    pending.length > 0
      ? [`checks pending on ${reposInWords(pending, nameOf)}`]
      : inPlay.some((one) => one.checks === "passing")
        ? ["checks passing"]
        : [];
  const approved = inPlay.filter((one) => one.review === "approved").length;
  const review = approved === inPlay.length ? "approved" : `${approved} approved`;
  return [...checks, ...partIf(approved, review)];
}

/**
 * The session's pull requests in one line of plain words, for example
 * "2 pull requests · 1 merged · checks failing on web". What needs the reader
 * (failing checks, requested changes) is always said, by repo; what is merely
 * fine is said only when nothing does.
 */
export function rollUp(pullRequests: readonly PullRequest[], nameOf: RepoNames): string {
  const total = pullRequests.length;
  if (total === 0) return "No pull request";
  const head = counted(total, "pull request", "pull requests");
  if (inState(pullRequests, "merged") === total) {
    return [head, total === 1 ? "merged" : "all merged"].join(" · ");
  }
  const inPlay = pullRequests.filter((one) => !isSettled(one));
  const needs = needParts(inPlay, nameOf);
  const status = needs.length > 0 ? needs : fineParts(inPlay, nameOf);
  return [head, ...stateParts(pullRequests), ...status].join(" · ");
}

/**
 * A message for the agent about the pull requests that need work: the repo,
 * the address and what failed, one line each. Empty when none needs any. The
 * reader edits it before sending.
 */
export function sendBackMessage(pullRequests: readonly PullRequest[], nameOf: RepoNames): string {
  const lines = pullRequests.filter(needsAttention).map((one) => {
    const what = [
      ...(one.checks === "failing" ? ["checks are failing"] : []),
      ...(one.review === "changes_requested" ? ["changes were requested"] : []),
    ];
    const repo = nameOf(one.session_repo_id) ?? `pull request #${one.number}`;
    return `- ${repo}: ${what.join(" and ")} on ${one.url}`;
  });
  if (lines.length === 0) return "";
  const lead =
    lines.length === 1
      ? "This pull request needs another pass:"
      : "These pull requests need another pass:";
  return `${lead}\n${lines.join("\n")}\nPlease look at what failed and fix it.`;
}
