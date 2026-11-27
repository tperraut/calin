import { createSignal, type Accessor } from "solid-js";
import { HttpError, type Api } from "../api/http";
import { sessionTopic, type SocketClient, type TopicHandler } from "../api/socket";
import type {
  OpenTerminal,
  TerminalClient,
  TerminalHandlers,
  TerminalRequest,
} from "../api/terminal";
import type { AnswerQuestionRequest } from "../gen/AnswerQuestionRequest";
import type { AttachRepoRequest } from "../gen/AttachRepoRequest";
import type { EventBody } from "../gen/EventBody";
import type { FileContent } from "../gen/FileContent";
import type { FileDiff } from "../gen/FileDiff";
import type { Graph } from "../gen/Graph";
import type { Health } from "../gen/Health";
import type { PermissionDecision } from "../gen/PermissionDecision";
import type { PullRequest } from "../gen/PullRequest";
import type { RepoChanges } from "../gen/RepoChanges";
import type { ResolvePermissionRequest } from "../gen/ResolvePermissionRequest";
import type { Session } from "../gen/Session";
import type { SessionDetail } from "../gen/SessionDetail";
import type { SessionRepo } from "../gen/SessionRepo";
import type { SessionStatus } from "../gen/SessionStatus";
import type { ShipRequest } from "../gen/ShipRequest";
import type { Timeline } from "../gen/Timeline";
import type { TreeEntry } from "../gen/TreeEntry";
import type { WorkflowRun } from "../gen/WorkflowRun";
import type { WriteFileRequest } from "../gen/WriteFileRequest";
import { mergePullRequests, shipPlan } from "../lib/delivery";
import { errorMessage } from "../lib/errors";
import { createRefresher, type Refresher } from "../lib/refresher";
import { coalesce, type Schedule } from "../lib/schedule";
import { mergeRun } from "../lib/workflow";
import { applyEvent, emptySessionView, mergeRepos, type SessionView } from "./sessionReducer";
import { sessionTreeOf, type SessionTree } from "./sessionTree";

export type SessionLoad =
  | { status: "loading" }
  | { status: "ready" }
  | { status: "not_found" }
  | { status: "error"; message: string; unreachable: boolean };

/**
 * What the session changed, as the server lists it (`GET /changes`). Shared by
 * the Changes panel and the tab's count, so it is read here rather than in
 * either. The store reads it again when a turn ends or a repo joins, since git
 * may then report what no `file_changed` event announced; its readers ask for the rest.
 */
export type ChangesFeed = {
  /** One entry per attached repo; `undefined` until read once. */
  list: Accessor<readonly RepoChanges[] | undefined>;
  /** The `changesRevision` the list accounts for: file events after it may not be in it. */
  revision: Accessor<number>;
  /** The server's reason when the last read failed. */
  failure: Accessor<string | undefined>;
  refreshing: Accessor<boolean>;
  /** Something changed: read soon, once, however often this is called meanwhile. */
  request(): void;
  /** Read at once. */
  now(): void;
};

/**
 * What follows review: the session's pull requests and the request that opens
 * them. Nothing here is assumed done: a pull request shows when its event arrives.
 */
export type Delivery = {
  /** In repo-set order: the REST record's, kept current by `pull_request_*` events. */
  pullRequests: Accessor<readonly PullRequest[]>;
  /** Why this server cannot open pull requests (`Health::github_problem`), when it cannot. */
  blocked: Accessor<string | undefined>;
  /** Asks the server again whether it can: the reader may have fixed what was wrong. */
  recheck(): void;
  /** True when pull requests are only recorded, never opened (api.rs, "Dry run"). */
  dryRun: Accessor<boolean>;
  /**
   * Set from the request to ship until every repo it covers has reported, as
   * a pull request event or as an error: `opening` when the session had no
   * pull request yet, `updating` when it did.
   */
  shipping: Accessor<"opening" | "updating" | undefined>;
  /** Repos the last ship asked from this page reported an error for. */
  failed: Accessor<number>;
  /** Rejects with the server's reason when the request is refused. */
  ship(request: ShipRequest): Promise<void>;
  /** Has the server look at the pull requests now; what changed arrives as events. */
  refresh(): Promise<void>;
  /** A clock that ticks slowly, for "updated 5 min ago". */
  now: Accessor<number>;
};

/** Live state of one session: its record over REST, its transcript over the socket. */
export type SessionStore = {
  readonly id: string;
  load: Accessor<SessionLoad>;
  session: Accessor<Session | undefined>;
  status: Accessor<SessionStatus | undefined>;
  view: Accessor<SessionView>;
  /** The repo set, primary first: the REST record's, kept current by `repo_attached` events. */
  repos: Accessor<readonly SessionRepo[]>;
  /** True once the replay has caught up with the head of the log. */
  synced: Accessor<boolean>;
  /** True when the server keeps leaving the same gap; only a reload can help. */
  stalled: Accessor<boolean>;
  /** The server's reason when it refused the session's topic; no live updates until it is retried. */
  subscriptionError: Accessor<string | undefined>;
  /** Asks for the topic again after a refusal. */
  resubscribe(): void;
  send(text: string): Promise<void>;
  /** Ends the current turn; the session goes to `review`. */
  interrupt(): Promise<void>;
  stop(): Promise<void>;
  /** Answers a pending permission request. `message` goes to the agent with a denial. */
  resolvePermission(
    requestId: string,
    decision: PermissionDecision,
    message?: string,
  ): Promise<void>;
  /**
   * Answers a question an agent asked. Nothing changes here: the question is
   * over when its `question_answered` event arrives. Rejects with the server's reason.
   */
  answerQuestion(questionId: string, answer: string, comment?: string): Promise<void>;
  /** The session's workflow run (see `WorkflowFeed`). */
  workflow: WorkflowFeed;
  /**
   * The sessions of this session's tree (same `root_session_id`), from the
   * sessions list, so it follows every `session_upserted`.
   */
  tree: Accessor<SessionTree>;
  /** The orchestration views of the whole tree (api.rs, "Graph and timeline"). */
  orchestration: Orchestration;
  /**
   * Attaches a repo to the running session. Nothing is added here: the repo
   * shows once its `repo_attached` event arrives. Rejects with the server's reason.
   */
  attachRepo(request: AttachRepoRequest): Promise<void>;
  changes: ChangesFeed;
  delivery: Delivery;
  reload(): void;
  /** Subscribes and loads; returns the function that undoes both. */
  start(): () => void;
  /** The session's repos on disk, for the workspace panels. Paths are relative to a repo's root. */
  workspace: SessionWorkspace;
};

/**
 * The workflow run: folded from the events, so it is live, with what only
 * `GET /sessions/{id}/workflow` tells (loop limits, the tree's cost). That is
 * read once the session is known to run a workflow, and again, a moment after
 * a node moves or usage is reported, while the Workflow view is on screen.
 */
export type WorkflowFeed = {
  /** `undefined` while the session is not known to run a workflow. */
  run: Accessor<WorkflowRun | undefined>;
  /** True once the record or the events say the session runs a workflow. */
  known: Accessor<boolean>;
  /** The server's reason when the last read failed. */
  failure: Accessor<string | undefined>;
  /** The Workflow view is on screen, or no longer is. */
  setVisible(visible: boolean): void;
  /** Reads at once. */
  now(): void;
};

export type Orchestration = {
  /** The graph now, or as it was at `at` (Unix milliseconds). */
  graph(at?: number): Promise<Graph>;
  timeline(): Promise<Timeline>;
};

/** The routes under `/sessions/{id}` that read and write the repos (api.rs), and the terminal socket. */
export type SessionWorkspace = {
  changes(): Promise<RepoChanges[]>;
  diff(sessionRepoId: string, path: string): Promise<FileDiff>;
  /** One level; an empty `path` is the repo's root. */
  tree(sessionRepoId: string, path: string): Promise<TreeEntry[]>;
  readFile(sessionRepoId: string, path: string): Promise<FileContent>;
  writeFile(sessionRepoId: string, path: string, body: WriteFileRequest): Promise<FileContent>;
  /** Starts a shell on the server; closing the returned client kills it. */
  openTerminal(
    request: Omit<TerminalRequest, "sessionId">,
    handlers: TerminalHandlers,
  ): TerminalClient;
};

export type SessionStoreDeps = {
  api: Api;
  socket: SocketClient;
  /** The session as the `sessions` topic last reported it, if it did. */
  listed?: (id: string) => Session | undefined;
  /** Every session the `sessions` topic reported; the session's tree is read from it. */
  sessions?: () => readonly Session[];
  schedule?: Schedule;
  /** Absent where no terminal can be opened (most tests): asking for one reports an error. */
  openTerminal?: OpenTerminal;
  /** The server as `GET /health` last described it, if it did. */
  health?: () => Health | undefined;
  /** Reads `GET /health` again. */
  checkHealth?: () => void;
  /** The app's clock; a still one where none is given. */
  now?: Accessor<number>;
};

const noTerminal: OpenTerminal = (_request, handlers) => {
  queueMicrotask(() =>
    handlers.onEnd({ kind: "error", message: "Terminals are not available here." }),
  );
  return { write() {}, resize() {}, close() {} };
};

const NO_SESSIONS: readonly Session[] = [];

/** Resubscriptions in a row that brought nothing new before giving up. */
const MAX_FRUITLESS_RESYNCS = 3;

/**
 * How long a ship may stay silent before the page stops waiting for it. The
 * contract has no event for "the ship is over", and one that reuses a pull
 * request with nothing to push may report nothing at all.
 */
export const SHIP_QUIET_MS = 30_000;

/** A ship under way, as far as the events have told. */
type ShipWatch = {
  kind: "opening" | "updating";
  /** Repos the ship covers whose outcome has not arrived, id to name. */
  waiting: Map<string, string>;
  /** Every repo the ship covers, id to name: an `error` event names its repo in words only. */
  covered: ReadonlyMap<string, string>;
  failed: Set<string>;
  /** Errors that named none of the covered repos: each still stands for one repo. */
  unnamedFailures: number;
  timer: ReturnType<typeof setTimeout> | undefined;
};

/** True when `message` holds `name` as a whole word, as in "could not push api: …". */
function mentions(message: string, name: string): boolean {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(^|[^\\w-])${escaped}($|[^\\w-])`).test(message);
}

export function createSessionStore(deps: SessionStoreDeps, id: string): SessionStore {
  const { api, socket } = deps;
  const topic = sessionTopic(id);

  const [load, setLoad] = createSignal<SessionLoad>({ status: "loading" });
  const [detail, setDetail] = createSignal<SessionDetail | undefined>(undefined);
  const [view, setView] = createSignal<SessionView>(emptySessionView);
  const [synced, setSynced] = createSignal(false);
  const [stalled, setStalled] = createSignal(false);
  const [subscriptionError, setSubscriptionError] = createSignal<string>();

  // The fold runs on plain values as frames arrive; signals follow once per frame.
  let current = emptySessionView;
  let caughtUp = false;
  // While a replay is on its way, frames still in flight from before it are expected.
  let replaying = true;
  let fruitlessResyncs = 0;
  let refusal: string | undefined;
  let active = false;
  let failed = false;
  let stopListening = (): void => {};

  // Mirrors of two signals, for what is decided outside the screen's own updates.
  let loadedDetail: SessionDetail | undefined;
  let loadedChanges: readonly RepoChanges[] = [];

  const [changes, setChanges] = createSignal<readonly RepoChanges[]>();
  const [changesRevision, setChangesRevision] = createSignal(0);
  const [changesFailure, setChangesFailure] = createSignal<string>();
  const [changesRefreshing, setChangesRefreshing] = createSignal(false);
  // Lives from `start` to its undoing: nothing is read for a session nobody looks at.
  let changesRefresher: Refresher | undefined;

  const [servedRun, setServedRun] = createSignal<WorkflowRun>();
  const [workflowFailure, setWorkflowFailure] = createSignal<string>();
  let workflowRefresher: Refresher | undefined;
  let workflowSeeded = false;
  let workflowShown = false;

  async function readWorkflow(): Promise<void> {
    try {
      const loaded = await api.getWorkflow(id);
      if (!active) return;
      setServedRun(loaded ?? undefined);
      setWorkflowFailure(undefined);
    } catch (error) {
      if (active) setWorkflowFailure(errorMessage(error));
    }
  }

  /** The first read, once the session is known to run a workflow; later ones follow the view. */
  function seedWorkflow(): void {
    if (workflowSeeded || !active) return;
    workflowSeeded = true;
    void readWorkflow();
  }

  async function readChanges(): Promise<void> {
    // What was folded before the question is in the answer; later events may not be.
    const revision = current.changesRevision;
    setChangesRefreshing(true);
    try {
      const loaded = await api.getChanges(id);
      loadedChanges = loaded;
      setChanges(loaded);
      setChangesRevision(revision);
      setChangesFailure(undefined);
    } catch (error) {
      setChangesFailure(errorMessage(error));
    } finally {
      setChangesRefreshing(false);
    }
  }

  const publish = coalesce(() => {
    setView(current);
    setSynced(caughtUp);
    setStalled(fruitlessResyncs >= MAX_FRUITLESS_RESYNCS);
    setSubscriptionError(refusal);
  }, deps.schedule);

  const [shipping, setShipping] = createSignal<"opening" | "updating">();
  const [shipFailures, setShipFailures] = createSignal(0);
  let shipWatch: ShipWatch | undefined;

  function endShip(): void {
    if (!shipWatch) return;
    clearTimeout(shipWatch.timer);
    shipWatch = undefined;
    setShipping(undefined);
  }

  /** Ends the watch once every covered repo has reported; keeps waiting, for a while, otherwise. */
  function settleShip(watch: ShipWatch): void {
    clearTimeout(watch.timer);
    if (watch.waiting.size <= watch.unnamedFailures) {
      endShip();
    } else {
      watch.timer = setTimeout(endShip, SHIP_QUIET_MS);
    }
  }

  /** api.rs, "Shipping": each covered repo answers with a pull request event or an `error` naming it. */
  function watchShip(watch: ShipWatch, body: EventBody): void {
    if (body.type === "pull_request_opened" || body.type === "pull_request_updated") {
      watch.waiting.delete(body.pull_request.session_repo_id);
    } else if (body.type === "error") {
      // The longest name first, so `web-admin` is not taken for `web`.
      const named = [...watch.covered]
        .toSorted(([, a], [, b]) => b.length - a.length)
        .find(([, name]) => mentions(body.message, name));
      if (named) {
        watch.failed.add(named[0]);
        watch.waiting.delete(named[0]);
      } else {
        watch.unnamedFailures += 1;
      }
      setShipFailures(Math.min(watch.failed.size + watch.unnamedFailures, watch.covered.size));
    } else if (body.type === "session_status") {
      // The session went elsewhere (a turn began, it was stopped): the ship is no longer the news.
      if (body.status !== "review" && body.status !== "shipping") {
        endShip();
        return;
      }
    } else {
      return;
    }
    settleShip(watch);
  }

  const sessionRepos = (): readonly SessionRepo[] =>
    mergeRepos(loadedDetail?.repos ?? [], current.repos);

  async function ship(request: ShipRequest): Promise<void> {
    if (shipWatch) return;
    const known = mergePullRequests(
      loadedDetail?.pull_requests ?? [],
      current.pullRequests,
      sessionRepos(),
      caughtUp,
    );
    const covered = new Map(
      shipPlan(sessionRepos(), loadedChanges, known).steps.map((step) => [
        step.repo.id,
        step.repo.name,
      ]),
    );
    // Watching starts before the request: the first events may beat its answer here.
    const watch: ShipWatch = {
      kind: known.length > 0 ? "updating" : "opening",
      waiting: new Map(covered),
      covered,
      failed: new Set(),
      unnamedFailures: 0,
      timer: undefined,
    };
    shipWatch = watch;
    setShipping(watch.kind);
    setShipFailures(0);
    try {
      await api.shipSession(id, request);
    } catch (error) {
      if (shipWatch === watch) endShip();
      throw error;
    }
    if (shipWatch === watch) settleShip(watch);
  }

  function resync(): void {
    if (fruitlessResyncs >= MAX_FRUITLESS_RESYNCS) {
      // Show what arrived, flagged as incomplete, instead of asking forever.
      replaying = false;
      caughtUp = true;
      publish();
      return;
    }
    fruitlessResyncs += 1;
    replaying = true;
    socket.resync(topic, current.seq);
  }

  const onFrame: TopicHandler = (msg) => {
    if (msg.op === "event") {
      const result = applyEvent(current, msg.event);
      if (result.outcome === "applied") {
        const before = current;
        current = result.view;
        fruitlessResyncs = 0;
        // Live only: a replay passes through every past status, and the list is read after it anyway.
        if (caughtUp && (current.status !== before.status || current.repos !== before.repos)) {
          changesRefresher?.request();
        }
        // Live only as well: a replayed pull request is not this ship's.
        if (caughtUp && shipWatch) watchShip(shipWatch, msg.event.body);
        if (current.workflow && caughtUp) {
          seedWorkflow();
          const type = msg.event.body.type;
          if (type === "workflow_node" || type === "workflow_finished" || type === "usage") {
            workflowRefresher?.request();
          }
        }
        publish();
      } else if (result.outcome === "gap" && !replaying) {
        resync();
      }
    } else if (msg.op === "error") {
      // No replay is coming, so nothing in flight explains a later gap.
      refusal = msg.message;
      replaying = false;
      publish();
    } else if (msg.op === "subscribed") {
      if (refusal !== undefined) {
        refusal = undefined;
        publish();
      }
      if (msg.head_seq > current.seq) {
        // The replay ended short of the head it announced.
        resync();
      } else {
        replaying = false;
        caughtUp = true;
        if (current.workflow) seedWorkflow();
        publish();
      }
    }
  };

  function fetchDetail(): void {
    api.getSession(id).then(
      (loaded) => {
        if (!active) return;
        failed = false;
        loadedDetail = loaded;
        setDetail(loaded);
        if (loaded.session.workflow !== null && loaded.session.workflow !== undefined) {
          seedWorkflow();
        }
        setLoad({ status: "ready" });
      },
      (error: unknown) => {
        if (!active) return;
        if (error instanceof HttpError && error.status === 404) {
          stopListening();
          setLoad({ status: "not_found" });
          return;
        }
        failed = true;
        setLoad({
          status: "error",
          message: errorMessage(error),
          unreachable: error instanceof HttpError && error.unreachable,
        });
      },
    );
  }

  const session = (): Session | undefined => deps.listed?.(id) ?? detail()?.session;
  const repos = (): readonly SessionRepo[] => mergeRepos(detail()?.repos ?? [], view().repos);
  const stillClock = Date.now();

  return {
    id,
    load,
    session,
    // Mid-replay the fold passes through every past status; the record is steadier.
    status: () => (synced() ? view().status : null) ?? session()?.status,
    view,
    repos,
    synced,
    stalled,
    subscriptionError,
    resubscribe() {
      replaying = true;
      socket.resync(topic, current.seq);
    },
    send: (text) => api.sendMessage(id, { text }),
    interrupt: () => api.interruptSession(id),
    stop: () => api.stopSession(id),
    resolvePermission(requestId, decision, message) {
      const body: ResolvePermissionRequest = { decision };
      if (message !== undefined && message.trim() !== "") body.message = message.trim();
      return api.resolvePermission(id, requestId, body);
    },
    answerQuestion(questionId, answer, comment) {
      const body: AnswerQuestionRequest = { answer: answer.trim() };
      if (comment !== undefined && comment.trim() !== "") body.comment = comment.trim();
      return api.answerQuestion(id, questionId, body);
    },
    workflow: {
      run: () => mergeRun(view().workflow, servedRun()),
      known: () =>
        view().workflow !== null ||
        servedRun() !== undefined ||
        (session()?.workflow ?? null) !== null,
      failure: workflowFailure,
      setVisible(visible) {
        workflowShown = visible;
        // Asked while still hidden, so becoming visible reads once rather than twice.
        if (visible) workflowRefresher?.now();
        workflowRefresher?.setVisible(visible && document.visibilityState === "visible");
      },
      now: () => workflowRefresher?.now(),
    },
    tree: () => sessionTreeOf(deps.sessions?.() ?? NO_SESSIONS, session()),
    orchestration: {
      graph: (at) => api.getGraph(id, at),
      timeline: () => api.getTimeline(id),
    },
    attachRepo: async (request) => {
      await api.attachRepo(id, request);
    },
    changes: {
      list: changes,
      revision: changesRevision,
      failure: changesFailure,
      refreshing: changesRefreshing,
      request: () => changesRefresher?.request(),
      now: () => changesRefresher?.now(),
    },
    delivery: {
      pullRequests: () =>
        mergePullRequests(detail()?.pull_requests ?? [], view().pullRequests, repos(), synced()),
      blocked() {
        const health = deps.health?.();
        // api.rs, "Dry run": nothing reaches GitHub then, so its being unusable is no obstacle.
        if (!health || health.dry_run) return undefined;
        return health.github_problem ?? undefined;
      },
      recheck: () => deps.checkHealth?.(),
      dryRun: () => deps.health?.()?.dry_run === true,
      shipping,
      failed: shipFailures,
      ship,
      refresh: () => api.refreshPullRequests(id),
      now: deps.now ?? (() => stillClock),
    },
    reload() {
      setLoad({ status: "loading" });
      fetchDetail();
    },
    start() {
      active = true;
      const refresher = createRefresher(readChanges);
      changesRefresher = refresher;
      const workflowReads = createRefresher(readWorkflow);
      workflowRefresher = workflowReads;
      // Nothing is read for a page in a background tab; what was missed is read on the way back.
      const onVisibility = (): void => {
        const visible = document.visibilityState === "visible";
        refresher.setVisible(visible);
        workflowReads.setVisible(visible && workflowShown);
      };
      document.addEventListener("visibilitychange", onVisibility);
      onVisibility();
      const unsubscribe = socket.subscribe(topic, onFrame, () => current.seq);
      // A failed load is retried as soon as the server answers the socket again.
      const offState = socket.onState((state) => {
        if (state === "open" && failed) fetchDetail();
      });
      stopListening = () => {
        unsubscribe();
        offState();
      };
      fetchDetail();
      return () => {
        active = false;
        stopListening();
        document.removeEventListener("visibilitychange", onVisibility);
        refresher.dispose();
        changesRefresher = undefined;
        workflowReads.dispose();
        workflowRefresher = undefined;
        endShip();
      };
    },
    workspace: {
      changes: () => api.getChanges(id),
      diff: (repo, path) => api.getDiff(id, repo, path),
      tree: (repo, path) => api.listTree(id, repo, path),
      readFile: (repo, path) => api.readFile(id, repo, path),
      writeFile: (repo, path, body) => api.writeFile(id, repo, path, body),
      openTerminal: (request, handlers) =>
        (deps.openTerminal ?? noTerminal)({ ...request, sessionId: id }, handlers),
    },
  };
}
