import { createSignal, createUniqueId, For, Show } from "solid-js";
import { compactCount, formatCost } from "../lib/format";
import { createRequest } from "../lib/request";
import { canInterrupt, canStop, permissionModeLook, repoModeLabel } from "../lib/status";
import type { SessionStore } from "../state/sessionStore";
import { SessionStatusBadge } from "./Badge";

/**
 * Title, status and the two ways to halt a session. Neither button changes
 * anything by itself: the status follows the session's events.
 */
export function SessionHeader(props: { store: SessionStore }) {
  const stop = createRequest(() => props.store.stop());
  const interrupt = createRequest(() => props.store.interrupt());
  // Only a phone needs the tap: from `lg` the details are always in view.
  const [detailsOpen, setDetailsOpen] = createSignal(false);
  const detailsId = createUniqueId();

  const title = (): string => props.store.session()?.title ?? "Session";
  const pendingCount = (): number =>
    props.store.synced() ? props.store.view().pendingPermissions.length : 0;
  const repoCount = (): string => {
    const count = props.store.repos().length;
    return count === 0 ? "no repo" : count === 1 ? "1 repo" : `${count} repos`;
  };

  return (
    <>
      <header class="border-b border-line bg-surface px-2 py-1.5 lg:px-4">
        <div class="flex items-center gap-2">
          <a href="/" aria-label="Back to sessions" class="btn border-transparent px-0 lg:hidden">
            <span aria-hidden="true">←</span>
          </a>
          <h1 class="min-w-0 flex-1 truncate font-semibold">{title()}</h1>
          <button
            type="button"
            class="btn px-2.5"
            title="End the current turn; the session stays open for review"
            disabled={!canInterrupt(props.store.status()) || interrupt.busy()}
            onClick={() => void interrupt.run()}
          >
            {interrupt.busy() ? "Interrupting…" : "Interrupt"}
          </button>
          <button
            type="button"
            class="btn px-2.5"
            title="End the agent and cancel the session"
            disabled={!canStop(props.store.status()) || stop.busy()}
            onClick={() => void stop.run()}
          >
            {stop.busy() ? "Stopping…" : "Stop"}
          </button>
        </div>
        <div class="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-muted">
          <Show when={props.store.status()}>
            {(status) => <SessionStatusBadge status={status()} />}
          </Show>
          <Show when={pendingCount() > 0}>
            <span class="font-medium text-tone-attention">
              {pendingCount() === 1 ? "1 request needs you" : `${pendingCount()} requests need you`}
            </span>
          </Show>
          <Show when={props.store.session()}>
            {(session) => (
              <>
                <button
                  type="button"
                  class="-mx-1 inline-flex min-h-11 items-center gap-1 px-1 lg:hidden"
                  aria-expanded={detailsOpen() ? "true" : "false"}
                  aria-controls={detailsId}
                  onClick={() => setDetailsOpen((open) => !open)}
                >
                  {permissionModeLook(session().permission_mode).label} · {repoCount()}
                  <span
                    aria-hidden="true"
                    class={["transition-transform", { "rotate-90": detailsOpen() }]}
                  >
                    ▸
                  </span>
                </button>
                <div
                  id={detailsId}
                  class={[
                    "basis-full flex-wrap items-center gap-x-2 gap-y-1 pb-1 lg:flex lg:basis-auto lg:pb-0",
                    detailsOpen() ? "flex" : "hidden",
                  ]}
                >
                  <span class="font-mono">{session().runner}</span>
                  <Show when={props.store.view().usage}>
                    {(usage) => (
                      <span class="font-mono" title="Cost and tokens used by this session's agents">
                        {formatCost(usage().cost_usd)} · {compactCount(usage().tokens_in)} in ·{" "}
                        {compactCount(usage().tokens_out)} out
                      </span>
                    )}
                  </Show>
                  <span title={permissionModeLook(session().permission_mode).hint}>
                    Permissions:{" "}
                    <span class="font-medium text-fg">
                      {permissionModeLook(session().permission_mode).label}
                    </span>
                  </span>
                  <Show
                    when={props.store.repos().length > 0}
                    fallback={<span>No repo attached</span>}
                  >
                    <ul aria-label="Repos" class="flex min-w-0 flex-wrap items-center gap-1">
                      <For each={props.store.repos()} keyed={(repo) => repo.id}>
                        {(repo) => (
                          <li
                            class="flex min-w-0 items-center gap-1 rounded border border-line px-1.5 py-0.5"
                            title={repo().path}
                          >
                            <span class="truncate font-medium text-fg">{repo().name}</span>
                            <Show when={repo().primary}>
                              <span class="text-tone-active">primary</span>
                            </Show>
                            <Show
                              when={repo().branch}
                              fallback={<span>{repoModeLabel(repo().mode).toLowerCase()}</span>}
                            >
                              {(branch) => <span class="truncate font-mono">{branch()}</span>}
                            </Show>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>
                </div>
              </>
            )}
          </Show>
        </div>
      </header>
      <Show when={interrupt.error()}>
        {(message) => (
          <p role="alert" class="border-b border-line bg-danger-soft px-3 py-2 text-sm">
            Could not interrupt the session: {message()}
          </p>
        )}
      </Show>
      <Show when={stop.error()}>
        {(message) => (
          <p role="alert" class="border-b border-line bg-danger-soft px-3 py-2 text-sm">
            Could not stop the session: {message()}
          </p>
        )}
      </Show>
    </>
  );
}
