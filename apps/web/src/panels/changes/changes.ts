import type { ChangedFile } from "../../gen/ChangedFile";
import type { ChangeStatus } from "../../gen/ChangeStatus";
import type { FileDiff } from "../../gen/FileDiff";
import type { RepoChanges } from "../../gen/RepoChanges";
import type { SessionRepo } from "../../gen/SessionRepo";
import type { Tone } from "../../lib/status";
import type { FileTarget } from "../registry";

export type Totals = { files: number; added: number; removed: number };

/** Totals of one repo. Lines are the server's own count: a binary file has none. */
export const repoTotals = (repo: RepoChanges): Totals => ({
  files: repo.files.length,
  added: repo.added,
  removed: repo.removed,
});

/** Totals across the session, and how many of its repos have changes at all. */
export function sessionTotals(repos: readonly RepoChanges[]): Totals & { changedRepos: number } {
  const total = { files: 0, added: 0, removed: 0, changedRepos: 0 };
  for (const repo of repos) {
    total.files += repo.files.length;
    total.added += repo.added;
    total.removed += repo.removed;
    if (repo.files.length > 0) total.changedRepos += 1;
  }
  return total;
}

/** In the order of the session's repo set (primary first); repos it does not list keep the server's order, after. */
export function inRepoOrder(
  changes: readonly RepoChanges[],
  known: readonly SessionRepo[],
): RepoChanges[] {
  const rank = new Map(known.map((repo, index) => [repo.id, index]));
  const position = (repo: RepoChanges): number => rank.get(repo.session_repo_id) ?? known.length;
  return changes.toSorted((a, b) => position(a) - position(b));
}

export function findFile(
  changes: readonly RepoChanges[],
  target: FileTarget,
): ChangedFile | undefined {
  return changes
    .find((repo) => repo.session_repo_id === target.sessionRepoId)
    ?.files.find((file) => file.path === target.path);
}

export type StatusLook = { letter: string; label: string; tone: Tone };

const statusLooks: Record<ChangeStatus, StatusLook> = {
  added: { letter: "A", label: "added", tone: "success" },
  modified: { letter: "M", label: "modified", tone: "active" },
  deleted: { letter: "D", label: "deleted", tone: "danger" },
  renamed: { letter: "R", label: "renamed", tone: "attention" },
  untracked: { letter: "U", label: "untracked", tone: "success" },
};

/** A status a newer server may send is shown under its wire name. */
export const statusLook = (status: ChangeStatus): StatusLook =>
  (statusLooks as Record<string, StatusLook | undefined>)[status] ?? {
    letter: "?",
    label: status,
    tone: "neutral",
  };

export const filesLabel = (count: number): string => `${count} ${count === 1 ? "file" : "files"}`;

/** What a diff has to show, decided once from the wire shape (repo.rs, `FileDiff`). */
export type DiffContent =
  | { kind: "binary" }
  | { kind: "too_large" }
  | { kind: "empty" }
  | { kind: "single"; side: "added" | "deleted"; text: string }
  | { kind: "both"; original: string; modified: string };

export function diffContent(diff: FileDiff): DiffContent {
  if (diff.binary) return { kind: "binary" };
  if (diff.too_large) return { kind: "too_large" };
  if (diff.old === null && diff.new === null) return { kind: "empty" };
  if (diff.old === null) return { kind: "single", side: "added", text: diff.new ?? "" };
  if (diff.new === null) return { kind: "single", side: "deleted", text: diff.old };
  return { kind: "both", original: diff.old, modified: diff.new };
}

export const sameDiff = (a: FileDiff, b: FileDiff): boolean =>
  a.path === b.path &&
  a.old_path === b.old_path &&
  a.old === b.old &&
  a.new === b.new &&
  a.binary === b.binary &&
  a.too_large === b.too_large;

export type Refresher = {
  /** Something changed: refresh soon, once, however many times this is called meanwhile. */
  request(): void;
  /** Refresh at once (the panel opened, the reader asked). */
  now(): void;
  /** While not visible nothing is fetched; what was missed is fetched on the way back. */
  setVisible(visible: boolean): void;
  dispose(): void;
};

/** Debounces and serializes a refresh, and holds it while its screen is out of sight. */
export function createRefresher(run: () => Promise<void>, delayMs = 500): Refresher {
  let visible = false;
  let stale = false;
  let running = false;
  let again = false;
  let disposed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;

  function start(): void {
    if (disposed) return;
    if (running) {
      again = true;
      return;
    }
    running = true;
    void run().finally(() => {
      running = false;
      if (!again) return;
      again = false;
      if (visible) start();
      else stale = true;
    });
  }

  function cancel(): boolean {
    if (timer === undefined) return false;
    clearTimeout(timer);
    timer = undefined;
    return true;
  }

  return {
    request() {
      if (!visible) {
        stale = true;
        return;
      }
      cancel();
      timer = setTimeout(() => {
        timer = undefined;
        start();
      }, delayMs);
    },
    now() {
      cancel();
      if (visible) start();
      else stale = true;
    },
    setVisible(next) {
      visible = next;
      if (!next) {
        if (cancel()) stale = true;
      } else if (stale) {
        stale = false;
        start();
      }
    },
    dispose() {
      disposed = true;
      cancel();
    },
  };
}
