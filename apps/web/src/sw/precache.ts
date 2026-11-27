// The build step behind the service worker's pre-cache list. Runs in Node from `vite.config.ts`
// and in the tests; it reads a description of the bundle, never the file system.

/** What the build says about one output file (a subset of Rollup's chunk and asset). */
export type OutputFile =
  | {
      type: "chunk";
      fileName: string;
      /** True for the app's entry only (not a development page such as the stress page). */
      isEntry: boolean;
      /** Static imports only: what must load before the entry runs. */
      imports: readonly string[];
      importedCss: readonly string[];
      importedAssets: readonly string[];
    }
  | { type: "asset"; fileName: string };

/** The page every navigation gets; the server answers any app route with it. */
export const SHELL_PAGE = "index.html";
/** The worker's own file, which must never be in its cache. */
export const WORKER_FILE = "sw.js";

/** Files copied as they are from `public/` that the installed app shows before any API call. */
const PUBLIC_SHELL =
  /^(favicon\.svg|manifest\.webmanifest|apple-touch-icon\.png|icons\/[^/]+\.png)$/;

/**
 * Root-relative URLs of what first paint needs: the page, the entry chunk and
 * what it imports statically, their styles and assets, and the icons. Chunks
 * loaded on demand are left to be cached on first use. Sorted, so the list
 * (and so the worker) only changes when the build does.
 */
export function precacheList(
  files: readonly OutputFile[],
  publicFiles: readonly string[],
): string[] {
  const byName = new Map(files.map((file) => [file.fileName, file]));
  const wanted = new Set<string>();
  if (byName.has(SHELL_PAGE)) wanted.add(SHELL_PAGE);

  const visit = (name: string): void => {
    if (wanted.has(name)) return;
    const file = byName.get(name);
    if (!file) return;
    wanted.add(name);
    if (file.type !== "chunk") return;
    for (const imported of file.imports) visit(imported);
    for (const css of file.importedCss) wanted.add(css);
    for (const asset of file.importedAssets) wanted.add(asset);
  };
  for (const file of files) {
    if (file.type === "chunk" && file.isEntry) visit(file.fileName);
  }
  for (const name of publicFiles) if (PUBLIC_SHELL.test(name)) wanted.add(name);

  return [...wanted]
    .filter((name) => !name.endsWith(".map") && name !== WORKER_FILE)
    .map((name) => `/${name}`)
    .sort();
}
