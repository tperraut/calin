import { createContext, createSignal, type Accessor, type Setter } from "solid-js";
import { HttpError, type Api, type IssueQuery, type UsageQuery } from "../api/http";
import {
  TOPIC_SESSIONS,
  type ConnectionState,
  type SocketClient,
  type TopicMsg,
} from "../api/socket";
import type { OpenTerminal } from "../api/terminal";
import type { WorkerMessage } from "../sw/protocol";
import type { AuthSessionInfo } from "../gen/AuthSessionInfo";
import type { Automation } from "../gen/Automation";
import type { AutomationRequest } from "../gen/AutomationRequest";
import type { AutomationRun } from "../gen/AutomationRun";
import type { CreatedDeviceToken } from "../gen/CreatedDeviceToken";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { CronPreview } from "../gen/CronPreview";
import type { DeviceTokenInfo } from "../gen/DeviceTokenInfo";
import type { Health } from "../gen/Health";
import type { Issue } from "../gen/Issue";
import type { IssueDetail } from "../gen/IssueDetail";
import type { Me } from "../gen/Me";
import type { PushPreferences } from "../gen/PushPreferences";
import type { PushSubscriptionInfo } from "../gen/PushSubscriptionInfo";
import type { PushSubscriptionRequest } from "../gen/PushSubscriptionRequest";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoSet } from "../gen/RepoSet";
import type { RunnerLimits } from "../gen/RunnerLimits";
import type { Session } from "../gen/Session";
import type { Settings } from "../gen/Settings";
import type { Usage } from "../gen/Usage";
import type { WorkflowInfo } from "../gen/WorkflowInfo";
import { signInUrl } from "../lib/auth";
import { NO_DEVICE, type Device } from "../lib/device";
import type { WorkerBridge } from "../lib/serviceWorker";
import { errorMessage } from "../lib/errors";
import { coalesce, type Schedule } from "../lib/schedule";
import { createCatalog, type Catalog, type RepoRegistry } from "./catalog";
import { mergeAllLimits, mergeLimits } from "./limits";
import { createSessionStore, type SessionStore } from "./sessionStore";
import { RemoteValue } from "./remote";
import { upsertSession } from "./sessionsList";

export type HealthState =
  | { status: "loading" }
  | { status: "ready"; health: Health }
  | { status: "unreachable" };

/** The runners the server offers, default first; none until it has answered. */
export const healthRunners = (health: HealthState): readonly string[] =>
  health.status === "ready" ? health.health.runners : [];

export type { RepoRegistry } from "./catalog";

/**
 * Who the app is talking for, as `GET /me` answered (remote-access spec, sections 1 to 3).
 * `checking`: nothing else is asked or opened until it answers.
 * `unreachable`: it did not answer, or not as Calin; it is asked again on its own.
 * `signed_out`: a 401. `ended` when a session was in use, or the server came to refuse it.
 */
export type AuthState =
  | { status: "checking" }
  | { status: "unreachable" }
  | { status: "local" }
  | { status: "signed_in"; identity: string }
  | { status: "signed_out"; ended: boolean };

/** The machine's settings, as the server last answered. */
export type SettingsState =
  | { status: "loading" }
  | { status: "ready"; settings: Settings }
  | { status: "error"; message: string };

/** The workflows a session can run, as `GET /workflows` last answered. */
export type WorkflowCatalog =
  | { status: "loading" }
  | { status: "ready"; workflows: readonly WorkflowInfo[] }
  | { status: "error"; message: string };

/** The workflows listed; none until the list is read. */
export const catalogWorkflows = (catalog: WorkflowCatalog | undefined): readonly WorkflowInfo[] =>
  catalog?.status === "ready" ? catalog.workflows : [];

export type AppServices = {
  api: Api;
  socket: SocketClient;
  schedule?: Schedule;
  /** Opens a terminal socket; see `createTerminalOpener`. */
  openTerminal?: OpenTerminal;
  /** Leaves the app for `url` with a full navigation; defaults to `location.assign`. */
  leave?: (url: string) => void;
  /** Notification support of this browser; none when absent. */
  device?: Device;
  /** The registered service worker; absent under the development server and in tests. */
  worker?: WorkerBridge;
};

/** Everything components read and do. They get it from `AppContext`, never from `src/api`. */
export type AppState = {
  auth: Accessor<AuthState>;
  /** The signed-in identity, in public mode only. */
  identity: Accessor<string | undefined>;
  /** Navigates the browser to sign in, coming back to where it is now. */
  signIn(): void;
  /** Ends this browser's session; the sign-in screen follows. Rejects with the server's reason. */
  signOut(): Promise<void>;
  /** What unsaved work the open screen holds, said on the sign-in screen when the session ends. */
  unsavedWork: Accessor<string | undefined>;
  setUnsavedWork(description: string | undefined): void;
  /** The browser sessions of the signed-in identity, the current one first. */
  listAuthSessions(): Promise<AuthSessionInfo[]>;
  /** Ends another session; one that is already gone counts as ended. */
  revokeAuthSession(id: string): Promise<void>;
  listDeviceTokens(): Promise<DeviceTokenInfo[]>;
  /** The answer holds the secret: keep it on screen only until the user is done with it. */
  createDeviceToken(name: string): Promise<CreatedDeviceToken>;
  revokeDeviceToken(id: string): Promise<void>;
  connection: Accessor<ConnectionState>;
  health: Accessor<HealthState>;
  /** Why this server cannot open pull requests (`Health::github_problem`), when it says so. */
  githubProblem: Accessor<string | undefined>;
  /** Newest first. */
  sessions: Accessor<readonly Session[]>;
  /** True once the server has sent every existing session. */
  sessionsLoaded: Accessor<boolean>;
  /** The server's reason when it refused the `sessions` topic. */
  sessionsError: Accessor<string | undefined>;
  /** The home repo first, then by name. Stays `loading` until a screen calls `loadRepos`. */
  repos: Accessor<RepoRegistry>;
  /** The home repo's named groups of repos; read with the repos. */
  repoSets: Accessor<readonly RepoSet[]>;
  /** Reads the repos and the sets, and keeps them fresh across reconnects from then on. */
  loadRepos(): void;
  registerRepo(request: RegisterRepoRequest): Promise<Repo>;
  /** Forgets the registry entry; the clone on disk is untouched. */
  removeRepo(id: string): Promise<void>;
  /** Starts cloning a declared repo; `repos` follows the clone until it ends. */
  cloneRepo(id: string): Promise<void>;
  /** Has the server re-read the home repo's list and rescan. */
  rescanRepos(): Promise<void>;
  /** Stays `loading` until a screen calls `loadWorkflows`. */
  workflows: Accessor<WorkflowCatalog>;
  /** Reads the workflows, and again after a reconnect or a change of settings from then on. */
  loadWorkflows(): void;
  /** Stays `loading` until a screen calls `loadSettings`. */
  settings: Accessor<SettingsState>;
  loadSettings(): void;
  /** Replaces the settings. Rejects with the server's reason; the repos follow a change. */
  saveSettings(settings: Settings): Promise<Settings>;
  /** A clock that ticks once a minute, for relative times. */
  now: Accessor<number>;
  /**
   * The newest limit windows per runner, by runner: what `GET /usage/limits`
   * said, then each `limits` event of a session open here (integrations spec, section 1).
   */
  limits: Accessor<readonly RunnerLimits[]>;
  /** Reads `GET /usage/limits` again; what the events said since stays when it is newer. */
  loadLimits(): void;
  /** A repo's GitHub issues; rejects with the server's reason (a 409 when GitHub cannot be used). */
  listIssues(repoId: string, query: IssueQuery): Promise<Issue[]>;
  getIssue(repoId: string, number: number): Promise<IssueDetail>;
  /** Totals and one breakdown over a range; rejects with the server's reason. */
  usage(query: UsageQuery): Promise<Usage>;
  listAutomations(): Promise<Automation[]>;
  getAutomation(id: string): Promise<Automation>;
  /** Creates the automation when `id` is absent, replaces it otherwise. Rejects with the server's reason. */
  saveAutomation(id: string | undefined, request: AutomationRequest): Promise<Automation>;
  deleteAutomation(id: string): Promise<void>;
  previewCron(expr: string): Promise<CronPreview>;
  /** `number` names the issue or pull request for a trigger; absent for a schedule. */
  runAutomation(id: string, number?: number): Promise<AutomationRun>;
  automationRuns(id: string): Promise<AutomationRun[]>;
  createSession(request: CreateSessionRequest): Promise<Session>;
  openSession(id: string): SessionStore;
  /** Asks again for the server, without waiting for the backoff. */
  retry(): void;
  /** What the browser says about its network connection. */
  online: Accessor<boolean>;
  /** True once a newer build of the app is installed and waits for a reload. */
  updateReady: Accessor<boolean>;
  /** Reloads into the newer build. */
  applyUpdate(): void;
  /** Called when a tapped notification asks to show `path`; returns the function that stops it. */
  onNavigateRequest(listener: (path: string) => void): () => void;
  /** This browser's notification support and subscription. */
  device: Device;
  /** The server's VAPID public key, base64url. */
  pushKey(): Promise<string>;
  listPushSubscriptions(): Promise<PushSubscriptionInfo[]>;
  /** Registers this browser's subscription; the same endpoint keeps its id. */
  registerPushSubscription(request: PushSubscriptionRequest): Promise<PushSubscriptionInfo>;
  removePushSubscription(id: string): Promise<void>;
  pushPreferences(): Promise<PushPreferences>;
  /** Rejects with the server's reason. */
  savePushPreferences(preferences: PushPreferences): Promise<PushPreferences>;
  sendTestPush(): Promise<void>;
  /** Connects and loads; returns the function that undoes both. */
  start(): () => void;
};

export const AppContext = createContext<AppState>();

// Relative times ("3 min ago") change at most once a minute.
const CLOCK_TICK_MS = 60_000;
const ME_RETRY_BASE_MS = 1000;
const ME_RETRY_MAX_MS = 15_000;

/** The sign-in state `/me` answered. Pure. */
export const meAuth = (me: Me): AuthState =>
  me.mode === "local" ? { status: "local" } : { status: "signed_in", identity: me.identity ?? "" };

const isStatus = (error: unknown, status: number): boolean =>
  error instanceof HttpError && error.status === status;

/**
 * What a failed `/me` means. `handled`: a 401, which went to `endSession` through
 * the http layer already. `local`: a server from before sign-in existed, which
 * knows no `/me` and no sign-in either. `unreachable` otherwise. Pure.
 */
export function meFailure(error: unknown): "handled" | "local" | "unreachable" {
  if (isStatus(error, 401)) return "handled";
  if (error instanceof HttpError && error.status === 404 && error.fromServer) return "local";
  return "unreachable";
}

/** How long to wait before asking `/me` again after `attempt` failures. Pure. */
export const meRetryDelay = (attempt: number): number =>
  Math.min(ME_RETRY_MAX_MS, ME_RETRY_BASE_MS * 2 ** attempt);

/** `/me` has not answered as Calin yet: nobody is signed in, so nothing has ended. Pure. */
export const authPending = (auth: AuthState): boolean =>
  auth.status === "checking" || auth.status === "unreachable";

const identityOf = (auth: AuthState): string | undefined =>
  auth.status === "signed_in" ? auth.identity : undefined;

const healthOf = (state: HealthState): Health | undefined =>
  state.status === "ready" ? state.health : undefined;

const githubProblemOf = (health: Health | undefined): string | undefined =>
  health?.github_problem ?? undefined;

/** Waits for `request`; a refusal with `status` counts as done. */
async function tolerating(request: Promise<void>, status: number): Promise<void> {
  try {
    await request;
  } catch (error) {
    if (!isStatus(error, status)) throw error;
  }
}

/**
 * The worker renewed this browser's push subscription while no page could
 * tell the server (see `sw/worker.ts`): tell it now.
 */
async function reportRenewal(device: Device, api: Api): Promise<void> {
  try {
    if (!(await device.takeRenewal())) return;
    const current = await device.subscription();
    if (current) await api.createPushSubscription(current.request);
  } catch {
    // Settings offers "Notify this device" again if this did not reach the server.
  }
}

const assignLocation = (url: string): void => window.location.assign(url);

/** The routes components call as they are, with no state of the app's own. */
function serverCalls(api: Api) {
  return {
    listAuthSessions: () => api.listAuthSessions(),
    // api.rs: an unknown id is a 404, which here means it ended some other way already.
    revokeAuthSession: (id: string) => tolerating(api.revokeAuthSession(id), 404),
    listDeviceTokens: () => api.listDeviceTokens(),
    createDeviceToken: (name: string) => api.createDeviceToken({ name }),
    revokeDeviceToken: (id: string) => api.revokeDeviceToken(id),
    listIssues: (repoId: string, query: IssueQuery) => api.listIssues(repoId, query),
    getIssue: (repoId: string, number: number) => api.getIssue(repoId, number),
    usage: (query: UsageQuery) => api.getUsage(query),
    listAutomations: () => api.listAutomations(),
    getAutomation: (id: string) => api.getAutomation(id),
    saveAutomation: (id: string | undefined, request: AutomationRequest) =>
      id === undefined ? api.createAutomation(request) : api.putAutomation(id, request),
    deleteAutomation: (id: string) => api.deleteAutomation(id),
    previewCron: (expr: string) => api.previewCron(expr),
    runAutomation: (id: string, number?: number) =>
      api.runAutomation(id, number === undefined ? {} : { number }),
    automationRuns: (id: string) => api.listAutomationRuns(id),
    pushKey: async () => (await api.getPushKey()).public_key,
    listPushSubscriptions: () => api.listPushSubscriptions(),
    registerPushSubscription: (request: PushSubscriptionRequest) =>
      api.createPushSubscription(request),
    removePushSubscription: (id: string) => api.deletePushSubscription(id),
    pushPreferences: () => api.getPushPreferences(),
    savePushPreferences: (preferences: PushPreferences) => api.putPushPreferences(preferences),
    sendTestPush: () => api.sendTestPush(),
  } satisfies Partial<AppState>;
}

/** The sessions list as the `sessions` topic sends it, published once per scheduled slot. */
class SessionsFeed {
  readonly list: Accessor<readonly Session[]>;
  readonly loaded: Accessor<boolean>;
  readonly error: Accessor<string | undefined>;
  private readonly setList: Setter<readonly Session[]>;
  private readonly setLoaded: Setter<boolean>;
  private readonly setError: Setter<string | undefined>;
  private readonly publish: () => void;
  private current: readonly Session[] = [];
  private complete = false;

  constructor(schedule: Schedule | undefined) {
    [this.list, this.setList] = createSignal<readonly Session[]>([]);
    [this.loaded, this.setLoaded] = createSignal(false);
    [this.error, this.setError] = createSignal<string>();
    this.publish = coalesce(() => {
      this.setList(this.current);
      this.setLoaded(this.complete);
    }, schedule);
  }

  upsert(session: Session): void {
    this.current = upsertSession(this.current, session);
    this.publish();
  }

  receive(msg: TopicMsg): void {
    if (msg.op === "session_upserted") {
      this.upsert(msg.session);
    } else if (msg.op === "subscribed") {
      this.complete = true;
      this.setError(undefined);
      this.publish();
    } else if (msg.op === "error") {
      this.setError(msg.message);
    }
  }

  reset(): void {
    this.current = [];
    this.complete = false;
    this.publish();
    this.setError(undefined);
  }
}

/** The newest limit windows per runner: `GET /usage/limits`, then each `limits` event. */
class LimitsFeed {
  readonly list: Accessor<readonly RunnerLimits[]>;
  private readonly setList: Setter<readonly RunnerLimits[]>;
  private readonly api: Api;
  private current: readonly RunnerLimits[] = [];
  private inFlight = false;

  constructor(api: Api) {
    this.api = api;
    [this.list, this.setList] = createSignal<readonly RunnerLimits[]>([]);
  }

  take(next: readonly RunnerLimits[]): void {
    if (next === this.current) return;
    this.current = next;
    this.setList(next);
  }

  report(report: RunnerLimits): void {
    this.take(mergeLimits(this.current, report));
  }

  load(): void {
    if (this.inFlight) return;
    this.inFlight = true;
    this.api.getUsageLimits().then(
      (loaded) => {
        this.inFlight = false;
        this.take(mergeAllLimits(this.current, loaded));
      },
      () => {
        // Limits are a hint wherever they show: without them the screens simply say nothing.
        this.inFlight = false;
      },
    );
  }
}

// A class rather than a closure of methods: each method then stands, and is measured, on its own.
// `private`, not `#`: the complexity report does not see `#` methods.
class AppCore {
  readonly auth: Accessor<AuthState>;
  readonly connection: Accessor<ConnectionState>;
  readonly now: Accessor<number>;
  readonly online: Accessor<boolean>;
  readonly updateReady: Accessor<boolean>;
  readonly sessions: SessionsFeed;
  readonly limits: LimitsFeed;
  readonly catalog: Catalog;
  readonly settings: RemoteValue<Settings, SettingsState>;
  readonly workflows: RemoteValue<readonly WorkflowInfo[], WorkflowCatalog>;
  readonly health: RemoteValue<Health, HealthState>;
  readonly device: Device;
  readonly navigateListeners = new Set<(path: string) => void>();
  private readonly setAuth: Setter<AuthState>;
  private readonly setConnection: Setter<ConnectionState>;
  private readonly setNow: Setter<number>;
  private readonly setOnline: Setter<boolean>;
  private readonly setUpdateReady: Setter<boolean>;
  private readonly services: AppServices;
  private readonly api: Api;
  private readonly socket: SocketClient;
  // Signals apply later; decisions made in the same turn read this.
  private authNow: AuthState = { status: "checking" };
  private running = false;
  private disconnect: (() => void) | undefined;
  private meInFlight = false;
  private meAttempt = 0;
  private meTimer: ReturnType<typeof setTimeout> | undefined;
  private opens = 0;
  private probing = false;

  constructor(services: AppServices) {
    this.services = services;
    this.api = services.api;
    this.socket = services.socket;
    this.device = services.device ?? NO_DEVICE;
    [this.auth, this.setAuth] = createSignal<AuthState>(this.authNow);
    [this.connection, this.setConnection] = createSignal<ConnectionState>(this.socket.state());
    [this.now, this.setNow] = createSignal(Date.now());
    [this.online, this.setOnline] = createSignal(
      typeof navigator === "undefined" || navigator.onLine,
    );
    [this.updateReady, this.setUpdateReady] = createSignal(false);
    this.sessions = new SessionsFeed(services.schedule);
    this.limits = new LimitsFeed(this.api);
    this.catalog = createCatalog(this.api);
    this.settings = new RemoteValue<Settings, SettingsState>({
      read: () => this.api.getSettings(),
      ready: (settings) => ({ status: "ready", settings }),
      failed: (error) => ({ status: "error", message: errorMessage(error) }),
    });
    this.workflows = new RemoteValue<readonly WorkflowInfo[], WorkflowCatalog>({
      read: () => this.api.listWorkflows(),
      ready: (workflows) => ({ status: "ready", workflows }),
      failed: (error) => ({ status: "error", message: errorMessage(error) }),
    });
    // A health that was read once stays on screen; the connection indicator covers outages.
    this.health = new RemoteValue<Health, HealthState>({
      read: () => this.api.health(),
      ready: (health) => ({ status: "ready", health }),
      failed: () => ({ status: "unreachable" }),
    });
  }

  knownHealth(): Health | undefined {
    return healthOf(this.health.state());
  }

  signIn(): void {
    (this.services.leave ?? assignLocation)(signInUrl(window.location));
  }

  async saveSettings(next: Settings): Promise<Settings> {
    const stored = await this.api.putSettings(next);
    this.settings.take(stored);
    // api.rs: a change re-reads the home repo and rescans, so the repo list is another one now.
    this.catalog.invalidate();
    // The workflows live in the home repo, which may be another one now.
    this.workflows.refresh(true);
    return stored;
  }

  openSession(id: string): SessionStore {
    const sessions = this.sessions.list;
    return createSessionStore(
      {
        api: this.api,
        socket: this.socket,
        listed: (wanted) => sessions().find((session) => session.id === wanted),
        sessions,
        schedule: this.services.schedule,
        openTerminal: this.services.openTerminal,
        health: () => this.knownHealth(),
        checkHealth: () => this.health.read(),
        now: this.now,
        onLimits: (report) => this.limits.report(report),
      },
      id,
    );
  }

  retry(): void {
    if (authPending(this.authNow)) {
      this.askMeNow();
      return;
    }
    if (this.disconnect === undefined) return;
    this.health.showLoading();
    this.health.read();
    this.catalog.reload({ missingOnly: true });
    this.settings.refresh(false);
    this.workflows.refresh(false);
    if (this.sessions.error() !== undefined) this.socket.resync(TOPIC_SESSIONS, 0);
    this.socket.reconnectNow();
  }

  /** The one reaction to a 401 anywhere (see `Api.onUnauthorized`): stop, forget, ask to sign in. */
  endSession(ended: boolean): void {
    if (this.authNow.status === "signed_out") return;
    this.changeAuth({ status: "signed_out", ended });
    clearTimeout(this.meTimer);
    this.disconnectNow();
    // For good: signing in again is a full navigation, which starts a new app.
    this.socket.close();
    this.sessions.reset();
    this.catalog.forget();
    this.settings.reset();
    this.workflows.reset();
    this.health.reset();
    this.limits.take([]);
  }

  start(): () => void {
    this.running = true;
    // A 401 before anyone was signed in is a sign-in to do, not one that ended.
    const offUnauthorized = this.api.onUnauthorized(() =>
      this.endSession(!authPending(this.authNow)),
    );
    const wentOnline = (): void => this.wentOnline();
    const wentOffline = (): void => {
      this.setOnline(false);
    };
    window.addEventListener("online", wentOnline);
    window.addEventListener("offline", wentOffline);
    const offWorker = this.listenToWorker();
    this.checkMe();
    return () => {
      this.running = false;
      window.removeEventListener("online", wentOnline);
      window.removeEventListener("offline", wentOffline);
      offWorker();
      offUnauthorized();
      clearTimeout(this.meTimer);
      this.disconnectNow();
    };
  }

  private changeAuth(next: AuthState): void {
    this.authNow = next;
    this.setAuth(next);
  }

  private disconnectNow(): void {
    this.disconnect?.();
    this.disconnect = undefined;
  }

  private listenToWorker(): () => void {
    const worker = this.services.worker;
    if (!worker) return () => {};
    const offReady = worker.onUpdateReady(() => this.setUpdateReady(true));
    const offMessages = worker.onMessage((message) => this.workerSaid(message));
    return () => {
      offReady();
      offMessages();
    };
  }

  /** Back online: ask for the server now rather than at the end of the backoff. */
  private wentOnline(): void {
    this.setOnline(true);
    if (this.authNow.status === "unreachable") this.askMeNow();
  }

  private askMeNow(): void {
    this.meAttempt = 0;
    this.checkMe();
  }

  private checkMe(): void {
    if (this.meInFlight || !this.running) return;
    clearTimeout(this.meTimer);
    this.meTimer = undefined;
    this.meInFlight = true;
    this.api.me().then(
      (me) => this.meAnswered(me),
      (error: unknown) => this.meFailed(error),
    );
  }

  private meAnswered(me: Me): void {
    this.meInFlight = false;
    if (!this.running || this.authNow.status === "signed_out") return;
    this.allowed(meAuth(me));
  }

  private meFailed(error: unknown): void {
    this.meInFlight = false;
    if (!this.running) return;
    const failure = meFailure(error);
    if (failure === "local") this.allowed({ status: "local" });
    else if (failure === "unreachable") this.unreachable();
  }

  private unreachable(): void {
    this.changeAuth({ status: "unreachable" });
    // The "offline" event may have fired before this page listened.
    this.setOnline(navigator.onLine);
    const delay = meRetryDelay(this.meAttempt);
    this.meAttempt += 1;
    this.meTimer = setTimeout(() => this.checkMe(), delay);
  }

  private allowed(next: AuthState): void {
    this.meAttempt = 0;
    this.changeAuth(next);
    this.disconnect ??= this.connect();
    if (next.status === "signed_in") this.reportRenewedSubscription();
  }

  /** Push exists in public mode only. */
  private reportRenewedSubscription(): void {
    if (this.device.pushSupport() === "supported") void reportRenewal(this.device, this.api);
  }

  private workerSaid(message: WorkerMessage): void {
    if (message.type === "navigate") {
      for (const listener of this.navigateListeners) listener(message.path);
    } else if (message.type === "refresh") {
      // The session is live over the socket; after a phone's sleep the socket may not be.
      this.socket.reconnectNow();
    } else if (message.type === "subscription-changed" && this.authNow.status === "signed_in") {
      this.reportRenewedSubscription();
    }
  }

  /** Subscribes and listens; returns the function that undoes it. Only once `/me` allowed it. */
  private connect(): () => void {
    this.catalog.setActive(true);
    this.opens = 0;
    this.probing = false;
    const offState = this.socket.onState((state) => this.socketState(state));
    const offRefused = this.socket.onRefused(() => this.probeMe());
    const unsubscribe = this.socket.subscribe(TOPIC_SESSIONS, (msg) => this.sessions.receive(msg));
    this.setConnection(this.socket.state());
    this.health.read();
    const clock = setInterval(() => this.setNow(Date.now()), CLOCK_TICK_MS);
    const wake = (): void => this.wake();
    document.addEventListener("visibilitychange", wake);
    window.addEventListener("online", wake);
    return () => {
      this.catalog.setActive(false);
      offState();
      offRefused();
      unsubscribe();
      clearInterval(clock);
      document.removeEventListener("visibilitychange", wake);
      window.removeEventListener("online", wake);
    };
  }

  private socketState(state: ConnectionState): void {
    this.setConnection(state);
    if (state === "open") this.opened();
  }

  private opened(): void {
    this.opens += 1;
    const again = this.opens > 1;
    // After a reconnect the server may be another build with other runners.
    if (again || !this.health.known) this.health.read();
    this.catalog.reload({ missingOnly: !again });
    this.limits.load();
    this.settings.refresh(false);
    // After a reconnect the home repo's files may have changed.
    this.workflows.refresh(again);
  }

  /** A refused upgrade looks like any failed connection: `/me` tells, and its 401 ends the session. */
  private probeMe(): void {
    if (this.probing) return;
    this.probing = true;
    const done = (): void => {
      this.probing = false;
    };
    this.api.me().then(done, done);
  }

  /** Phones drop the socket in the background; do not sit out the backoff on return. */
  private wake(): void {
    if (document.visibilityState === "visible") this.socket.reconnectNow();
  }
}

export function createAppState(services: AppServices): AppState {
  const core = new AppCore(services);
  const { api, worker } = services;
  const { catalog, sessions, settings, workflows, health, limits } = core;
  const [unsavedWork, setUnsavedWork] = createSignal<string>();
  return {
    ...serverCalls(api),
    auth: core.auth,
    identity: () => identityOf(core.auth()),
    signIn: () => core.signIn(),
    async signOut() {
      // Already signed out on the server: the screen only has to catch up.
      await tolerating(api.logout(), 401);
      core.endSession(false);
    },
    unsavedWork,
    setUnsavedWork: (description) => setUnsavedWork(description),
    connection: core.connection,
    health: health.state,
    githubProblem: () => githubProblemOf(core.knownHealth()),
    sessions: sessions.list,
    sessionsLoaded: sessions.loaded,
    sessionsError: sessions.error,
    repos: catalog.repos,
    repoSets: catalog.sets,
    loadRepos: () => catalog.load(),
    registerRepo: (request) => catalog.register(request),
    removeRepo: (id) => catalog.remove(id),
    cloneRepo: (id) => catalog.clone(id),
    rescanRepos: () => catalog.rescan(),
    workflows: workflows.state,
    loadWorkflows: () => workflows.want(),
    settings: settings.state,
    loadSettings() {
      settings.showLoading();
      settings.want();
    },
    saveSettings: (next) => core.saveSettings(next),
    now: core.now,
    limits: limits.list,
    loadLimits: () => limits.load(),
    async createSession(request) {
      const { session } = await api.createSession(request);
      sessions.upsert(session);
      return session;
    },
    openSession: (id) => core.openSession(id),
    retry: () => core.retry(),
    online: core.online,
    updateReady: core.updateReady,
    applyUpdate: () => worker?.applyUpdate(),
    onNavigateRequest(listener) {
      core.navigateListeners.add(listener);
      return () => core.navigateListeners.delete(listener);
    },
    device: core.device,
    start: () => core.start(),
  };
}
