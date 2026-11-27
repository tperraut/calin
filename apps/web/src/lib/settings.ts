import type { Settings } from "../gen/Settings";

export type SettingsField = "home_repo" | "scan_roots" | "clone_root" | "max_parallel";

/** The field a refusal is about, and for the scan roots which one, when the message names it. */
export type SettingsBlame = { field: SettingsField; scanRoot?: string };

const NAMED: ReadonlyArray<[RegExp, SettingsField]> = [
  [/home[ _-]?repo/, "home_repo"],
  [/scan[ _-]?root/, "scan_roots"],
  [/clone[ _-]?root/, "clone_root"],
  [/(max[ _-]?)?parallel|at once/, "max_parallel"],
];

/**
 * The field a 400 from `PUT /settings` most plausibly concerns. The contract
 * gives a message only (api.rs), so this reads it: a field named in words
 * first, then the longest path that was sent and is quoted back, then what the
 * rule that failed is about. `undefined` when nothing points anywhere.
 */
export function blameSettingsField(message: string, sent: Settings): SettingsBlame | undefined {
  const text = message.toLowerCase();
  const quotedRoot = sent.scan_roots
    .filter((root) => root !== "" && message.includes(root))
    .toSorted((a, b) => b.length - a.length)
    .at(0);
  // The field named first is the subject: "scan root /x is inside the home repo" is about the scan root.
  const named = NAMED.map(([pattern, field]) => ({ field, at: text.search(pattern) }))
    .filter((one) => one.at !== -1)
    .toSorted((a, b) => a.at - b.at)
    .at(0);
  if (named) {
    return named.field === "scan_roots"
      ? { field: named.field, scanRoot: quotedRoot }
      : { field: named.field };
  }

  const paths: Array<{ path: string; blame: SettingsBlame }> = [];
  if (sent.home_repo) paths.push({ path: sent.home_repo, blame: { field: "home_repo" } });
  if (sent.clone_root) paths.push({ path: sent.clone_root, blame: { field: "clone_root" } });
  for (const root of sent.scan_roots) {
    if (root !== "") paths.push({ path: root, blame: { field: "scan_roots", scanRoot: root } });
  }
  const quoted = paths
    .filter((one) => message.includes(one.path))
    .toSorted((a, b) => b.path.length - a.path.length);
  const [longest, next] = quoted;
  // Two fields holding the same path say nothing about which one is meant.
  if (longest && (!next || next.path.length < longest.path.length)) return longest.blame;

  // api.rs: the home repo must be the root of a git work tree, a scan root an existing directory.
  if (/work ?tree|git/.test(text)) return { field: "home_repo" };
  if (/director|folder/.test(text) && sent.scan_roots.length > 0) return { field: "scan_roots" };
  return undefined;
}

/** What the form sends: trimmed, without blanks or repeats, an empty path as "not set". */
export function settingsFromForm(form: {
  homeRepo: string;
  scanRoots: readonly string[];
  cloneRoot: string;
  /** As typed; empty for the server's default. The server says what it does not accept. */
  maxParallel?: string;
}): Settings {
  const roots: string[] = [];
  for (const root of form.scanRoots) {
    const path = root.trim();
    if (path !== "" && !roots.includes(path)) roots.push(path);
  }
  return {
    home_repo: form.homeRepo.trim() === "" ? null : form.homeRepo.trim(),
    scan_roots: roots,
    clone_root: form.cloneRoot.trim() === "" ? null : form.cloneRoot.trim(),
    max_parallel: parallelOf(form.maxParallel ?? ""),
  };
}

/** The number typed, or `null` for none; a value that is not a number is the server's to refuse. */
function parallelOf(typed: string): number | null {
  const text = typed.trim();
  if (text === "") return null;
  const value = Number(text);
  return Number.isFinite(value) ? value : null;
}
