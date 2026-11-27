import type { PermissionMode } from "../gen/PermissionMode";
import type { RepoMode } from "../gen/RepoMode";
import { permissionModes } from "./status";

/** The two ways the composer attaches a repo; `read` is not offered there. */
export type ComposerRepoMode = Extract<RepoMode, "write" | "live">;

export type RepoChoice = { repoId: string; mode: ComposerRepoMode };

/** What the composer starts from: the choices of the last session started in this browser. */
export type ComposerPrefs = {
  /** In order; the first is the primary repo. */
  repos: RepoChoice[];
  permissionMode: PermissionMode;
  /** Absent until a runner was used: the server's default applies. */
  runner?: string;
};

export const defaultComposerPrefs = (): ComposerPrefs => ({ repos: [], permissionMode: "ask" });

const COMPOSER_KEY = "calin.composer.v1";
const SIDE_PANE_KEY = "calin.sidePane.v1";

// Storage can be missing, full or forbidden (private windows, blocked site
// data). Every access is guarded: the app works the same, it just forgets.
function read(key: string): unknown {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? undefined : JSON.parse(raw);
  } catch {
    return undefined;
  }
}

function write(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Nothing to do: the next visit starts from the defaults.
  }
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

function repoChoice(value: unknown): RepoChoice | undefined {
  if (!isRecord(value) || typeof value.repoId !== "string") return undefined;
  return { repoId: value.repoId, mode: value.mode === "live" ? "live" : "write" };
}

/** Whatever is stored is checked field by field: it may come from another build. */
export function loadComposerPrefs(): ComposerPrefs {
  const stored = read(COMPOSER_KEY);
  const prefs = defaultComposerPrefs();
  if (!isRecord(stored)) return prefs;
  if (Array.isArray(stored.repos)) {
    const seen = new Set<string>();
    for (const value of stored.repos) {
      const choice = repoChoice(value);
      if (!choice || seen.has(choice.repoId)) continue;
      seen.add(choice.repoId);
      prefs.repos.push(choice);
    }
  }
  const mode = permissionModes.find((one) => one === stored.permissionMode);
  if (mode) prefs.permissionMode = mode;
  if (typeof stored.runner === "string" && stored.runner !== "") prefs.runner = stored.runner;
  return prefs;
}

export function saveComposerPrefs(prefs: ComposerPrefs): void {
  write(COMPOSER_KEY, prefs);
}

/** Width of the side pane in pixels, when the reader has resized it. */
export function loadSidePaneWidth(): number | undefined {
  const stored = read(SIDE_PANE_KEY);
  return typeof stored === "number" && Number.isFinite(stored) && stored > 0 ? stored : undefined;
}

export function saveSidePaneWidth(width: number): void {
  write(SIDE_PANE_KEY, Math.round(width));
}
