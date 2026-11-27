import { Show } from "solid-js";
import type { Session } from "../gen/Session";

const chip =
  "inline-flex min-h-11 min-w-11 shrink-0 items-center justify-center gap-1 px-1.5 text-xs font-medium -outline-offset-2";

/**
 * Where a session came from: the issue it was started from, as "#12" linking
 * to the issue on GitHub, and the automation that started it. Nothing for a
 * session started by hand.
 */
export function SessionOrigin(props: { session: Session; class?: string }) {
  return (
    <Show when={props.session.issue !== null || props.session.automation_id !== null}>
      <span class={["flex shrink-0 items-center", props.class]}>
        <Show when={props.session.issue}>
          {(issue) => (
            <a
              href={issue().url}
              target="_blank"
              rel="noopener noreferrer"
              data-part="issue"
              class={chip}
              title={`Issue #${issue().number}: ${issue().title}`}
              aria-label={`Issue #${issue().number}: ${issue().title} (opens GitHub)`}
            >
              <span class="rounded-full border border-line px-2 py-0.5 font-mono text-fg">
                #{issue().number}
              </span>
            </a>
          )}
        </Show>
        <Show when={props.session.automation_id}>
          {(id) => (
            <a
              href={`/automations/${encodeURIComponent(id())}`}
              data-part="automation"
              class={chip}
              title="Started by an automation"
              aria-label="Started by an automation: open it"
            >
              <span class="rounded-full border border-line px-2 py-0.5 text-muted">automation</span>
            </a>
          )}
        </Show>
      </span>
    </Show>
  );
}
