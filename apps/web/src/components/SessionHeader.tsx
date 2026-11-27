import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  For,
  Show,
  useContext,
} from "solid-js";
import { compactCount, formatCost } from "../lib/format";
import { createRequest } from "../lib/request";
import { canAttach, canInterrupt, canStop, permissionModeLook, repoModeLabel } from "../lib/status";
import { sessionPath } from "../panels/registry";
import { AppContext } from "../state/app";
import type { SessionStore } from "../state/sessionStore";
import { AttachRepo } from "./AttachRepo";
import { SessionStatusBadge } from "./Badge";
import { PullRequestChips } from "./PullRequests";
import { currentShipPlan, ShipAction, shipOffered } from "./ShipAction";
import { ShipForm } from "./ShipForm";

/**
 * Title, status, the two ways to halt a session, its repos with the way to add
 * one, and what follows review: the way to open its pull requests, then the
 * pull requests themselves. No button changes anything by itself: the status,
 * the repo set and the pull requests follow the session's events.
 */
export function SessionHeader(props: {
  store: SessionStore;
  /** Whether the ship form is asked for; it shows only while the session can be shipped. */
  shipOpen: boolean;
  onShipOpen: (open: boolean) => void;
}) {
  const app = useContext(AppContext);
  const [attaching, setAttaching] = createSignal(false);
  const stop = createRequest(() => props.store.stop());
  const interrupt = createRequest(() => props.store.interrupt());
  // Only a phone needs the tap: from `lg` the details are always in view.
  const [detailsOpen, setDetailsOpen] = createSignal(false);
  const detailsId = createUniqueId();

  const title = (): string => props.store.session()?.title ?? "Session";
  const pendingCount = (): number =>
    props.store.synced() ? props.store.view().pendingPermissions.length : 0;
  const plan = createMemo(() => currentShipPlan(props.store));
  const shippable = createMemo(() => shipOffered(props.store, plan()));
  const hasPullRequests = (): boolean => props.store.delivery.pullRequests().length > 0;
  // A form left open must not come back by itself the next time the session is in review.
  createEffect(shippable, (offered) => {
    if (!offered) props.onShipOpen(false);
  });
  const tree = createMemo(() => props.store.tree());
  const parentId = (): string | undefined => props.store.session()?.parent_session_id ?? undefined;
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
                    "min-w-0 basis-full flex-wrap items-center gap-x-2 gap-y-1 pb-1 lg:flex lg:basis-auto lg:pb-0",
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
                    <ul
                      aria-label="Repos"
                      class="flex max-w-full min-w-0 flex-wrap items-center gap-1"
                    >
                      <For each={props.store.repos()} keyed={(repo) => repo.id}>
                        {(repo) => (
                          <li
                            class="flex max-w-full min-w-0 items-center gap-1 rounded border border-line px-1.5 py-0.5"
                            title={repo().path}
                          >
                            <span class="max-w-[60%] shrink-0 truncate font-medium text-fg">
                              {repo().name}
                            </span>
                            <Show when={repo().primary}>
                              <span class="text-tone-active">primary</span>
                            </Show>
                            <Show
                              when={repo().branch}
                              fallback={<span>{repoModeLabel(repo().mode).toLowerCase()}</span>}
                            >
                              {(branch) => (
                                // Cut on screen when long; whole in the text and the title.
                                <span class="min-w-0 truncate font-mono" title={branch()}>
                                  {branch()}
                                </span>
                              )}
                            </Show>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>
                  <button
                    type="button"
                    class="inline-flex min-h-11 items-center rounded border border-line px-2 font-medium text-fg hover:border-muted disabled:cursor-not-allowed disabled:opacity-50 lg:min-h-0 lg:py-0.5"
                    aria-expanded={attaching() ? "true" : "false"}
                    title={
                      canAttach(props.store.status())
                        ? "Add a repo to this session"
                        : "A repo can be attached while the session is running, waiting or in review"
                    }
                    disabled={!canAttach(props.store.status())}
                    onClick={() => {
                      // The list may be from another screen, or never read: ask for today's.
                      if (!attaching()) app.loadRepos();
                      setAttaching((open) => !open);
                    }}
                  >
                    Attach repo
                  </button>
                </div>
              </>
            )}
          </Show>
        </div>
        <Show when={parentId() !== undefined || tree().children.length > 0}>
          <div
            data-part="family"
            class="flex flex-wrap items-center gap-x-2 gap-y-1 pt-0.5 text-xs text-muted"
          >
            <Show when={parentId()}>
              {(id) => (
                <a
                  href={sessionPath(id())}
                  class="inline-flex min-h-11 max-w-full min-w-0 items-center gap-1 lg:min-h-0"
                >
                  <span class="shrink-0">Child of</span>
                  <span class="truncate font-medium text-accent underline underline-offset-4">
                    {tree().parent?.title ?? "its parent session"}
                  </span>
                </a>
              )}
            </Show>
            <Show when={tree().children.length > 0}>
              <ul
                aria-label="Child sessions"
                // One row on a phone, scrolled sideways: several children must not push the panels down.
                class="flex max-w-full min-w-0 items-center gap-1 [scrollbar-width:none] overflow-x-auto lg:flex-wrap lg:overflow-visible"
              >
                <For each={tree().children} keyed={(child) => child.id}>
                  {(child) => (
                    <li class="max-w-[80%] min-w-0 shrink-0 lg:max-w-full lg:shrink">
                      <a
                        href={sessionPath(child().id)}
                        class="flex min-h-11 max-w-full min-w-0 items-center gap-1.5 rounded border border-line px-1.5 hover:border-muted lg:min-h-0 lg:py-0.5"
                      >
                        <span class="truncate font-medium text-fg">{child().title}</span>
                        <SessionStatusBadge status={child().status} />
                      </a>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </div>
        </Show>
        <Show when={shippable() || hasPullRequests()}>
          <div class="flex flex-wrap items-center gap-x-2 gap-y-1 pt-1 text-xs text-muted">
            <ShipAction
              store={props.store}
              open={props.shipOpen}
              onOpen={() => props.onShipOpen(!props.shipOpen)}
            />
            <Show when={hasPullRequests()}>
              <PullRequestChips store={props.store} />
            </Show>
          </div>
        </Show>
      </header>
      <Show when={props.shipOpen && shippable()}>
        <ShipForm store={props.store} plan={plan()} onClose={() => props.onShipOpen(false)} />
      </Show>
      <Show when={attaching()}>
        <AttachRepo
          registry={app.repos()}
          attached={props.store.repos()}
          enabled={canAttach(props.store.status())}
          onAttach={(request) => props.store.attachRepo(request)}
          onRetry={() => app.loadRepos()}
          onClose={() => setAttaching(false)}
        />
      </Show>
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
