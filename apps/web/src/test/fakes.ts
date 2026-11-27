// Test doubles and builders shared by the test files. Never imported by the app.
import { HttpError, type Api } from "../api/http";
import type { ConnectionState, Cursor, SocketClient, TopicHandler, TopicMsg } from "../api/socket";
import type { Event } from "../gen/Event";
import type { EventBody } from "../gen/EventBody";
import type { PullRequest } from "../gen/PullRequest";
import type { Repo } from "../gen/Repo";
import type { Session } from "../gen/Session";
import type { SessionDetail } from "../gen/SessionDetail";
import type { SessionRepo } from "../gen/SessionRepo";
import type { WorkflowInfo } from "../gen/WorkflowInfo";
import type { Schedule } from "../lib/schedule";

export function makeSession(id: string, createdAt = 1000, patch: Partial<Session> = {}): Session {
  return {
    id,
    title: `Session ${id}`,
    prompt: "do the thing",
    status: "queued",
    runner: "mock",
    permission_mode: "ask",
    workflow: null,
    branch: null,
    parent_session_id: null,
    root_session_id: id,
    head_seq: 0,
    created_at: createdAt,
    updated_at: createdAt,
    ...patch,
  };
}

export const makeDetail = (
  session: Session,
  repos: SessionRepo[] = [],
  pullRequests: PullRequest[] = [],
): SessionDetail => ({
  session,
  agents: [],
  repos,
  pull_requests: pullRequests,
});

/** The pull request of the session repo `sessionRepoId`: an open one with nothing reported yet. */
export function makePullRequest(
  sessionRepoId: string,
  number: number,
  patch: Partial<PullRequest> = {},
): PullRequest {
  return {
    session_repo_id: sessionRepoId,
    number,
    url: `https://github.com/acme/${sessionRepoId}/pull/${number}`,
    title: "Add a health check",
    state: "open",
    checks: "none",
    review: "none",
    updated_at: 1000,
    ...patch,
  };
}

export function makeRepo(id: string, patch: Partial<Repo> = {}): Repo {
  return {
    id,
    name: id,
    path: `/code/${id}`,
    default_branch: "main",
    remote_url: null,
    available: true,
    declared: false,
    home: false,
    url: null,
    setup: null,
    checks: [],
    cloning: false,
    problem: null,
    ...patch,
  };
}

export function makeSessionRepo(id: string, patch: Partial<SessionRepo> = {}): SessionRepo {
  return {
    id,
    session_id: "s1",
    repo_id: `repo-${id}`,
    name: id,
    mode: "write",
    path: `/sessions/s1/${id}`,
    branch: null,
    base_ref: null,
    primary: false,
    ...patch,
  };
}

export function makeEvent(seq: number, body: EventBody, agentId: string | null = null): Event {
  return {
    id: `e${seq}`,
    session_id: "s1",
    seq,
    ts: 1000 + seq,
    agent_id: agentId,
    parent_id: null,
    body,
  };
}

/** Runs scheduled work at once, so a test reads state right after `flush()`. */
export const immediately: Schedule = (run) => run();

/** A socket client the test drives: it records what was asked and delivers what it is told. */
export class FakeSocket implements SocketClient {
  current: ConnectionState = "connecting";
  readonly resyncs: Array<{ topic: string; afterSeq: number }> = [];
  reconnects = 0;
  private readonly topics = new Map<string, Set<{ handler: TopicHandler; cursor?: Cursor }>>();
  private readonly listeners = new Set<(state: ConnectionState) => void>();

  subscribe(topic: string, handler: TopicHandler, cursor?: Cursor): () => void {
    const subscriber = { handler, cursor };
    const subscribers = this.topics.get(topic) ?? new Set();
    subscribers.add(subscriber);
    this.topics.set(topic, subscribers);
    return () => {
      subscribers.delete(subscriber);
      if (subscribers.size === 0) this.topics.delete(topic);
    };
  }

  resync(topic: string, afterSeq: number): void {
    this.resyncs.push({ topic, afterSeq });
  }

  state(): ConnectionState {
    return this.current;
  }

  onState(listener: (state: ConnectionState) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  reconnectNow(): void {
    this.reconnects += 1;
  }

  close(): void {}

  setState(state: ConnectionState): void {
    this.current = state;
    for (const listener of this.listeners) listener(state);
  }

  emit(topic: string, msg: TopicMsg): void {
    for (const { handler } of this.topics.get(topic) ?? []) handler(msg);
  }

  subscriberCount(topic: string): number {
    return this.topics.get(topic)?.size ?? 0;
  }

  /** The position the first subscriber of `topic` would resume from. */
  cursor(topic: string): number | undefined {
    for (const { cursor } of this.topics.get(topic) ?? []) return cursor?.();
    return undefined;
  }
}

/** The built-in workflow, as `GET /workflows` lists it. */
export const quickWorkflow: WorkflowInfo = {
  id: "quick",
  name: "Quick",
  description: "One agent turn on the prompt, then review.",
  builtin: true,
  budget_usd: null,
  graph: {
    nodes: [
      { id: "start", kind: "start", label: "", ports: [{ name: "next", failure: false }] },
      { id: "agent", kind: "agent", label: "{{task}}", ports: [{ name: "done", failure: false }] },
      { id: "end", kind: "end", label: "", ports: [] },
    ],
    edges: [
      { from: "start", port: "next", to: "agent" },
      { from: "agent", port: "done", to: "end" },
    ],
  },
  problem: null,
};

/** An `Api` whose routes all fail unless the test supplies them. */
export function fakeApi(routes: Partial<Api> = {}): Api {
  const missing = (name: string) => () =>
    Promise.reject(new Error(`fakeApi: ${name} was called but not stubbed`));
  return {
    health: missing("health"),
    getSettings: missing("getSettings"),
    putSettings: missing("putSettings"),
    listRepos: missing("listRepos"),
    registerRepo: missing("registerRepo"),
    refreshRepos: missing("refreshRepos"),
    cloneRepo: missing("cloneRepo"),
    removeRepo: missing("removeRepo"),
    // Sets are a convenience on top of the repo list: none unless a test says so.
    listRepoSets: () => Promise.resolve([]),
    // Only the built-in workflow unless a test says otherwise.
    listWorkflows: () => Promise.resolve([quickWorkflow]),
    listSessions: missing("listSessions"),
    createSession: missing("createSession"),
    getSession: missing("getSession"),
    sendMessage: missing("sendMessage"),
    interruptSession: missing("interruptSession"),
    stopSession: missing("stopSession"),
    resolvePermission: missing("resolvePermission"),
    answerQuestion: missing("answerQuestion"),
    getGraph: missing("getGraph"),
    getTimeline: missing("getTimeline"),
    // No workflow run unless a test says otherwise.
    getWorkflow: () => Promise.resolve(null),
    attachRepo: missing("attachRepo"),
    shipSession: missing("shipSession"),
    refreshPullRequests: missing("refreshPullRequests"),
    getChanges: missing("getChanges"),
    getDiff: missing("getDiff"),
    listTree: missing("listTree"),
    readFile: missing("readFile"),
    writeFile: missing("writeFile"),
    ...routes,
  };
}

export const unreachable = (): HttpError => new HttpError(0, "Cannot reach the server", false);
export const notFound = (): HttpError => new HttpError(404, "session not found", true);

/** Lets promise callbacks that are already resolved run. */
export async function settle(): Promise<void> {
  for (let turn = 0; turn < 5; turn += 1) await Promise.resolve();
}

/** A promise the test settles by hand, to look at the screen while a request is in flight. */
export function deferred<T = void>() {
  let resolve: (value: T) => void = () => {};
  let reject: (reason: unknown) => void = () => {};
  const promise = new Promise<T>((onResolve, onReject) => {
    resolve = onResolve;
    reject = onReject;
  });
  return { promise, resolve, reject };
}

export const conflict = (message: string): HttpError => new HttpError(409, message, true);
export const badRequest = (message: string): HttpError => new HttpError(400, message, true);

/** A repo the home repo declares that has no clone on this machine yet. */
export const makeMissingRepo = (id: string, patch: Partial<Repo> = {}): Repo =>
  makeRepo(id, {
    path: "",
    default_branch: null,
    available: false,
    declared: true,
    url: `git@example.com:you/${id}.git`,
    ...patch,
  });
