import type { RepoChanges } from "../gen/RepoChanges";
import type { FileChange } from "../state/sessionReducer";

/**
 * How many files the session changed, for the Changes tab. Before the
 * server's list was read it is the number of files the events named. Once it
 * was, the list is the truth (it sees what a shell did and what was undone),
 * plus the files events have named since that read and the list does not hold yet.
 */
export function changedFileCount(
  list: readonly RepoChanges[] | undefined,
  /** The `changesRevision` the list accounts for. */
  listRevision: number,
  events: readonly FileChange[],
): number {
  if (list === undefined) return events.length;
  const listed = new Set<string>();
  for (const repo of list) {
    // The same shape as a `FileChange` key.
    for (const file of repo.files) listed.add(`${repo.session_repo_id}\n${file.path}`);
  }
  let count = listed.size;
  for (const change of events) {
    if (change.revision <= listRevision || change.sessionRepoId === null) continue;
    if (change.op !== "delete" && !listed.has(change.key)) count += 1;
  }
  return count;
}
