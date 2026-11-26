import { useParams } from "@solidjs/router";
import {
  createEffect,
  createMemo,
  createSignal,
  Match,
  onSettled,
  Show,
  Switch,
  useContext,
} from "solid-js";
import { MessageBox } from "../components/MessageBox";
import { PlanPanel } from "../components/PlanPanel";
import { SessionStatusBadge } from "../components/Badge";
import { Transcript } from "../components/Transcript";
import type { SessionStatus } from "../gen/SessionStatus";
import { errorMessage } from "../lib/errors";
import { compactCount, formatCost } from "../lib/format";
import { canMessage, canStop, sessionLook } from "../lib/status";
import { AppContext } from "../state/app";
import type { TranscriptEntry } from "../state/sessionReducer";

const ANNOUNCED_CHARS = 300;

function closedHint(status: SessionStatus | undefined): string {
  if (status === undefined) return "Loading…";
  if (status === "queued" || status === "preparing") {
    return "Messages open once the session is running";
  }
  if (status === "shipping") return "Messages are closed while the session ships";
  return "This session has ended";
}

/** The last agent message that is complete, for the screen-reader announcement. */
function lastCompletedText(transcript: readonly TranscriptEntry[]): string {
  for (let index = transcript.length - 1; index >= 0; index -= 1) {
    const entry = transcript[index];
    if (entry.kind !== "item" || entry.item.status !== "completed") continue;
    if (entry.item.content.kind === "text") return entry.item.content.text;
  }
  return "";
}

function Notice(props: { title: string; detail: string; onRetry?: () => void }) {
  return (
    <div class="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
      <h1 class="text-lg font-semibold">{props.title}</h1>
      <p class="max-w-sm text-sm text-muted">{props.detail}</p>
      <div class="flex gap-2">
        <Show when={props.onRetry}>
          <button type="button" class="btn" onClick={() => props.onRetry?.()}>
            Try again
          </button>
        </Show>
        <a href="/" class="btn">
          Back to sessions
        </a>
      </div>
    </div>
  );
}

/** One session, live. Mounted once per id: the route remounts it when the id changes. */
export function SessionScreen(props: { id: string }) {
  const app = useContext(AppContext);
  // `id` is fixed for the life of this component (see SessionPage), so one read is enough.
  const store = app.openSession(props.id);
  onSettled(() => store.start());

  const [stopping, setStopping] = createSignal(false);
  const [stopError, setStopError] = createSignal<string>();

  async function stop(): Promise<void> {
    setStopping(true);
    setStopError(undefined);
    try {
      await store.stop();
    } catch (reason) {
      setStopError(errorMessage(reason));
    } finally {
      setStopping(false);
    }
  }

  const loadError = () => {
    const load = store.load();
    return load.status === "error" ? load : undefined;
  };
  const title = (): string => store.session()?.title ?? "Session";
  const announcedMessage = createMemo(() =>
    store.synced() ? lastCompletedText(store.view().transcript).slice(0, ANNOUNCED_CHARS) : "",
  );

  createEffect(
    () => store.session()?.title,
    (sessionTitle) => {
      document.title = sessionTitle === undefined ? "Calin" : `${sessionTitle} · Calin`;
      return () => {
        document.title = "Calin";
      };
    },
  );

  return (
    <Switch>
      <Match when={store.load().status === "not_found"}>
        <Notice
          title="Session not found"
          detail="No session has this address. It may have been removed, or the link is incomplete."
        />
      </Match>
      <Match when={loadError()}>
        {(failure) => (
          <Notice
            title={failure().unreachable ? "Cannot reach the server" : "Could not load the session"}
            detail={
              failure().unreachable
                ? "The session will load as soon as the server answers again."
                : failure().message
            }
            onRetry={() => store.reload()}
          />
        )}
      </Match>
      <Match when={true}>
        <header class="flex items-center gap-2 border-b border-line bg-surface px-2 py-1.5 lg:px-4">
          <a href="/" aria-label="Back to sessions" class="btn border-transparent px-0 lg:hidden">
            <span aria-hidden="true">←</span>
          </a>
          <div class="min-w-0 flex-1">
            <h1 class="truncate font-semibold">{title()}</h1>
            <div class="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-muted">
              <Show when={store.status()}>
                {(status) => <SessionStatusBadge status={status()} />}
              </Show>
              <Show when={store.session()}>
                {(session) => <span class="font-mono">{session().runner}</span>}
              </Show>
              <Show when={store.view().usage}>
                {(usage) => (
                  <span class="font-mono" title="Cost and tokens used by this session's agents">
                    {formatCost(usage().cost_usd)} · {compactCount(usage().tokens_in)} in ·{" "}
                    {compactCount(usage().tokens_out)} out
                  </span>
                )}
              </Show>
            </div>
          </div>
          <button
            type="button"
            class="btn"
            disabled={!canStop(store.status()) || stopping()}
            onClick={() => void stop()}
          >
            {stopping() ? "Stopping…" : "Stop"}
          </button>
        </header>
        <Show when={stopError()}>
          {(message) => (
            <p role="alert" class="border-b border-line bg-danger-soft px-3 py-2 text-sm">
              Could not stop the session: {message()}
            </p>
          )}
        </Show>
        <Show when={store.stalled()}>
          <p role="alert" class="border-b border-line bg-attention-soft px-3 py-2 text-sm">
            Part of this transcript could not be loaded. Reload the page to try again.
          </p>
        </Show>
        <Transcript entries={store.view().transcript} synced={store.synced()} />
        <Show when={store.view().plan.length > 0}>
          <PlanPanel entries={store.view().plan} />
        </Show>
        <MessageBox
          enabled={canMessage(store.status())}
          closedHint={closedHint(store.status())}
          onSend={(text) => store.send(text)}
        />
        {/* Coarse announcements only: status changes and whole agent messages, never deltas. */}
        <div class="sr-only" role="status" aria-live="polite">
          <Show when={store.synced() && store.status()}>
            {(status) => <p>Session status: {sessionLook(status()).label}</p>}
          </Show>
          <p>{announcedMessage()}</p>
        </div>
      </Match>
    </Switch>
  );
}

export default function SessionPage() {
  const params = useParams<{ id: string }>();
  return (
    <Show when={params.id} keyed>
      {(id) => <SessionScreen id={id} />}
    </Show>
  );
}
