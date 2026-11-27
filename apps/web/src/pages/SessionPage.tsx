import { useNavigate, useParams } from "@solidjs/router";
import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  Match,
  onSettled,
  Show,
  Switch,
  useContext,
} from "solid-js";
import { MessageBox } from "../components/MessageBox";
import { PanelTabs, tabId, tabPanelId, type Tab } from "../components/PanelTabs";
import { PermissionBar } from "../components/PermissionBar";
import { PlanPanel } from "../components/PlanPanel";
import { SessionHeader } from "../components/SessionHeader";
import { SidePane } from "../components/SidePane";
import { Transcript } from "../components/Transcript";
import type { SessionStatus } from "../gen/SessionStatus";
import { changedFileCount } from "../lib/changeCount";
import { createMediaQuery, WIDE_QUERY } from "../lib/media";
import { canMessage, resumesOnMessage, sessionLook } from "../lib/status";
import { PanelHost, PanelOutlet } from "../panels/PanelHost";
import {
  panelFromSegment,
  sessionPath,
  sidePanels,
  type FileRequest,
  type FileTarget,
  type PanelId,
  type SidePanelId,
} from "../panels/registry";
import { AppContext } from "../state/app";
import { createPermissionAnswers } from "../state/permissionAnswers";
import type { PendingPermission, TranscriptEntry } from "../state/sessionReducer";

const ANNOUNCED_CHARS = 300;
const NONE_PENDING: readonly PendingPermission[] = [];

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

/**
 * One session, live, as a workspace of panels. Mounted once per id: the route
 * remounts it when the id changes, and only `panel` moves with the address.
 *
 * On a phone the panels are tabs under the header and one shows at a time. In
 * the wide layout the transcript stays on the left and the others share the
 * side pane, which is collapsed while `panel` is the transcript.
 */
export function SessionScreen(props: {
  id: string;
  panel: PanelId;
  onPanel: (panel: PanelId) => void;
}) {
  const app = useContext(AppContext);
  // `id` is fixed for the life of this component (see SessionPage), so one read is enough.
  const store = app.openSession(props.id);
  onSettled(() => store.start());

  const wide = createMediaQuery(WIDE_QUERY);
  const idBase = createUniqueId();

  const loadError = () => {
    const load = store.load();
    return load.status === "error" ? load : undefined;
  };
  const sidePanel = (): SidePanelId | undefined =>
    props.panel === "transcript" ? undefined : props.panel;

  // The side panels live here, once, for as long as this screen does: the two
  // layouts below only adopt their element (see PanelHost). They wait for the
  // session's record, so no panel loads or opens a socket for an unknown session.
  const hostedPanel = createMemo(() => (store.load().status === "ready" ? sidePanel() : undefined));
  // A file asked for from a panel or from the transcript; the Files panel takes it, now or when it mounts.
  const [fileRequest, setFileRequest] = createSignal<FileRequest>();
  let fileRequests = 0;
  function openFile(target: FileTarget): void {
    fileRequests += 1;
    setFileRequest({ target, seq: fileRequests });
    props.onPanel("files");
  }
  let panels: HTMLDivElement | undefined;
  void (
    <PanelHost
      store={store}
      panel={hostedPanel()}
      fileRequest={fileRequest()}
      onOpenFile={openFile}
      ref={(element) => (panels = element)}
    />
  );
  // One set of answers for the pinned bar and the tool cards: either can answer a request.
  const answers = createPermissionAnswers((requestId, decision, message) =>
    store.resolvePermission(requestId, decision, message),
  );
  // On a phone only the selected panel shows; the transcript keeps its place in the page meanwhile.
  const transcriptHidden = (): boolean => !wide() && props.panel !== "transcript";
  // A replay passes through every request ever made; only the caught-up list is worth showing.
  const pending = (): readonly PendingPermission[] =>
    store.synced() ? store.view().pendingPermissions : NONE_PENDING;

  // The server's list, once read, sees what the events do not: a shell's edits, a revert.
  const changedCount = createMemo(() =>
    changedFileCount(store.changes.list(), store.changes.revision(), store.view().changedFiles),
  );
  // Read once the session is known, whether or not the Changes panel is ever opened;
  // the store reads it again when the status changes.
  createEffect(
    () => store.load().status === "ready",
    (ready) => {
      if (ready) store.changes.request();
    },
  );
  const sideTabs = createMemo((): Tab<SidePanelId>[] =>
    sidePanels.map(({ id, label }) =>
      id === "changes" && changedCount() > 0
        ? { id, label, badge: String(changedCount()) }
        : { id, label },
    ),
  );
  const allTabs = createMemo((): Tab<PanelId>[] => [
    { id: "transcript", label: "Transcript" },
    ...sideTabs(),
  ]);

  const announcedMessage = createMemo(() =>
    store.synced() ? lastCompletedText(store.view().transcript).slice(0, ANNOUNCED_CHARS) : "",
  );

  // `waiting` leads the tab title, so a background tab shows that the session needs its reader.
  const documentTitle = createMemo(() => {
    const title = store.session()?.title;
    if (title === undefined) return "Calin";
    return store.status() === "waiting" ? `Waiting for you · ${title} · Calin` : `${title} · Calin`;
  });
  createEffect(documentTitle, (title) => {
    document.title = title;
    return () => {
      document.title = "Calin";
    };
  });

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
        <SessionHeader store={store} />
        <Show when={store.stalled()}>
          <p role="alert" class="border-b border-line bg-attention-soft px-3 py-2 text-sm">
            Part of this transcript could not be loaded. Reload the page to try again.
          </p>
        </Show>
        {/* Shown once the record is loaded: for an unknown session the not-found screen says it better. */}
        <Show when={store.load().status === "ready" && store.subscriptionError()}>
          {(message) => (
            <p
              role="alert"
              class="flex items-center justify-between gap-2 border-b border-line bg-attention-soft px-3 py-2 text-sm"
            >
              <span class="min-w-0">
                The server refused live updates for this session: {message()}
              </span>
              <button type="button" class="btn" onClick={() => store.resubscribe()}>
                Try again
              </button>
            </p>
          )}
        </Show>
        {/* Coarse announcements only: status changes and whole agent messages, never deltas. */}
        <div class="sr-only" role="status" aria-live="polite">
          <Show when={store.synced() && store.status()}>
            {(status) => <p>Session status: {sessionLook(status()).label}</p>}
          </Show>
          <p>{announcedMessage()}</p>
        </div>
        <div class="flex min-h-0 flex-1">
          <div class="flex min-h-0 min-w-0 flex-1 flex-col">
            <Show when={!wide()}>
              <div class="flex border-b border-line bg-surface">
                <PanelTabs
                  label="Session panels"
                  idBase={idBase}
                  tabs={allTabs()}
                  active={props.panel}
                  onSelect={(panel) => props.onPanel(panel)}
                />
              </div>
            </Show>
            <div
              role={wide() ? undefined : "tabpanel"}
              id={tabPanelId(idBase, "transcript")}
              aria-labelledby={wide() ? undefined : tabId(idBase, "transcript")}
              hidden={transcriptHidden()}
              class="flex min-h-0 flex-1 flex-col"
            >
              <Transcript
                entries={store.view().transcript}
                synced={store.synced()}
                repos={store.repos()}
                answers={answers}
                onOpenFile={openFile}
              />
              <Show when={store.view().plan.length > 0}>
                <PlanPanel entries={store.view().plan} />
              </Show>
            </div>
            <Show when={!wide()}>
              {/* Kept in the page while the transcript shows, so the panels stay laid out where they were. */}
              <div
                role={sidePanel() === undefined ? undefined : "tabpanel"}
                id={tabPanelId(idBase, sidePanel() ?? "none")}
                aria-labelledby={sidePanel() === undefined ? undefined : tabId(idBase, props.panel)}
                hidden={sidePanel() === undefined}
                class="flex min-h-0 flex-1 flex-col"
              >
                <PanelOutlet panels={panels} />
              </div>
            </Show>
            {/* Under every panel: a request must not wait unseen behind a tab. */}
            <PermissionBar
              pending={pending()}
              answers={answers}
              onResolve={(requestId, decision, message) =>
                store.resolvePermission(requestId, decision, message)
              }
            />
            <div hidden={transcriptHidden()}>
              <MessageBox
                enabled={canMessage(store.status())}
                closedHint={closedHint(store.status())}
                resumes={resumesOnMessage(store.status())}
                onSend={(text) => store.send(text)}
              />
            </div>
          </div>
          <Show when={wide()}>
            <SidePane
              idBase={idBase}
              tabs={sideTabs()}
              panel={sidePanel()}
              onSelect={(panel) => props.onPanel(panel)}
              onCollapse={() => props.onPanel("transcript")}
            >
              <PanelOutlet panels={panels} />
            </SidePane>
          </Show>
        </div>
      </Match>
    </Switch>
  );
}

export default function SessionPage() {
  const params = useParams<{ id: string; panel?: string }>();
  const navigate = useNavigate();
  return (
    <Show when={params.id} keyed>
      {(id) => (
        <SessionScreen
          id={id}
          panel={panelFromSegment(params.panel)}
          onPanel={(panel) => navigate(sessionPath(id, panel))}
        />
      )}
    </Show>
  );
}
