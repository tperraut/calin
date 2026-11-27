import { For, Show } from "solid-js";
import type { Session } from "../gen/Session";
import { absoluteTime, relativeTime } from "../lib/format";
import { sessionPath } from "../panels/registry";
import { SessionStatusBadge } from "./Badge";

/** The sessions as links, in the order given. `now` drives the relative times. */
export function SessionList(props: { sessions: readonly Session[]; now: number }) {
  return (
    <ul class="divide-y divide-line">
      <For each={props.sessions} keyed={(session) => session.id}>
        {(session) => (
          <li>
            {/* The router marks the open session: aria-current on its own address, data-active
                on its panels' as well. The focus ring is drawn inside the row because the
                scrolling list clips what is outside. */}
            <a
              href={sessionPath(session().id)}
              class="flex min-h-11 flex-col gap-1 px-3 py-2.5 -outline-offset-2 hover:bg-sunken data-active:bg-accent-soft"
            >
              <span class="flex items-center justify-between gap-2">
                <span class="truncate font-medium">{session().title}</span>
                <SessionStatusBadge status={session().status} />
              </span>
              <span class="flex items-center gap-2 text-xs text-muted">
                <span class="font-mono">{session().runner}</span>
                <span aria-hidden="true">·</span>
                <time
                  datetime={new Date(session().created_at).toISOString()}
                  title={absoluteTime(session().created_at)}
                >
                  {relativeTime(session().created_at, props.now)}
                </time>
                {/* The list knows a session's status only: `shipping` is pull requests still in play. */}
                <Show when={session().status === "shipping"}>
                  <span aria-hidden="true">·</span>
                  <span data-part="pull-requests" class="font-medium text-tone-active">
                    Pull requests open
                  </span>
                </Show>
              </span>
            </a>
          </li>
        )}
      </For>
    </ul>
  );
}
