import { For } from "solid-js";
import type { PlanEntry } from "../gen/PlanEntry";
import { planLook } from "../lib/status";

/** The agent's current plan, folded to one line until opened. */
export function PlanPanel(props: { entries: readonly PlanEntry[] }) {
  const done = (): number => props.entries.filter((entry) => entry.status === "completed").length;
  const current = (): string | undefined =>
    props.entries.find((entry) => entry.status === "in_progress")?.content;

  return (
    <details class="group border-t border-line bg-surface">
      <summary class="flex min-h-11 items-center gap-2 px-3 text-sm">
        <span aria-hidden="true" class="text-muted transition-transform group-open:rotate-90">
          ▸
        </span>
        <span class="font-medium">Plan</span>
        <span class="shrink-0 text-muted">
          {done()} of {props.entries.length} done
        </span>
        <span class="truncate group-open:hidden">{current()}</span>
      </summary>
      <ol class="max-h-48 space-y-1 overflow-y-auto px-3 pb-3 text-sm">
        <For each={props.entries} keyed={false}>
          {(entry) => (
            <li
              class={[
                "flex gap-2",
                {
                  "text-muted": entry().status === "completed",
                  "font-medium": entry().status === "in_progress",
                },
              ]}
            >
              <span aria-hidden="true" class="font-mono">
                {planLook(entry().status).mark}
              </span>
              <span class="sr-only">{planLook(entry().status).label}:</span>
              <span class="min-w-0 wrap-break-word">{entry().content}</span>
            </li>
          )}
        </For>
      </ol>
    </details>
  );
}
