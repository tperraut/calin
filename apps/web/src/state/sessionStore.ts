import { createSignal, type Accessor } from "solid-js";
import { HttpError, type Api } from "../api/http";
import { sessionTopic, type SocketClient, type TopicHandler } from "../api/socket";
import type {
  OpenTerminal,
  TerminalClient,
  TerminalHandlers,
  TerminalRequest,
} from "../api/terminal";
import type { FileContent } from "../gen/FileContent";
import type { FileDiff } from "../gen/FileDiff";
import type { PermissionDecision } from "../gen/PermissionDecision";
import type { RepoChanges } from "../gen/RepoChanges";
import type { ResolvePermissionRequest } from "../gen/ResolvePermissionRequest";
import type { Session } from "../gen/Session";
import type { SessionDetail } from "../gen/SessionDetail";
import type { SessionRepo } from "../gen/SessionRepo";
import type { SessionStatus } from "../gen/SessionStatus";
import type { TreeEntry } from "../gen/TreeEntry";
import type { WriteFileRequest } from "../gen/WriteFileRequest";
import { errorMessage } from "../lib/errors";
import { coalesce, type Schedule } from "../lib/schedule";
import { applyEvent, emptySessionView, mergeRepos, type SessionView } from "./sessionReducer";

export type SessionLoad =
  | { status: "loading" }
  | { status: "ready" }
  | { status: "not_found" }
  | { status: "error"; message: string; unreachable: boolean };

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
  reload(): void;
  /** Subscribes and loads; returns the function that undoes both. */
  start(): () => void;
  /** The session's repos on disk, for the workspace panels. Paths are relative to a repo's root. */
  workspace: SessionWorkspace;
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
  schedule?: Schedule;
  /** Absent where no terminal can be opened (most tests): asking for one reports an error. */
  openTerminal?: OpenTerminal;
};

const noTerminal: OpenTerminal = (_request, handlers) => {
  queueMicrotask(() =>
    handlers.onEnd({ kind: "error", message: "Terminals are not available here." }),
  );
  return { write() {}, resize() {}, close() {} };
};

/** Resubscriptions in a row that brought nothing new before giving up. */
const MAX_FRUITLESS_RESYNCS = 3;

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

  const publish = coalesce(() => {
    setView(current);
    setSynced(caughtUp);
    setStalled(fruitlessResyncs >= MAX_FRUITLESS_RESYNCS);
    setSubscriptionError(refusal);
  }, deps.schedule);

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
        current = result.view;
        fruitlessResyncs = 0;
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
        publish();
      }
    }
  };

  function fetchDetail(): void {
    api.getSession(id).then(
      (loaded) => {
        if (!active) return;
        failed = false;
        setDetail(loaded);
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

  return {
    id,
    load,
    session,
    // Mid-replay the fold passes through every past status; the record is steadier.
    status: () => (synced() ? view().status : null) ?? session()?.status,
    view,
    repos: () => mergeRepos(detail()?.repos ?? [], view().repos),
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
    reload() {
      setLoad({ status: "loading" });
      fetchDetail();
    },
    start() {
      active = true;
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
