import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  For,
  Show,
  useContext,
} from "solid-js";
import type { Session } from "../gen/Session";
import type { SessionRepo } from "../gen/SessionRepo";
import { compactCount, formatCost } from "../lib/format";
import { createRequest, type Request } from "../lib/request";
import { canAttach, canInterrupt, canStop, permissionModeLook, repoModeLabel } from "../lib/status";
import { sessionPath } from "../panels/registry";
import { AppContext } from "../state/app";
import type { SessionStore } from "../state/sessionStore";
import type { SessionTree } from "../state/sessionTree";
import { AttachRepo } from "./AttachRepo";
import { SessionStatusBadge } from "./Badge";
import { SessionOrigin } from "./SessionChips";
import { PullRequestChips } from "./PullRequests";
import { currentShipPlan, ShipAction, shipOffered } from "./ShipAction";
import { ShipForm } from "./ShipForm";

const repoCountLabel = (count: number): string =>
  count === 0 ? "no repo" : count === 1 ? "1 repo" : `${count} repos`;

const pendingLabel = (count: number): string =>
  count === 1 ? "1 request needs you" : `${count} requests need you`;

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

  function toggleAttach(): void {
    // The list may be from another screen, or never read: ask for today's.
    if (!attaching()) app.loadRepos();
    setAttaching((open) => !open);
  }

  return (
    <>
      <header class="border-b border-line bg-surface px-2 py-1.5 lg:px-4">
        <div class="flex items-center gap-2">
          <a href="/" aria-label="Back to sessions" class="btn btn-ghost px-0 lg:hidden">
            <span aria-hidden="true">←</span>
          </a>
          <h1 class="min-w-0 flex-1 truncate font-semibold">{title()}</h1>
          {/* Shown only while they can act: a dead button costs a phone's title its room. */}
          <HaltButton
            request={interrupt}
            offered={canInterrupt(props.store.status())}
            label="Interrupt"
            busyLabel="Interrupting…"
            title="End the current turn; the session stays open for review"
            class="btn px-2.5"
          />
          <HaltButton
            request={stop}
            offered={canStop(props.store.status())}
            label="Stop"
            busyLabel="Stopping…"
            title="End the agent and cancel the session"
            class="btn px-2.5 text-tone-danger hover:not-disabled:border-tone-danger"
          />
        </div>
        <div class="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-muted">
          <Show when={props.store.status()}>
            {(status) => <SessionStatusBadge status={status()} />}
          </Show>
          <Show when={props.store.session()}>
            {(session) => <SessionOrigin session={session()} class="-my-2.5" />}
          </Show>
          <Show when={pendingCount() > 0}>
            <span class="font-medium text-tone-attention">{pendingLabel(pendingCount())}</span>
          </Show>
          <Show when={props.store.session()}>
            {(session) => (
              <SessionDetails
                store={props.store}
                session={session()}
                attaching={attaching()}
                onAttach={toggleAttach}
              />
            )}
          </Show>
        </div>
        <SessionFamily store={props.store} />
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
      <HaltFailure verb="interrupt" error={interrupt.error()} />
      <HaltFailure verb="stop" error={stop.error()} />
    </>
  );
}

/** Interrupt or Stop: shown while the session's status allows it, or while its request is on its way. */
function HaltButton(props: {
  request: Request;
  offered: boolean;
  label: string;
  busyLabel: string;
  title: string;
  class: string;
}) {
  return (
    <Show when={props.offered || props.request.busy()}>
      <button
        type="button"
        class={props.class}
        title={props.title}
        disabled={props.request.busy()}
        onClick={() => void props.request.run()}
      >
        {props.request.busy() ? props.busyLabel : props.label}
      </button>
    </Show>
  );
}

function HaltFailure(props: { verb: string; error: string | undefined }) {
  return (
    <Show when={props.error}>
      {(message) => (
        <p role="alert" class="border-b border-line bg-danger-soft px-3 py-2 text-sm">
          Could not {props.verb} the session: {message()}
        </p>
      )}
    </Show>
  );
}

/**
 * The runner, the cost, the permission mode and the repos. On a phone they
 * fold behind one line naming the mode and counting the repos; from `lg`
 * they are always in view.
 */
function SessionDetails(props: {
  store: SessionStore;
  session: Session;
  attaching: boolean;
  onAttach: () => void;
}) {
  const [open, setOpen] = createSignal(false);
  const detailsId = createUniqueId();
  const mode = () => permissionModeLook(props.session.permission_mode);
  const attachable = (): boolean => canAttach(props.store.status());
  return (
    <>
      <button
        type="button"
        class="-mx-1 inline-flex min-h-11 items-center gap-1 px-1 lg:hidden"
        aria-expanded={open() ? "true" : "false"}
        aria-controls={detailsId}
        onClick={() => setOpen((was) => !was)}
      >
        {mode().label} · {repoCountLabel(props.store.repos().length)}
        <span aria-hidden="true" class={["transition-transform", { "rotate-90": open() }]}>
          ▸
        </span>
      </button>
      <div
        id={detailsId}
        class={[
          "min-w-0 basis-full flex-wrap items-center gap-x-2 gap-y-1 pb-1 lg:flex lg:basis-auto lg:pb-0",
          open() ? "flex" : "hidden",
        ]}
      >
        <span class="font-mono">{props.session.runner}</span>
        <Show when={props.store.view().usage}>
          {(usage) => (
            <span class="font-mono" title="Cost and tokens used by this session's agents">
              {formatCost(usage().cost_usd)} · {compactCount(usage().tokens_in)} in ·{" "}
              {compactCount(usage().tokens_out)} out
            </span>
          )}
        </Show>
        <span title={mode().hint}>
          Permissions: <span class="font-medium text-fg">{mode().label}</span>
        </span>
        <Show when={props.store.repos().length > 0} fallback={<span>No repo attached</span>}>
          <RepoChips repos={props.store.repos()} />
        </Show>
        <button
          type="button"
          class="inline-flex min-h-11 items-center rounded border border-line px-2 font-medium text-fg hover:border-muted disabled:cursor-not-allowed disabled:opacity-50 lg:min-h-0 lg:py-0.5"
          aria-expanded={props.attaching ? "true" : "false"}
          title={
            attachable()
              ? "Add a repo to this session"
              : "A repo can be attached while the session is running, waiting or in review"
          }
          disabled={!attachable()}
          onClick={() => props.onAttach()}
        >
          Attach repo
        </button>
      </div>
    </>
  );
}

/** Each repo of the session: its name, whether it is the primary one, and its branch or mode. */
function RepoChips(props: { repos: readonly SessionRepo[] }) {
  return (
    <ul aria-label="Repos" class="flex max-w-full min-w-0 flex-wrap items-center gap-1">
      <For each={props.repos} keyed={(repo) => repo.id}>
        {(repo) => (
          <li
            class="flex max-w-full min-w-0 items-center gap-1 rounded border border-line px-1.5 py-0.5"
            title={repo().path}
          >
            <span class="max-w-[60%] shrink-0 truncate font-medium text-fg">{repo().name}</span>
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
  );
}

/** The session's parent and children, each leading to its own session; nothing for a lone session. */
function SessionFamily(props: { store: SessionStore }) {
  const tree = createMemo(() => props.store.tree());
  const parentId = (): string | undefined => props.store.session()?.parent_session_id ?? undefined;
  return (
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
          <ChildChips tree={tree()} />
        </Show>
      </div>
    </Show>
  );
}

function ChildChips(props: { tree: SessionTree }) {
  return (
    <ul
      aria-label="Child sessions"
      // One row on a phone, scrolled sideways: several children must not push the panels down.
      class="flex max-w-full min-w-0 items-center gap-1 [scrollbar-width:none] overflow-x-auto lg:flex-wrap lg:overflow-visible"
    >
      <For each={props.tree.children} keyed={(child) => child.id}>
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
  );
}
