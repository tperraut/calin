import { createMemo, createSignal, For, Show } from "solid-js";
import type { Session } from "../gen/Session";
import { absoluteTime, relativeTime } from "../lib/format";
import { sessionPath } from "../panels/registry";
import { childrenShownByDefault, nestSessions, type SessionNode } from "../state/sessionsList";
import { SessionStatusBadge } from "./Badge";

function SessionLink(props: { session: Session; now: number; child: boolean }) {
  return (
    // The router marks the open session: aria-current on its own address, data-active
    // on its panels' as well. The focus ring is drawn inside the row because the
    // scrolling list clips what is outside.
    <a
      href={sessionPath(props.session.id)}
      class="flex min-h-11 min-w-0 flex-1 flex-col gap-1 px-3 py-2.5 -outline-offset-2 hover:bg-sunken data-active:bg-accent-soft"
    >
      <span class="flex items-center justify-between gap-2">
        <span class="truncate font-medium">{props.session.title}</span>
        <SessionStatusBadge status={props.session.status} />
      </span>
      <span class="flex items-center gap-2 text-xs text-muted">
        <Show when={props.child} fallback={<span class="font-mono">{props.session.runner}</span>}>
          <span data-part="child">Child session</span>
        </Show>
        <span aria-hidden="true">·</span>
        <time
          datetime={new Date(props.session.created_at).toISOString()}
          title={absoluteTime(props.session.created_at)}
        >
          {relativeTime(props.session.created_at, props.now)}
        </time>
        {/* api.rs, "The queue": a root session that is queued waits for a free slot. */}
        <Show when={props.session.status === "queued" && !props.child}>
          <span aria-hidden="true">·</span>
          <span data-part="queued" class="min-w-0 truncate font-medium">
            Queued: waiting for a free slot
          </span>
        </Show>
        {/* The list knows a session's status only: `shipping` is pull requests still in play. */}
        <Show when={props.session.status === "shipping"}>
          <span aria-hidden="true">·</span>
          <span data-part="pull-requests" class="font-medium text-tone-active">
            Pull requests open
          </span>
        </Show>
      </span>
    </a>
  );
}

function SessionRows(props: {
  nodes: readonly SessionNode[];
  now: number;
  openId: string | undefined;
  child: boolean;
  /** What the reader chose per parent; absent until they do. */
  chosen: ReadonlyMap<string, boolean>;
  onToggle: (id: string, shown: boolean) => void;
}) {
  const shown = (node: SessionNode): boolean =>
    props.chosen.get(node.session.id) ?? childrenShownByDefault(node, props.openId);
  return (
    <For each={props.nodes} keyed={(node) => node.session.id}>
      {(node) => (
        <li>
          <div class="flex items-stretch">
            <SessionLink session={node().session} now={props.now} child={props.child} />
            <Show when={node().children.length > 0}>
              <button
                type="button"
                class="flex min-h-11 min-w-11 shrink-0 items-center justify-center gap-1 border-l border-line px-2 text-xs text-muted -outline-offset-2 hover:bg-sunken"
                aria-expanded={shown(node()) ? "true" : "false"}
                aria-label={`Child sessions of ${node().session.title}`}
                title={shown(node()) ? "Hide the child sessions" : "Show the child sessions"}
                onClick={() => props.onToggle(node().session.id, !shown(node()))}
              >
                <span
                  aria-hidden="true"
                  class={["transition-transform", { "rotate-90": shown(node()) }]}
                >
                  ▸
                </span>
                {node().children.length}
              </button>
            </Show>
          </div>
          <Show when={node().children.length > 0 && shown(node())}>
            <ul
              aria-label={`Child sessions of ${node().session.title}`}
              class="ml-4 divide-y divide-line border-t border-l border-line"
            >
              <SessionRows
                nodes={node().children}
                now={props.now}
                openId={props.openId}
                child={true}
                chosen={props.chosen}
                onToggle={props.onToggle}
              />
            </ul>
          </Show>
        </li>
      )}
    </For>
  );
}

/**
 * The sessions as links, in the order given, with the sessions an agent
 * dispatched nested under the one that dispatched them. A parent's children
 * are folded away once all of them are settled; the reader can always toggle.
 * `now` drives the relative times; `openId` is the session on screen, if any.
 */
export function SessionList(props: { sessions: readonly Session[]; now: number; openId?: string }) {
  const nodes = createMemo(() => nestSessions(props.sessions));
  const [chosen, setChosen] = createSignal<ReadonlyMap<string, boolean>>(new Map());
  return (
    <ul class="divide-y divide-line">
      <SessionRows
        nodes={nodes()}
        now={props.now}
        openId={props.openId}
        child={false}
        chosen={chosen()}
        onToggle={(id, shown) => setChosen((all) => new Map(all).set(id, shown))}
      />
    </ul>
  );
}
