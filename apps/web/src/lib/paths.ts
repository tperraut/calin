import type { SessionRepo } from "../gen/SessionRepo";

const trimmed = (path: string): string => (path.length > 1 ? path.replace(/\/+$/, "") : path);

/**
 * The session repo an absolute path is in, and the path relative to that
 * repo's root (empty for the root itself). The innermost repo wins when one
 * sits inside another. `undefined` for a path outside every repo.
 */
export function locateInRepos(
  absolute: string,
  repos: readonly SessionRepo[],
): { repo: SessionRepo; path: string } | undefined {
  const target = trimmed(absolute);
  let best: { repo: SessionRepo; path: string; depth: number } | undefined;
  for (const repo of repos) {
    const root = trimmed(repo.path);
    if (root === "") continue;
    const inside = target === root || target.startsWith(root.endsWith("/") ? root : `${root}/`);
    if (!inside || (best && best.depth >= root.length)) continue;
    best = { repo, path: target.slice(root.length).replace(/^\/+/, ""), depth: root.length };
  }
  return best && { repo: best.repo, path: best.path };
}
