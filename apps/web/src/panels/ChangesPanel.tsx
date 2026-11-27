import { createMemo, For, Show } from "solid-js";
import { fileOpLabel } from "../lib/status";
import type { FileChange } from "../state/sessionReducer";
import type { PanelProps } from "./registry";

type Group = { key: string; name: string; detail: string | null; files: FileChange[] };

const OUTSIDE = "";

/**
 * The files touched so far, live from `file_changed` events, grouped by repo.
 * A list for now; the diff view replaces it.
 */
export default function ChangesPanel(props: PanelProps) {
  const groups = createMemo((): Group[] => {
    const byRepo = new Map<string, FileChange[]>();
    for (const file of props.store.view().changedFiles) {
      const key = file.sessionRepoId ?? OUTSIDE;
      const files = byRepo.get(key);
      if (files) files.push(file);
      else byRepo.set(key, [file]);
    }
    const known = props.store.repos();
    const result: Group[] = [];
    // Repo-set order first (primary on top), then whatever the set does not explain.
    for (const repo of known) {
      const files = byRepo.get(repo.id);
      if (files) result.push({ key: repo.id, name: repo.name, detail: repo.branch, files });
      byRepo.delete(repo.id);
    }
    for (const [key, files] of byRepo) {
      result.push(
        key === OUTSIDE
          ? { key, name: "Outside the session's repos", detail: null, files }
          : { key, name: key, detail: null, files },
      );
    }
    return result;
  });

  return (
    <div class="min-h-0 flex-1 overflow-y-auto overscroll-contain p-3">
      <Show
        when={groups().length > 0}
        fallback={<p class="py-8 text-center text-sm text-muted">No file has been changed yet.</p>}
      >
        <div class="mx-auto flex max-w-3xl flex-col gap-4">
          <For each={groups()} keyed={(group) => group.key}>
            {(group) => (
              <section>
                <h2 class="flex items-baseline gap-2 text-sm font-semibold">
                  <span class="truncate">{group().name}</span>
                  <Show when={group().detail}>
                    {(branch) => (
                      <span class="truncate font-mono text-xs font-normal text-muted">
                        {branch()}
                      </span>
                    )}
                  </Show>
                  <span class="ml-auto shrink-0 text-xs font-normal text-muted">
                    {group().files.length} {group().files.length === 1 ? "file" : "files"}
                  </span>
                </h2>
                <ul class="mt-1 divide-y divide-line rounded-md border border-line bg-surface">
                  <For each={group().files} keyed={(file) => file.key}>
                    {(file) => (
                      <li class="flex min-h-11 items-center gap-2 px-3 py-1.5">
                        <span class="min-w-0 flex-1 font-mono text-sm wrap-break-word">
                          {file().path}
                        </span>
                        <span class="shrink-0 text-xs text-muted">
                          {fileOpLabel(file().op)} by {file().by === "user" ? "you" : "the agent"}
                        </span>
                      </li>
                    )}
                  </For>
                </ul>
              </section>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
