import { createSignal, type Accessor, type Setter } from "solid-js";
import { HttpError, type Api } from "../api/http";
import { sessionTopic, type SocketClient, type TopicMsg } from "../api/socket";
import type {
  OpenTerminal,
  TerminalClient,
  TerminalHandlers,
  TerminalRequest,
} from "../api/terminal";
import type { AnswerQuestionRequest } from "../gen/AnswerQuestionRequest";
import type { AttachRepoRequest } from "../gen/AttachRepoRequest";
import type { Event } from "../gen/Event";
import type { EventBody } from "../gen/EventBody";
import type { FileContent } from "../gen/FileContent";
import type { FileDiff } from "../gen/FileDiff";
import type { Graph } from "../gen/Graph";
import type { Health } from "../gen/Health";
import type { LimitWindow } from "../gen/LimitWindow";
import type { PermissionDecision } from "../gen/PermissionDecision";
import type { PullRequest } from "../gen/PullRequest";
import type { RepoChanges } from "../gen/RepoChanges";
import type { ResolvePermissionRequest } from "../gen/ResolvePermissionRequest";
import type { RunnerLimits } from "../gen/RunnerLimits";
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
  /** A `limits` event arrived: the runner of the agent that reported it, as of the event. */
  onLimits?: (report: RunnerLimits) => void;
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

/** What a ship covers, as the page knows it when the ship is asked. */
type ShipScope = { known: readonly PullRequest[]; covered: ReadonlyMap<string, string> };

/** True when `message` holds `name` as a whole word, as in "could not push api: …". */
function mentions(message: string, name: string): boolean {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(^|[^\\w-])${escaped}($|[^\\w-])`).test(message);
}

/** The id of the covered repo that `message` names, if it names one. */
export function namedRepo(
  covered: ReadonlyMap<string, string>,
  message: string,
): string | undefined {
  // The longest name first, so `web-admin` is not taken for `web`.
  const named = [...covered]
    .toSorted(([, a], [, b]) => b.length - a.length)
    .find(([, name]) => mentions(message, name));
  return named?.[0];
}

/** Trimmed, or `undefined` when there is nothing but blanks. */
function filled(text: string | undefined): string | undefined {
  const trimmed = text?.trim();
  return trimmed === "" ? undefined : trimmed;
}

function resolution(
  decision: PermissionDecision,
  message: string | undefined,
): ResolvePermissionRequest {
  const note = filled(message);
  return note === undefined ? { decision } : { decision, message: note };
}

function answerOf(answer: string, comment: string | undefined): AnswerQuestionRequest {
  const note = filled(comment);
  return note === undefined ? { answer: answer.trim() } : { answer: answer.trim(), comment: note };
}

function blockedBy(health: Health | undefined): string | undefined {
  // api.rs, "Dry run": nothing reaches GitHub then, so its being unusable is no obstacle.
  if (!health || health.dry_run) return undefined;
  return health.github_problem ?? undefined;
}

/** Follows the ship asked from this page until every repo it covers has reported. */
class ShipTracker {
  readonly shipping: Accessor<"opening" | "updating" | undefined>;
  readonly failed: Accessor<number>;
  private readonly setShipping: Setter<"opening" | "updating" | undefined>;
  private readonly setFailed: Setter<number>;
  private watch: ShipWatch | undefined;

  constructor() {
    [this.shipping, this.setShipping] = createSignal<"opening" | "updating">();
    [this.failed, this.setFailed] = createSignal(0);
  }

  /** One at a time; rejects with the server's reason when `send` does. */
  async ship(scope: () => ShipScope, send: () => Promise<void>): Promise<void> {
    if (this.watch) return;
    const { known, covered } = scope();
    // Watching starts before the request: the first events may beat its answer here.
    const watch: ShipWatch = {
      kind: known.length > 0 ? "updating" : "opening",
      waiting: new Map(covered),
      covered,
      failed: new Set(),
      unnamedFailures: 0,
      timer: undefined,
    };
    this.watch = watch;
    this.setShipping(watch.kind);
    this.setFailed(0);
    try {
      await send();
    } catch (error) {
      if (this.watch === watch) this.end();
      throw error;
    }
    if (this.watch === watch) this.settle(watch);
  }

  /** api.rs, "Shipping": each covered repo answers with a pull request event or an `error` naming it. */
  observe(body: EventBody): void {
    const watch = this.watch;
    if (!watch) return;
    if (body.type === "pull_request_opened" || body.type === "pull_request_updated") {
      watch.waiting.delete(body.pull_request.session_repo_id);
    } else if (body.type === "error") {
      this.fail(watch, body.message);
    } else if (body.type === "session_status") {
      // The session went elsewhere (a turn began, it was stopped): the ship is no longer the news.
      if (body.status !== "review" && body.status !== "shipping") {
        this.end();
        return;
      }
    } else {
      return;
    }
    this.settle(watch);
  }

  end(): void {
    if (!this.watch) return;
    clearTimeout(this.watch.timer);
    this.watch = undefined;
    this.setShipping(undefined);
  }

  private fail(watch: ShipWatch, message: string): void {
    const named = namedRepo(watch.covered, message);
    if (named === undefined) {
      watch.unnamedFailures += 1;
    } else {
      watch.failed.add(named);
      watch.waiting.delete(named);
    }
    this.setFailed(Math.min(watch.failed.size + watch.unnamedFailures, watch.covered.size));
  }

  /** Ends the watch once every covered repo has reported; keeps waiting, for a while, otherwise. */
  private settle(watch: ShipWatch): void {
    clearTimeout(watch.timer);
    if (watch.waiting.size <= watch.unnamedFailures) {
      this.end();
    } else {
      watch.timer = setTimeout(() => this.end(), SHIP_QUIET_MS);
    }
  }
}

/** The changes list (see `ChangesFeed`). Read between `start` and `stop` only: nothing is read for a session nobody looks at. */
class ChangesReads {
  readonly list: Accessor<readonly RepoChanges[] | undefined>;
  readonly revision: Accessor<number>;
  readonly failure: Accessor<string | undefined>;
  readonly refreshing: Accessor<boolean>;
  /** Mirrors `list`, for what is decided outside the screen's own updates. */
  latest: readonly RepoChanges[] = [];
  private readonly setList: Setter<readonly RepoChanges[] | undefined>;
  private readonly setRevision: Setter<number>;
  private readonly setFailure: Setter<string | undefined>;
  private readonly setRefreshing: Setter<boolean>;
  private readonly read: () => Promise<RepoChanges[]>;
  private readonly revisionNow: () => number;
  private refresher: Refresher | undefined;

  /** `revisionNow`: the `changesRevision` folded so far. */
  constructor(read: () => Promise<RepoChanges[]>, revisionNow: () => number) {
    this.read = read;
    this.revisionNow = revisionNow;
    [this.list, this.setList] = createSignal<readonly RepoChanges[]>();
    [this.revision, this.setRevision] = createSignal(0);
    [this.failure, this.setFailure] = createSignal<string>();
    [this.refreshing, this.setRefreshing] = createSignal(false);
  }

  request(): void {
    this.refresher?.request();
  }

  now(): void {
    this.refresher?.now();
  }

  setVisible(visible: boolean): void {
    this.refresher?.setVisible(visible);
  }

  start(): void {
    this.refresher = createRefresher(() => this.readOnce());
  }

  stop(): void {
    this.refresher?.dispose();
    this.refresher = undefined;
  }

  private async readOnce(): Promise<void> {
    // What was folded before the question is in the answer; later events may not be.
    const revision = this.revisionNow();
    this.setRefreshing(true);
    try {
      const loaded = await this.read();
      this.latest = loaded;
      this.setList(loaded);
      this.setRevision(revision);
      this.setFailure(undefined);
    } catch (error) {
      this.setFailure(errorMessage(error));
    } finally {
      this.setRefreshing(false);
    }
  }
}

/** The run as `GET /sessions/{id}/workflow` tells it (see `WorkflowFeed`). Read between `start` and `stop` only. */
class WorkflowReads {
  readonly served: Accessor<WorkflowRun | undefined>;
  readonly failure: Accessor<string | undefined>;
  private readonly setServed: Setter<WorkflowRun | undefined>;
  private readonly setFailure: Setter<string | undefined>;
  private readonly read: () => Promise<WorkflowRun | null>;
  private refresher: Refresher | undefined;
  private active = false;
  private seeded = false;
  private shown = false;

  constructor(read: () => Promise<WorkflowRun | null>) {
    this.read = read;
    [this.served, this.setServed] = createSignal<WorkflowRun>();
    [this.failure, this.setFailure] = createSignal<string>();
  }

  /** The first read, once the session is known to run a workflow; later ones follow the view. */
  seed(): void {
    if (this.seeded || !this.active) return;
    this.seeded = true;
    void this.readOnce();
  }

  /** A live event of a session that runs a workflow: a node that moves, or usage, is the run's news. */
  follow(type: EventBody["type"]): void {
    this.seed();
    if (type === "workflow_node" || type === "workflow_finished" || type === "usage") {
      this.refresher?.request();
    }
  }

  now(): void {
    this.refresher?.now();
  }

  /** The Workflow view is on screen, or no longer is. */
  setShown(shown: boolean): void {
    this.shown = shown;
    // Asked while still hidden, so becoming visible reads once rather than twice.
    if (shown) this.refresher?.now();
    this.refresher?.setVisible(shown && document.visibilityState === "visible");
  }

  /** The page's tab was shown or hidden. */
  setPageVisible(visible: boolean): void {
    this.refresher?.setVisible(visible && this.shown);
  }

  start(): void {
    this.active = true;
    this.refresher = createRefresher(() => this.readOnce());
  }

  stop(): void {
    this.active = false;
    this.refresher?.dispose();
    this.refresher = undefined;
  }

  private async readOnce(): Promise<void> {
    try {
      const loaded = await this.read();
      if (!this.active) return;
      this.setServed(loaded ?? undefined);
      this.setFailure(undefined);
    } catch (error) {
      if (this.active) this.setFailure(errorMessage(error));
    }
  }
}

/** One session's state and what decides it; `createSessionStore` is its public face. */
class LiveSession {
  readonly load: Accessor<SessionLoad>;
  readonly detail: Accessor<SessionDetail | undefined>;
  readonly view: Accessor<SessionView>;
  readonly synced: Accessor<boolean>;
  readonly stalled: Accessor<boolean>;
  readonly subscriptionError: Accessor<string | undefined>;
  readonly changes: ChangesReads;
  readonly workflow: WorkflowReads;
  readonly ship = new ShipTracker();
  private readonly setLoad: Setter<SessionLoad>;
  private readonly setDetail: Setter<SessionDetail | undefined>;
  private readonly deps: SessionStoreDeps;
  private readonly id: string;
  private readonly topic: string;
  private readonly publish: () => void;
  // The fold runs on plain values as frames arrive; signals follow once per frame.
  private current = emptySessionView;
  private caughtUp = false;
  // While a replay is on its way, frames still in flight from before it are expected.
  private replaying = true;
  private fruitlessResyncs = 0;
  private refusal: string | undefined;
  private active = false;
  private failed = false;
  private stopListening = (): void => {};
  /** Mirrors `detail`, for what is decided outside the screen's own updates. */
  private loadedDetail: SessionDetail | undefined;

  constructor(deps: SessionStoreDeps, id: string) {
    this.deps = deps;
    this.id = id;
    this.topic = sessionTopic(id);
    [this.load, this.setLoad] = createSignal<SessionLoad>({ status: "loading" });
    [this.detail, this.setDetail] = createSignal<SessionDetail>();
    const [view, setView] = createSignal<SessionView>(emptySessionView);
    const [synced, setSynced] = createSignal(false);
    const [stalled, setStalled] = createSignal(false);
    const [subscriptionError, setSubscriptionError] = createSignal<string>();
    this.view = view;
    this.synced = synced;
    this.stalled = stalled;
    this.subscriptionError = subscriptionError;
    this.publish = coalesce(() => {
      setView(this.current);
      setSynced(this.caughtUp);
      setStalled(this.fruitlessResyncs >= MAX_FRUITLESS_RESYNCS);
      setSubscriptionError(this.refusal);
    }, deps.schedule);
    this.changes = new ChangesReads(
      () => deps.api.getChanges(id),
      () => this.current.changesRevision,
    );
    this.workflow = new WorkflowReads(() => deps.api.getWorkflow(id));
  }

  session(): Session | undefined {
    return this.deps.listed?.(this.id) ?? this.detail()?.session;
  }

  status(): SessionStatus | undefined {
    // Mid-replay the fold passes through every past status; the record is steadier.
    return (this.synced() ? this.view().status : null) ?? this.session()?.status;
  }

  repos(): readonly SessionRepo[] {
    return mergeRepos(this.detail()?.repos ?? [], this.view().repos);
  }

  pullRequests(): readonly PullRequest[] {
    const seed = this.detail()?.pull_requests ?? [];
    return mergePullRequests(seed, this.view().pullRequests, this.repos(), this.synced());
  }

  tree(): SessionTree {
    return sessionTreeOf(this.deps.sessions?.() ?? NO_SESSIONS, this.session());
  }

  workflowRun(): WorkflowRun | undefined {
    return mergeRun(this.view().workflow, this.workflow.served());
  }

  workflowKnown(): boolean {
    return (
      this.view().workflow !== null ||
      this.workflow.served() !== undefined ||
      (this.session()?.workflow ?? null) !== null
    );
  }

  shipNow(request: ShipRequest): Promise<void> {
    return this.ship.ship(
      () => this.shipScope(),
      () => this.deps.api.shipSession(this.id, request),
    );
  }

  resubscribe(): void {
    this.replaying = true;
    this.deps.socket.resync(this.topic, this.current.seq);
  }

  reload(): void {
    this.setLoad({ status: "loading" });
    this.fetchDetail();
  }

  /** Subscribes and loads; returns the function that undoes both. */
  start(): () => void {
    const { socket } = this.deps;
    this.active = true;
    this.changes.start();
    this.workflow.start();
    // Nothing is read for a page in a background tab; what was missed is read on the way back.
    const onVisibility = (): void => this.setPageVisible(document.visibilityState === "visible");
    document.addEventListener("visibilitychange", onVisibility);
    onVisibility();
    const unsubscribe = socket.subscribe(
      this.topic,
      (msg) => this.onFrame(msg),
      () => this.current.seq,
    );
    // A failed load is retried as soon as the server answers the socket again.
    const offState = socket.onState((state) => {
      if (state === "open" && this.failed) this.fetchDetail();
    });
    this.stopListening = () => {
      unsubscribe();
      offState();
    };
    this.fetchDetail();
    return () => {
      this.active = false;
      this.stopListening();
      document.removeEventListener("visibilitychange", onVisibility);
      this.changes.stop();
      this.workflow.stop();
      this.ship.end();
    };
  }

  private setPageVisible(visible: boolean): void {
    this.changes.setVisible(visible);
    this.workflow.setPageVisible(visible);
  }

  private shipScope(): ShipScope {
    const repos = mergeRepos(this.loadedDetail?.repos ?? [], this.current.repos);
    const seed = this.loadedDetail?.pull_requests ?? [];
    const known = mergePullRequests(seed, this.current.pullRequests, repos, this.caughtUp);
    const steps = shipPlan(repos, this.changes.latest, known).steps;
    return { known, covered: new Map(steps.map((step) => [step.repo.id, step.repo.name])) };
  }

  private onFrame(msg: TopicMsg): void {
    if (msg.op === "event") {
      this.onEvent(msg.event);
    } else if (msg.op === "error") {
      this.onRefused(msg.message);
    } else if (msg.op === "subscribed") {
      this.onSubscribed(msg.head_seq);
    }
  }

  private onEvent(event: Event): void {
    const result = applyEvent(this.current, event);
    if (result.outcome === "applied") {
      this.onApplied(result.view, event);
    } else if (result.outcome === "gap" && !this.replaying) {
      this.resync();
    }
  }

  private onApplied(view: SessionView, event: Event): void {
    const before = this.current;
    this.current = view;
    this.fruitlessResyncs = 0;
    // Live only: a replay passes through every past status and pull request; it is read after anyway.
    if (this.caughtUp) this.liveNews(before, event.body);
    // Replayed ones too: the app keeps whichever report is newest.
    if (event.body.type === "limits") this.reportLimits(event, event.body.windows);
    if (this.current.workflow && this.caughtUp) this.workflow.follow(event.body.type);
    this.publish();
  }

  private liveNews(before: SessionView, body: EventBody): void {
    if (this.current.status !== before.status || this.current.repos !== before.repos) {
      this.changes.request();
    }
    this.ship.observe(body);
  }

  private onRefused(message: string): void {
    // No replay is coming, so nothing in flight explains a later gap.
    this.refusal = message;
    this.replaying = false;
    this.publish();
  }

  private onSubscribed(headSeq: number): void {
    if (this.refusal !== undefined) {
      this.refusal = undefined;
      this.publish();
    }
    if (headSeq > this.current.seq) {
      // The replay ended short of the head it announced.
      this.resync();
      return;
    }
    this.replaying = false;
    this.caughtUp = true;
    if (this.current.workflow) this.workflow.seed();
    this.publish();
  }

  private resync(): void {
    if (this.fruitlessResyncs >= MAX_FRUITLESS_RESYNCS) {
      // Show what arrived, flagged as incomplete, instead of asking forever.
      this.replaying = false;
      this.caughtUp = true;
      this.publish();
      return;
    }
    this.fruitlessResyncs += 1;
    this.replaying = true;
    this.deps.socket.resync(this.topic, this.current.seq);
  }

  private reportLimits(event: Event, windows: LimitWindow[]): void {
    const agent = this.current.agents.find((one) => one.id === event.agent_id);
    const runner = agent?.runner ?? this.session()?.runner;
    if (runner === undefined) return;
    this.deps.onLimits?.({ runner, windows, reported_at: event.ts });
  }

  private fetchDetail(): void {
    this.deps.api.getSession(this.id).then(
      (loaded) => this.loaded(loaded),
      (error: unknown) => this.loadFailed(error),
    );
  }

  private loaded(loaded: SessionDetail): void {
    if (!this.active) return;
    this.failed = false;
    this.loadedDetail = loaded;
    this.setDetail(loaded);
    if (loaded.session.workflow !== null && loaded.session.workflow !== undefined) {
      this.workflow.seed();
    }
    this.setLoad({ status: "ready" });
  }

  private loadFailed(error: unknown): void {
    if (!this.active) return;
    if (error instanceof HttpError && error.status === 404) {
      this.stopListening();
      this.setLoad({ status: "not_found" });
      return;
    }
    this.failed = true;
    this.setLoad({
      status: "error",
      message: errorMessage(error),
      unreachable: error instanceof HttpError && error.unreachable,
    });
  }
}

export function createSessionStore(deps: SessionStoreDeps, id: string): SessionStore {
  const { api } = deps;
  const live = new LiveSession(deps, id);
  const stillClock = Date.now();

  return {
    id,
    load: live.load,
    session: () => live.session(),
    status: () => live.status(),
    view: live.view,
    repos: () => live.repos(),
    synced: live.synced,
    stalled: live.stalled,
    subscriptionError: live.subscriptionError,
    resubscribe: () => live.resubscribe(),
    send: (text) => api.sendMessage(id, { text }),
    interrupt: () => api.interruptSession(id),
    stop: () => api.stopSession(id),
    resolvePermission: (requestId, decision, message) =>
      api.resolvePermission(id, requestId, resolution(decision, message)),
    answerQuestion: (questionId, answer, comment) =>
      api.answerQuestion(id, questionId, answerOf(answer, comment)),
    workflow: {
      run: () => live.workflowRun(),
      known: () => live.workflowKnown(),
      failure: live.workflow.failure,
      setVisible: (visible) => live.workflow.setShown(visible),
      now: () => live.workflow.now(),
    },
    tree: () => live.tree(),
    orchestration: {
      graph: (at) => api.getGraph(id, at),
      timeline: () => api.getTimeline(id),
    },
    attachRepo: async (request) => {
      await api.attachRepo(id, request);
    },
    changes: {
      list: live.changes.list,
      revision: live.changes.revision,
      failure: live.changes.failure,
      refreshing: live.changes.refreshing,
      request: () => live.changes.request(),
      now: () => live.changes.now(),
    },
    delivery: {
      pullRequests: () => live.pullRequests(),
      blocked: () => blockedBy(deps.health?.()),
      recheck: () => deps.checkHealth?.(),
      dryRun: () => deps.health?.()?.dry_run === true,
      shipping: live.ship.shipping,
      failed: live.ship.failed,
      ship: (request) => live.shipNow(request),
      refresh: () => api.refreshPullRequests(id),
      now: deps.now ?? (() => stillClock),
    },
    reload: () => live.reload(),
    start: () => live.start(),
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
