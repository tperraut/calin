import { useNavigate, useParams } from "@solidjs/router";
import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  Match,
  onSettled,
  type ParentProps,
  Show,
  Switch,
  useContext,
} from "solid-js";
import { MessageBox } from "../components/MessageBox";
import { PanelTabs, tabId, tabPanelId, type Tab } from "../components/PanelTabs";
import { PermissionBar } from "../components/PermissionBar";
import { PlanPanel } from "../components/PlanPanel";
import { QuestionBar } from "../components/QuestionBar";
import { SessionHeader } from "../components/SessionHeader";
import { SidePane } from "../components/SidePane";
import { Transcript, type RevealRequest } from "../components/Transcript";
import type { PullRequest } from "../gen/PullRequest";
import type { SessionStatus } from "../gen/SessionStatus";
import type { WorkflowRun } from "../gen/WorkflowRun";
import { changedFileCount } from "../lib/changeCount";
import { needsAttention } from "../lib/delivery";
import { createMediaQuery, WIDE_QUERY } from "../lib/media";
import { speakerFor } from "../lib/messages";
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
import type {
  OpenQuestion,
  PendingPermission,
  SessionView,
  TranscriptEntry,
} from "../state/sessionReducer";
import type { SessionLoad, SessionStore } from "../state/sessionStore";

const ANNOUNCED_CHARS = 300;
const NONE_PENDING: readonly PendingPermission[] = [];
const NO_QUESTIONS: readonly OpenQuestion[] = [];

function closedHint(status: SessionStatus | undefined): string {
  if (status === undefined) return "Loading…";
  if (status === "queued" || status === "preparing") {
    return "Messages open once the session is running";
  }
  if (status === "done") return "This session is done: it can be read, not continued";
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

/** What the live region reads out of the transcript: nothing while it is still replaying. */
const announcedText = (synced: boolean, transcript: readonly TranscriptEntry[]): string =>
  synced ? lastCompletedText(transcript).slice(0, ANNOUNCED_CHARS) : "";

// A replay passes through every request ever made; only the caught-up lists are worth showing.
const pendingOf = (synced: boolean, view: SessionView): readonly PendingPermission[] =>
  synced ? view.pendingPermissions : NONE_PENDING;

// One thing at a time above the message box: a permission holds a tool call, so it goes first.
const questionsOf = (synced: boolean, view: SessionView): readonly OpenQuestion[] =>
  synced && view.pendingPermissions.length === 0 ? view.openQuestions : NO_QUESTIONS;

type LoadFailure = Extract<SessionLoad, { status: "error" }>;

const loadFailureOf = (load: SessionLoad): LoadFailure | undefined =>
  load.status === "error" ? load : undefined;

/** What a session that could not load says: the server out of reach, or its refusal. */
function loadFailureLook(failure: LoadFailure): { title: string; detail: string } {
  return failure.unreachable
    ? {
        title: "Cannot reach the server",
        detail: "The session will load as soon as the server answers again.",
      }
    : { title: "Could not load the session", detail: failure.message };
}

/** api.rs, "The queue": a session past the limit waits for a slot; the server says so in words. */
function queuedReasonOf(
  status: SessionStatus | undefined,
  reason: string | null,
): string | undefined {
  if (status !== "queued" || reason === null || reason.trim() === "") return undefined;
  return reason.trim();
}

/**
 * The tab title. What needs the reader leads it, so a background tab shows it:
 * a request to answer, or a pull request whose checks or review went wrong.
 */
function documentTitleOf(
  title: string | undefined,
  status: SessionStatus | undefined,
  pullRequests: readonly PullRequest[],
): string {
  if (title === undefined) return "Calin";
  if (status === "waiting") return `Waiting for you · ${title} · Calin`;
  if (status === "shipping" && pullRequests.some(needsAttention)) {
    return `Pull request needs you · ${title} · Calin`;
  }
  return `${title} · Calin`;
}

const doneNoteOf = (hasPullRequests: boolean): string =>
  hasPullRequests
    ? "Done: every pull request is merged or closed. This session is now read-only."
    : "Done: this session is now read-only.";

const shipFailureNote = (failed: number): string =>
  failed === 1 ? "1 repo could not be shipped." : `${failed} repos could not be shipped.`;

/** The side panels' tabs; the Changes tab counts the changed files once there are any. */
const sideTabsOf = (changed: number): Tab<SidePanelId>[] =>
  sidePanels.map(({ id, label }) =>
    id === "changes" && changed > 0 ? { id, label, badge: String(changed) } : { id, label },
  );

/** A workflow loop's limit, when the run is known and has that node. */
const loopLimitIn = (run: WorkflowRun | undefined, nodeId: string): number | null =>
  run?.nodes.find((node) => node.id === nodeId)?.max ?? null;

function Notice(props: { title: string; detail: string; onRetry?: () => void }) {
  return (
    <div class="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
      <h1 class="text-xl font-semibold">{props.title}</h1>
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

  const loadError = (): LoadFailure | undefined => loadFailureOf(store.load());
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
  // The ship form lives under the header; the header and the Changes panel both open it.
  const [shipOpen, setShipOpen] = createSignal(false);
  // On a phone the transcript and its box are behind the panel that asked; in
  // the wide layout they are already beside it.
  function transcriptOnPhone(): void {
    if (!wide()) props.onPanel("transcript");
  }
  // A message written for the reader by another part of the screen; the message box takes it.
  const [prefill, setPrefill] = createSignal<{ text: string; seq: number }>();
  let prefills = 0;
  function draftMessage(text: string): void {
    prefills += 1;
    setPrefill({ text, seq: prefills });
    transcriptOnPhone();
  }
  // The one agent whose rows the transcript shows; the graph can set it, the transcript's own control too.
  const [agentFilter, setAgentFilter] = createSignal<string | null>(null);
  const [reveal, setReveal] = createSignal<RevealRequest>();
  let reveals = 0;
  function showAgent(agentId: string): void {
    setAgentFilter(agentId);
    transcriptOnPhone();
  }
  function showItem(itemId: string): void {
    reveals += 1;
    setAgentFilter(null);
    setReveal({ itemId, seq: reveals });
    transcriptOnPhone();
  }
  let panels: HTMLDivElement | undefined;
  void (
    <PanelHost
      store={store}
      panel={hostedPanel()}
      fileRequest={fileRequest()}
      onOpenFile={openFile}
      onOpenShipForm={() => setShipOpen(true)}
      onDraftMessage={draftMessage}
      onOpenPanel={(panel) => props.onPanel(panel)}
      onShowAgent={showAgent}
      onShowItem={showItem}
      onUnsavedWork={(description) => app.setUnsavedWork(description)}
      ref={(element) => (panels = element)}
    />
  );
  // One set of answers for the pinned bar and the tool cards: either can answer a request.
  const answers = createPermissionAnswers((requestId, decision, message) =>
    store.resolvePermission(requestId, decision, message),
  );
  // On a phone only the selected panel shows; the transcript keeps its place in the page meanwhile.
  const transcriptHidden = (): boolean => !wide() && props.panel !== "transcript";
  const pending = (): readonly PendingPermission[] => pendingOf(store.synced(), store.view());
  const questions = (): readonly OpenQuestion[] => questionsOf(store.synced(), store.view());
  const speaker = createMemo(() =>
    speakerFor(store.session(), store.view().agents, store.tree().sessions),
  );

  const loopLimit = (nodeId: string): number | null => loopLimitIn(store.workflow.run(), nodeId);

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
  const sideTabs = createMemo((): Tab<SidePanelId>[] => sideTabsOf(changedCount()));
  const allTabs = createMemo((): Tab<PanelId>[] => [
    { id: "transcript", label: "Transcript" },
    ...sideTabs(),
  ]);

  const announcedMessage = createMemo(() => announcedText(store.synced(), store.view().transcript));

  const documentTitle = createMemo(() =>
    documentTitleOf(store.session()?.title, store.status(), store.delivery.pullRequests()),
  );
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
            title={loadFailureLook(failure()).title}
            detail={loadFailureLook(failure()).detail}
            onRetry={() => store.reload()}
          />
        )}
      </Match>
      <Match when={true}>
        <SessionHeader store={store} shipOpen={shipOpen()} onShipOpen={setShipOpen} />
        <SessionNotices store={store} />
        <LiveAnnouncements store={store} message={announcedMessage()} />
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
            <TranscriptSlot idBase={idBase} tab={!wide()} hidden={transcriptHidden()}>
              <Transcript
                entries={store.view().transcript}
                synced={store.synced()}
                repos={store.repos()}
                answers={answers}
                onOpenFile={openFile}
                agents={store.view().agents}
                subagentCalls={store.view().subagentCalls}
                speaker={speaker()}
                filter={agentFilter()}
                onFilter={setAgentFilter}
                reveal={reveal()}
                loopLimit={loopLimit}
              />
              <Show when={store.view().plan.length > 0}>
                <PlanPanel entries={store.view().plan} />
              </Show>
            </TranscriptSlot>
            <Show when={!wide()}>
              <PhonePanelSlot idBase={idBase} panel={sidePanel()} panels={panels} />
            </Show>
            {/* Under every panel: a request must not wait unseen behind a tab. */}
            <PermissionBar
              pending={pending()}
              answers={answers}
              onResolve={(requestId, decision, message) =>
                store.resolvePermission(requestId, decision, message)
              }
            />
            <QuestionBar
              questions={questions()}
              onAnswer={(questionId, answer, comment) =>
                store.answerQuestion(questionId, answer, comment)
              }
              compact={transcriptHidden()}
            />
            <MessageArea store={store} prefill={prefill()} hidden={transcriptHidden()} />
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

/** What holds the session back, above everything else: a queue, a stalled replay, refused live updates. */
function SessionNotices(props: { store: SessionStore }) {
  return (
    <>
      <Show when={queuedReasonOf(props.store.status(), props.store.view().statusReason)}>
        {(reason) => (
          <p
            data-part="queued"
            class="border-b border-line bg-sunken px-3 py-2 text-sm wrap-break-word"
          >
            <span class="font-medium">Queued:</span> {reason()}
          </p>
        )}
      </Show>
      <Show when={props.store.stalled()}>
        <p role="alert" class="border-b border-line bg-attention-soft px-3 py-2 text-sm">
          Part of this transcript could not be loaded. Reload the page to try again.
        </p>
      </Show>
      {/* Shown once the record is loaded: for an unknown session the not-found screen says it better. */}
      <Show when={props.store.load().status === "ready" && props.store.subscriptionError()}>
        {(message) => (
          <p
            role="alert"
            class="flex items-center justify-between gap-2 border-b border-line bg-attention-soft px-3 py-2 text-sm"
          >
            <span class="min-w-0">
              The server refused live updates for this session: {message()}
            </span>
            <button type="button" class="btn" onClick={() => props.store.resubscribe()}>
              Try again
            </button>
          </p>
        )}
      </Show>
    </>
  );
}

/** Coarse announcements only: status changes and whole agent messages, never deltas. */
function LiveAnnouncements(props: { store: SessionStore; message: string }) {
  return (
    <div class="sr-only" role="status" aria-live="polite">
      <Show when={props.store.synced() && props.store.status()}>
        {(status) => <p>Session status: {sessionLook(status()).label}</p>}
      </Show>
      <p>{props.message}</p>
      <Show when={props.store.delivery.failed() > 0}>
        <p>{shipFailureNote(props.store.delivery.failed())} The transcript says why.</p>
      </Show>
    </div>
  );
}

/** The transcript's place: on a phone one of the session's tabs, in the wide layout always in view. */
function TranscriptSlot(props: ParentProps<{ idBase: string; tab: boolean; hidden: boolean }>) {
  return (
    <div
      role={props.tab ? "tabpanel" : undefined}
      id={tabPanelId(props.idBase, "transcript")}
      aria-labelledby={props.tab ? tabId(props.idBase, "transcript") : undefined}
      hidden={props.hidden}
      class="flex min-h-0 flex-1 flex-col"
    >
      <h2 class="sr-only">Transcript</h2>
      {props.children}
    </div>
  );
}

/** On a phone, the side panel the address names, as the tabpanel of its tab. */
function PhonePanelSlot(props: {
  idBase: string;
  panel: SidePanelId | undefined;
  panels: HTMLElement | undefined;
}) {
  return (
    // Kept in the page while the transcript shows, so the panels stay laid out where they were.
    <div
      role={props.panel === undefined ? undefined : "tabpanel"}
      id={tabPanelId(props.idBase, props.panel ?? "none")}
      aria-labelledby={props.panel === undefined ? undefined : tabId(props.idBase, props.panel)}
      hidden={props.panel === undefined}
      class="flex min-h-0 flex-1 flex-col"
    >
      <PanelOutlet panels={props.panels} />
    </div>
  );
}

/** The message box, under a note once the session is done. */
function MessageArea(props: {
  store: SessionStore;
  prefill: { text: string; seq: number } | undefined;
  hidden: boolean;
}) {
  return (
    <div hidden={props.hidden}>
      <Show when={props.store.status() === "done"}>
        <p data-part="done-note" class="border-t border-line bg-sunken px-3 py-2 text-sm">
          {doneNoteOf(props.store.delivery.pullRequests().length > 0)}
        </p>
      </Show>
      <MessageBox
        enabled={canMessage(props.store.status())}
        closedHint={closedHint(props.store.status())}
        resumes={resumesOnMessage(props.store.status())}
        prefill={props.prefill}
        shown={!props.hidden}
        onSend={(text) => props.store.send(text)}
      />
    </div>
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
