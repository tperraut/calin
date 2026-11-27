import type { PermissionMode } from "../gen/PermissionMode";
import type { RepoMode } from "../gen/RepoMode";
import { permissionModes } from "./status";

/** The two ways the composer attaches a repo; `read` is not offered there. */
export type ComposerRepoMode = Extract<RepoMode, "write" | "live">;

export type RepoChoice = { repoId: string; mode: ComposerRepoMode };

/** What the composer starts from: the choices of the last session started in this browser. */
export type ComposerPrefs = {
  /**
   * In order; the first is the primary repo. Absent until a session was
   * started here: the composer's own default applies (the home repo, if any).
   */
  repos?: RepoChoice[];
  permissionMode: PermissionMode;
  /** Absent until a runner was used: the server's default applies. */
  runner?: string;
  /** The workflow id last started with; absent for the built-in `quick`. */
  workflow?: string;
};

export const defaultComposerPrefs = (): ComposerPrefs => ({ permissionMode: "ask" });

const COMPOSER_KEY = "calin.composer.v2";
// Written before there was a home repo: its repo list is not a choice made against one, so it is not read.
const COMPOSER_KEY_V1 = "calin.composer.v1";
const SIDE_PANE_KEY = "calin.sidePane.v1";
const DIFF_LAYOUT_KEY = "calin.diffLayout.v1";
const EDITOR_WRAP_KEY = "calin.editorWrap.v1";

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
  const current = read(COMPOSER_KEY);
  const stored = isRecord(current) ? current : read(COMPOSER_KEY_V1);
  const prefs = defaultComposerPrefs();
  if (!isRecord(stored)) return prefs;
  if (isRecord(current) && Array.isArray(current.repos)) {
    const seen = new Set<string>();
    prefs.repos = [];
    for (const value of current.repos) {
      const choice = repoChoice(value);
      if (!choice || seen.has(choice.repoId)) continue;
      seen.add(choice.repoId);
      prefs.repos.push(choice);
    }
  }
  const mode = permissionModes.find((one) => one === stored.permissionMode);
  if (mode) prefs.permissionMode = mode;
  if (typeof stored.runner === "string" && stored.runner !== "") prefs.runner = stored.runner;
  if (typeof stored.workflow === "string" && stored.workflow !== "") {
    prefs.workflow = stored.workflow;
  }
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

export type DiffLayoutChoice = "split" | "unified";

/** The diff layout the reader picked on a wide screen, if they did. */
export function loadDiffLayout(): DiffLayoutChoice | undefined {
  const stored = read(DIFF_LAYOUT_KEY);
  return stored === "split" || stored === "unified" ? stored : undefined;
}

export function saveDiffLayout(layout: DiffLayoutChoice): void {
  write(DIFF_LAYOUT_KEY, layout);
}

/** Whether the editor wraps long lines, when the reader has chosen. */
export function loadEditorWrap(): boolean | undefined {
  const stored = read(EDITOR_WRAP_KEY);
  return typeof stored === "boolean" ? stored : undefined;
}

export function saveEditorWrap(wrap: boolean): void {
  write(EDITOR_WRAP_KEY, wrap);
}
