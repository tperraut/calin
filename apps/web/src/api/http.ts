import type { AnswerQuestionRequest } from "../gen/AnswerQuestionRequest";
import type { ApiError } from "../gen/ApiError";
import type { AttachRepoRequest } from "../gen/AttachRepoRequest";
import type { AuthSessionInfo } from "../gen/AuthSessionInfo";
import type { Automation } from "../gen/Automation";
import type { AutomationRequest } from "../gen/AutomationRequest";
import type { AutomationRun } from "../gen/AutomationRun";
import type { CreateDeviceTokenRequest } from "../gen/CreateDeviceTokenRequest";
import type { CreatedDeviceToken } from "../gen/CreatedDeviceToken";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { CronPreview } from "../gen/CronPreview";
import type { DeviceTokenInfo } from "../gen/DeviceTokenInfo";
import type { FileContent } from "../gen/FileContent";
import type { FileDiff } from "../gen/FileDiff";
import type { Graph } from "../gen/Graph";
import type { Health } from "../gen/Health";
import type { Issue } from "../gen/Issue";
import type { IssueDetail } from "../gen/IssueDetail";
import type { IssueState } from "../gen/IssueState";
import type { Me } from "../gen/Me";
import type { PublicHealth } from "../gen/PublicHealth";
import type { PushKey } from "../gen/PushKey";
import type { PushPreferences } from "../gen/PushPreferences";
import type { PushSubscriptionInfo } from "../gen/PushSubscriptionInfo";
import type { PushSubscriptionRequest } from "../gen/PushSubscriptionRequest";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoChanges } from "../gen/RepoChanges";
import type { RepoSet } from "../gen/RepoSet";
import type { ResolvePermissionRequest } from "../gen/ResolvePermissionRequest";
import type { RunAutomationRequest } from "../gen/RunAutomationRequest";
import type { RunnerLimits } from "../gen/RunnerLimits";
import type { SendMessageRequest } from "../gen/SendMessageRequest";
import type { Session } from "../gen/Session";
import type { SessionDetail } from "../gen/SessionDetail";
import type { SessionRepo } from "../gen/SessionRepo";
import type { Settings } from "../gen/Settings";
import type { ShipRequest } from "../gen/ShipRequest";
import type { Timeline } from "../gen/Timeline";
import type { TreeEntry } from "../gen/TreeEntry";
import type { Usage } from "../gen/Usage";
import type { UsageBy } from "../gen/UsageBy";
import type { WorkflowInfo } from "../gen/WorkflowInfo";
import type { WorkflowRun } from "../gen/WorkflowRun";
import type { WriteFileRequest } from "../gen/WriteFileRequest";

/** Mount point of every route in `crates/calin-proto/src/api.rs`. */
export const API_BASE = "/api/v1";

/** Where a browser goes to sign in, outside `/api/v1` (api.rs). */
export const AUTH_LOGIN_PATH = "/auth/login";
const AUTH_LOGOUT_PATH = "/auth/logout";

/**
 * A failed request. `status` is 0 when no response arrived. `fromServer` is
 * true only when the body was an `ApiError`, so `message` is the server's own.
 */
export class HttpError extends Error {
  readonly status: number;
  readonly fromServer: boolean;

  constructor(status: number, message: string, fromServer: boolean) {
    super(message);
    this.name = "HttpError";
    this.status = status;
    this.fromServer = fromServer;
  }

  /** A proxy or network failure rather than an answer from Calin. */
  get unreachable(): boolean {
    return !this.fromServer && (this.status === 0 || this.status >= 500);
  }
}

export const UNREACHABLE_MESSAGE = "Cannot reach the server";

/** What `GET /repos/{id}/issues` is asked for; the server's defaults apply to what is absent. */
export type IssueQuery = { state?: IssueState; q?: string };

/** What `GET /usage` is asked for, in Unix milliseconds; the server's defaults apply to what is absent. */
export type UsageQuery = { from?: number; to?: number; by?: UsageBy };

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

export interface Api {
  /**
   * Who is calling and in which mode. Rejects with a 401 `HttpError` when
   * sign-in is required and absent.
   */
  me(): Promise<Me>;
  /** Ends this browser's session (`POST /auth/logout`, outside `/api/v1`). */
  logout(): Promise<void>;
  /**
   * Calls `listener` whenever any request is answered 401: the one place a
   * missing or ended sign-in is noticed. Returns the function that stops it.
   */
  onUnauthorized(listener: () => void): () => void;
  /**
   * The server's description. A reduced `PublicHealth` means the caller is not
   * signed in: it is reported to `onUnauthorized` and rejected as a 401.
   */
  health(): Promise<Health>;
  /** The signed-in identity's browser sessions, the current one first. 409 in local mode. */
  listAuthSessions(): Promise<AuthSessionInfo[]>;
  /** 404 when it is already gone. */
  revokeAuthSession(id: string): Promise<void>;
  /** The signed-in identity's device tokens, newest first. 409 in local mode. */
  listDeviceTokens(): Promise<DeviceTokenInfo[]>;
  /** The answer carries the secret, which the server never shows again. */
  createDeviceToken(body: CreateDeviceTokenRequest): Promise<CreatedDeviceToken>;
  revokeDeviceToken(id: string): Promise<void>;
  /** The server's VAPID public key. The push routes are a 409 in local mode. */
  getPushKey(): Promise<PushKey>;
  /** The devices notified for the signed-in identity, newest first. */
  listPushSubscriptions(): Promise<PushSubscriptionInfo[]>;
  /** An endpoint already registered for the caller is replaced and keeps its `id`. */
  createPushSubscription(body: PushSubscriptionRequest): Promise<PushSubscriptionInfo>;
  deletePushSubscription(id: string): Promise<void>;
  getPushPreferences(): Promise<PushPreferences>;
  putPushPreferences(body: PushPreferences): Promise<PushPreferences>;
  /** Accepted at once; the notification follows through the push service. */
  sendTestPush(): Promise<void>;
  getSettings(): Promise<Settings>;
  /** Replaces the stored settings; answers with them as stored. 400 names what is wrong. */
  putSettings(body: Settings): Promise<Settings>;
  /** Hand-registered repos and those the home repo declares: the home repo first, then by name. */
  listRepos(): Promise<Repo[]>;
  registerRepo(body: RegisterRepoRequest): Promise<Repo>;
  /** Re-reads the home repo's list and rescans; answers with the new repo list. */
  refreshRepos(): Promise<Repo[]>;
  /** Starts cloning a declared repo; it reports `cloning` until it is `available`. */
  cloneRepo(id: string): Promise<void>;
  /** Removes the registry entry only; the clone on disk is untouched. 409 for a declared repo. */
  removeRepo(id: string): Promise<void>;
  /** The home repo's named groups of repos, in its order. */
  listRepoSets(): Promise<RepoSet[]>;
  /**
   * A repo's GitHub issues, newest first, at most 50. 409 with the reason when
   * the repo has no GitHub remote or `gh` cannot be used.
   */
  listIssues(repoId: string, query?: IssueQuery): Promise<Issue[]>;
  /** One issue with its Markdown body; 404 for an unknown one. */
  getIssue(repoId: string, number: number): Promise<IssueDetail>;
  /** Totals and one breakdown over a range (integrations spec, section 1). 400 for an invalid query. */
  getUsage(query?: UsageQuery): Promise<Usage>;
  /** The newest limit windows each runner reported; none after a restart until an agent runs. */
  getUsageLimits(): Promise<RunnerLimits[]>;
  /** By name. */
  listAutomations(): Promise<Automation[]>;
  /** 400 with the reason when it does not validate. */
  createAutomation(body: AutomationRequest): Promise<Automation>;
  /** What a cron expression means, or why it is not valid. */
  previewCron(expr: string): Promise<CronPreview>;
  getAutomation(id: string): Promise<Automation>;
  /** Replaces the automation; 400 with the reason when it does not validate. */
  putAutomation(id: string, body: AutomationRequest): Promise<Automation>;
  /** The sessions it started are kept. */
  deleteAutomation(id: string): Promise<void>;
  /** Fires it now; 409 while its previous session is still active. */
  runAutomation(id: string, body: RunAutomationRequest): Promise<AutomationRun>;
  /** Newest first, at most 50. */
  listAutomationRuns(id: string): Promise<AutomationRun[]>;
  /** The built-in `quick` first, then the home repo's workflows by id; an invalid one carries its problems. */
  listWorkflows(): Promise<WorkflowInfo[]>;
  listSessions(): Promise<Session[]>;
  createSession(body: CreateSessionRequest): Promise<SessionDetail>;
  getSession(id: string): Promise<SessionDetail>;
  sendMessage(id: string, body: SendMessageRequest): Promise<void>;
  interruptSession(id: string): Promise<void>;
  stopSession(id: string): Promise<void>;
  resolvePermission(id: string, requestId: string, body: ResolvePermissionRequest): Promise<void>;
  /** Answers a question an agent asked; 404 for an unknown one, 409 once it is answered. */
  answerQuestion(id: string, questionId: string, body: AnswerQuestionRequest): Promise<void>;
  /**
   * Who spawned whom, who told what to whom and who changed which repo, for the
   * session's whole tree. `at` (Unix milliseconds) asks for it as it was then.
   */
  getGraph(id: string, at?: number): Promise<Graph>;
  /** Every agent's activity over time, for the session's whole tree. */
  getTimeline(id: string): Promise<Timeline>;
  /** The session's workflow run; `null` when it runs none (the server's 404). */
  getWorkflow(id: string): Promise<WorkflowRun | null>;
  /** Attaches a repo to a session that is running, waiting or in review; 409 otherwise. */
  attachRepo(id: string, body: AttachRepoRequest): Promise<SessionRepo>;
  /**
   * Opens, or updates, the session's pull requests. Answers at once: what it
   * did arrives as events. 409 unless the session is in review or shipping, or
   * when GitHub cannot be used.
   */
  shipSession(id: string, body: ShipRequest): Promise<void>;
  /** Has the server look at the session's pull requests now; what changed arrives as events. */
  refreshPullRequests(id: string): Promise<void>;
  /** What the session changed, one entry per attached repo. */
  getChanges(id: string): Promise<RepoChanges[]>;
  /** Both sides of one changed file. `path` is relative to the repo root (api.rs, Semantics). */
  getDiff(id: string, sessionRepoId: string, path: string): Promise<FileDiff>;
  /** One level of a repo's tree; an empty `path` lists the root. */
  listTree(id: string, sessionRepoId: string, path: string): Promise<TreeEntry[]>;
  readFile(id: string, sessionRepoId: string, path: string): Promise<FileContent>;
  /** 409 when `expected_hash` no longer matches the file, or when the repo is attached `read`. */
  writeFile(
    id: string,
    sessionRepoId: string,
    path: string,
    body: WriteFileRequest,
  ): Promise<FileContent>;
}

type Method = "GET" | "POST" | "PUT" | "DELETE";

function isApiError(value: unknown): value is ApiError {
  return (
    typeof value === "object" &&
    value !== null &&
    "error" in value &&
    typeof value.error === "string"
  );
}

async function failure(response: Response): Promise<HttpError> {
  try {
    const body: unknown = await response.json();
    if (isApiError(body)) return new HttpError(response.status, body.error, true);
  } catch {
    // Not JSON: the answer came from something in front of Calin.
  }
  const message = response.status >= 500 ? UNREACHABLE_MESSAGE : `HTTP ${response.status}`;
  return new HttpError(response.status, message, false);
}

/** `PublicHealth` is what an unauthenticated caller gets from `/health` (api.rs). */
function isPublicHealth(value: Health | PublicHealth): value is PublicHealth {
  return "auth" in value && !("version" in value);
}

export const SIGNED_OUT_MESSAGE = "Not signed in";

const sessionPath = (id: string) => `/sessions/${encodeURIComponent(id)}`;
const repoPath = (id: string) => `/repos/${encodeURIComponent(id)}`;
const automationPath = (id: string) => `/automations/${encodeURIComponent(id)}`;
const tokenPath = (id: string) => `/tokens/${encodeURIComponent(id)}`;

// A path is one query value: `/` and every awkward character in it travel percent-encoded.
function inRepoPath(id: string, sessionRepoId: string, route: string, path: string): string {
  return (
    `${sessionPath(id)}/repos/${encodeURIComponent(sessionRepoId)}/${route}` +
    (path === "" ? "" : `?path=${encodeURIComponent(path)}`)
  );
}

/** `?a=1&b=2` from the entries that are set, each value percent-encoded; empty when none is. */
function queryString(params: Record<string, string | number | undefined>): string {
  const parts = Object.entries(params).flatMap(([key, value]) =>
    value === undefined ? [] : [`${key}=${encodeURIComponent(String(value))}`],
  );
  return parts.length === 0 ? "" : `?${parts.join("&")}`;
}

function issuesPath(repoId: string, wanted: IssueQuery = {}): string {
  const q = wanted.q === "" ? undefined : wanted.q;
  return `${repoPath(repoId)}/issues${queryString({ state: wanted.state, q })}`;
}

const rounded = (value: number | undefined) =>
  value === undefined ? undefined : Math.round(value);

function usagePath(wanted: UsageQuery = {}): string {
  return `/usage${queryString({ from: rounded(wanted.from), to: rounded(wanted.to), by: wanted.by })}`;
}

function graphPath(id: string, at: number | undefined): string {
  return `${sessionPath(id)}/graph${at === undefined ? "" : `?at=${Math.round(at)}`}`;
}

/** api.rs: 404 is "this session runs no workflow", an answer rather than a failure. */
function isNoWorkflow(error: unknown): boolean {
  return error instanceof HttpError && error.status === 404 && error.fromServer;
}

function requestInit(method: Method, body: unknown): RequestInit {
  const headers: Record<string, string> = { Accept: "application/json" };
  // The session cookie travels on its own (same origin): it is HttpOnly and the app never holds it.
  const init: RequestInit = { method, headers };
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  return init;
}

/** Sends requests and turns every failure into an `HttpError`; 401s also reach the listeners. */
class Transport {
  private readonly unauthorizedListeners = new Set<() => void>();
  private readonly fetchFn: FetchLike;
  private readonly base: string;

  constructor(fetchFn: FetchLike, base: string) {
    this.fetchFn = fetchFn;
    this.base = base;
  }

  onUnauthorized(listener: () => void): () => void {
    this.unauthorizedListeners.add(listener);
    return () => this.unauthorizedListeners.delete(listener);
  }

  notifyUnauthorized(): void {
    for (const listener of this.unauthorizedListeners) listener();
  }

  /** `url` is absolute on this origin; `base` applies only through `json` and `empty`. */
  async sendTo(url: string, method: Method, body?: unknown): Promise<Response> {
    const response = await this.fetchOrUnreachable(url, requestInit(method, body));
    if (response.status === 401) throw await this.signedOut(response);
    if (!response.ok) throw await failure(response);
    return response;
  }

  async json<T>(method: Method, path: string, body?: unknown): Promise<T> {
    const response = await this.sendTo(this.base + path, method, body);
    try {
      // The generated types are the contract; the server is trusted to honor it.
      return (await response.json()) as T;
    } catch {
      throw new HttpError(response.status, "Unexpected response from the server", false);
    }
  }

  async empty(method: Method, path: string, body?: unknown): Promise<void> {
    await this.sendTo(this.base + path, method, body);
  }

  private async fetchOrUnreachable(url: string, init: RequestInit): Promise<Response> {
    try {
      return await this.fetchFn(url, init);
    } catch {
      throw new HttpError(0, UNREACHABLE_MESSAGE, false);
    }
  }

  private async signedOut(response: Response): Promise<HttpError> {
    const refusal = await failure(response);
    this.notifyUnauthorized();
    return refusal.fromServer ? refusal : new HttpError(401, SIGNED_OUT_MESSAGE, false);
  }
}

const browserFetch: FetchLike = (input, init) => fetch(input, init);

export function createApi(fetchFn: FetchLike = browserFetch, base: string = API_BASE): Api {
  const http = new Transport(fetchFn, base);
  const json = <T>(method: Method, path: string, body?: unknown) =>
    http.json<T>(method, path, body);
  const empty = (method: Method, path: string, body?: unknown) => http.empty(method, path, body);

  return {
    me: () => json<Me>("GET", "/me"),
    async logout() {
      await http.sendTo(AUTH_LOGOUT_PATH, "POST");
    },
    onUnauthorized: (listener) => http.onUnauthorized(listener),
    async health() {
      const loaded = await json<Health | PublicHealth>("GET", "/health");
      if (!isPublicHealth(loaded)) return loaded;
      http.notifyUnauthorized();
      throw new HttpError(401, SIGNED_OUT_MESSAGE, false);
    },
    listAuthSessions: () => json<AuthSessionInfo[]>("GET", "/auth/sessions"),
    revokeAuthSession: (id) => empty("DELETE", `/auth/sessions/${encodeURIComponent(id)}`),
    listDeviceTokens: () => json<DeviceTokenInfo[]>("GET", "/tokens"),
    createDeviceToken: (body) => json<CreatedDeviceToken>("POST", "/tokens", body),
    revokeDeviceToken: (id) => empty("DELETE", tokenPath(id)),
    getPushKey: () => json<PushKey>("GET", "/push/key"),
    listPushSubscriptions: () => json<PushSubscriptionInfo[]>("GET", "/push/subscriptions"),
    createPushSubscription: (body) =>
      json<PushSubscriptionInfo>("POST", "/push/subscriptions", body),
    deletePushSubscription: (id) =>
      empty("DELETE", `/push/subscriptions/${encodeURIComponent(id)}`),
    getPushPreferences: () => json<PushPreferences>("GET", "/push/preferences"),
    putPushPreferences: (body) => json<PushPreferences>("PUT", "/push/preferences", body),
    sendTestPush: () => empty("POST", "/push/test"),
    getSettings: () => json<Settings>("GET", "/settings"),
    putSettings: (body) => json<Settings>("PUT", "/settings", body),
    listRepos: () => json<Repo[]>("GET", "/repos"),
    registerRepo: (body) => json<Repo>("POST", "/repos", body),
    refreshRepos: () => json<Repo[]>("POST", "/repos/refresh"),
    cloneRepo: (id) => empty("POST", `${repoPath(id)}/clone`),
    removeRepo: (id) => empty("DELETE", repoPath(id)),
    listRepoSets: () => json<RepoSet[]>("GET", "/repo-sets"),
    listIssues: (repoId, wanted) => json<Issue[]>("GET", issuesPath(repoId, wanted)),
    getIssue: (repoId, number) =>
      json<IssueDetail>("GET", `${repoPath(repoId)}/issues/${encodeURIComponent(String(number))}`),
    getUsage: (wanted) => json<Usage>("GET", usagePath(wanted)),
    getUsageLimits: () => json<RunnerLimits[]>("GET", "/usage/limits"),
    listAutomations: () => json<Automation[]>("GET", "/automations"),
    createAutomation: (body) => json<Automation>("POST", "/automations", body),
    previewCron: (expr) => json<CronPreview>("GET", `/automations/cron${queryString({ expr })}`),
    getAutomation: (id) => json<Automation>("GET", automationPath(id)),
    putAutomation: (id, body) => json<Automation>("PUT", automationPath(id), body),
    deleteAutomation: (id) => empty("DELETE", automationPath(id)),
    runAutomation: (id, body) => json<AutomationRun>("POST", `${automationPath(id)}/run`, body),
    listAutomationRuns: (id) => json<AutomationRun[]>("GET", `${automationPath(id)}/runs`),
    listWorkflows: () => json<WorkflowInfo[]>("GET", "/workflows"),
    listSessions: () => json<Session[]>("GET", "/sessions"),
    createSession: (body) => json<SessionDetail>("POST", "/sessions", body),
    getSession: (id) => json<SessionDetail>("GET", sessionPath(id)),
    sendMessage: (id, body) => empty("POST", `${sessionPath(id)}/messages`, body),
    interruptSession: (id) => empty("POST", `${sessionPath(id)}/interrupt`),
    stopSession: (id) => empty("POST", `${sessionPath(id)}/stop`),
    resolvePermission: (id, requestId, body) =>
      empty("POST", `${sessionPath(id)}/permissions/${encodeURIComponent(requestId)}`, body),
    answerQuestion: (id, questionId, body) =>
      empty("POST", `${sessionPath(id)}/questions/${encodeURIComponent(questionId)}`, body),
    getGraph: (id, at) => json<Graph>("GET", graphPath(id, at)),
    getTimeline: (id) => json<Timeline>("GET", `${sessionPath(id)}/timeline`),
    async getWorkflow(id) {
      try {
        return await json<WorkflowRun>("GET", `${sessionPath(id)}/workflow`);
      } catch (error) {
        if (isNoWorkflow(error)) return null;
        throw error;
      }
    },
    attachRepo: (id, body) => json<SessionRepo>("POST", `${sessionPath(id)}/repos`, body),
    shipSession: (id, body) => empty("POST", `${sessionPath(id)}/ship`, body),
    refreshPullRequests: (id) => empty("POST", `${sessionPath(id)}/pull-requests/refresh`),
    getChanges: (id) => json<RepoChanges[]>("GET", `${sessionPath(id)}/changes`),
    getDiff: (id, repo, path) => json<FileDiff>("GET", inRepoPath(id, repo, "diff", path)),
    listTree: (id, repo, path) => json<TreeEntry[]>("GET", inRepoPath(id, repo, "tree", path)),
    readFile: (id, repo, path) => json<FileContent>("GET", inRepoPath(id, repo, "file", path)),
    writeFile: (id, repo, path, body) =>
      json<FileContent>("PUT", inRepoPath(id, repo, "file", path), body),
  };
}
