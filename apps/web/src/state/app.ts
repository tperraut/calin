import { createContext, createSignal, type Accessor } from "solid-js";
import { HttpError, type Api, type IssueQuery, type UsageQuery } from "../api/http";
import { TOPIC_SESSIONS, type ConnectionState, type SocketClient } from "../api/socket";
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
import { createCatalog, type RepoRegistry } from "./catalog";
import { mergeAllLimits, mergeLimits } from "./limits";
import { createSessionStore, type SessionStore } from "./sessionStore";
import { upsertSession } from "./sessionsList";

export type HealthState =
  | { status: "loading" }
  | { status: "ready"; health: Health }
  | { status: "unreachable" };

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

export function createAppState(services: AppServices): AppState {
  const { api, socket } = services;
  const device = services.device ?? NO_DEVICE;

  const [connection, setConnection] = createSignal<ConnectionState>(socket.state());
  const [health, setHealth] = createSignal<HealthState>({ status: "loading" });
  const [sessions, setSessions] = createSignal<readonly Session[]>([]);
  const [sessionsLoaded, setSessionsLoaded] = createSignal(false);
  const [sessionsError, setSessionsError] = createSignal<string>();
  const [settings, setSettings] = createSignal<SettingsState>({ status: "loading" });
  const [now, setNow] = createSignal(Date.now());
  const [auth, setAuth] = createSignal<AuthState>({ status: "checking" });
  const [unsavedWork, setUnsavedWork] = createSignal<string>();
  const [online, setOnline] = createSignal(typeof navigator === "undefined" || navigator.onLine);
  const [updateReady, setUpdateReady] = createSignal(false);
  const navigateListeners = new Set<(path: string) => void>();
  // Signals apply later; decisions made in the same turn read this.
  let authNow: AuthState = { status: "checking" };
  function changeAuth(next: AuthState): void {
    authNow = next;
    setAuth(next);
  }

  let list: readonly Session[] = [];
  let listComplete = false;
  const publishSessions = coalesce(() => {
    setSessions(list);
    setSessionsLoaded(listComplete);
  }, services.schedule);

  function upsert(session: Session): void {
    list = upsertSession(list, session);
    publishSessions();
  }

  const catalog = createCatalog(api);

  const [limits, setLimits] = createSignal<readonly RunnerLimits[]>([]);
  let limitsNow: readonly RunnerLimits[] = [];
  function takeLimits(next: readonly RunnerLimits[]): void {
    if (next === limitsNow) return;
    limitsNow = next;
    setLimits(next);
  }
  let limitsInFlight = false;
  function loadLimits(): void {
    if (limitsInFlight) return;
    limitsInFlight = true;
    api.getUsageLimits().then(
      (loaded) => {
        limitsInFlight = false;
        takeLimits(mergeAllLimits(limitsNow, loaded));
      },
      () => {
        // Limits are a hint wherever they show: without them the screens simply say nothing.
        limitsInFlight = false;
      },
    );
  }

  let settingsKnown = false;
  let settingsWanted = false;
  let settingsInFlight = false;
  function loadSettings(): void {
    if (settingsInFlight) return;
    settingsInFlight = true;
    api.getSettings().then(
      (loaded) => {
        settingsInFlight = false;
        settingsKnown = true;
        setSettings({ status: "ready", settings: loaded });
      },
      (error: unknown) => {
        settingsInFlight = false;
        // Settings that were read once stay on screen through an outage.
        if (!settingsKnown) setSettings({ status: "error", message: errorMessage(error) });
      },
    );
  }

  const [workflows, setWorkflows] = createSignal<WorkflowCatalog>({ status: "loading" });
  let workflowsKnown = false;
  let workflowsWanted = false;
  let workflowsInFlight = false;
  function loadWorkflows(): void {
    if (workflowsInFlight) return;
    workflowsInFlight = true;
    api.listWorkflows().then(
      (loaded) => {
        workflowsInFlight = false;
        workflowsKnown = true;
        setWorkflows({ status: "ready", workflows: loaded });
      },
      (error: unknown) => {
        workflowsInFlight = false;
        // A list that was read once stays usable through an outage.
        if (!workflowsKnown) setWorkflows({ status: "error", message: errorMessage(error) });
      },
    );
  }

  let healthKnown = false;
  let healthInFlight = false;
  function loadHealth(): void {
    if (healthInFlight) return;
    healthInFlight = true;
    api.health().then(
      (loaded) => {
        healthInFlight = false;
        healthKnown = true;
        setHealth({ status: "ready", health: loaded });
      },
      () => {
        healthInFlight = false;
        // A health that was read once stays on screen; the connection indicator covers outages.
        if (!healthKnown) setHealth({ status: "unreachable" });
      },
    );
  }

  const knownHealth = (): Health | undefined => {
    const state = health();
    return state.status === "ready" ? state.health : undefined;
  };

  /** Subscribes and listens; returns the function that undoes it. Only once `/me` allowed it. */
  function connect(): () => void {
    catalog.setActive(true);
    let opens = 0;
    const offState = socket.onState((state) => {
      setConnection(state);
      if (state !== "open") return;
      opens += 1;
      // After a reconnect the server may be another build with other runners.
      if (opens > 1 || !healthKnown) loadHealth();
      catalog.reload({ missingOnly: opens === 1 });
      loadLimits();
      if (settingsWanted && !settingsKnown) loadSettings();
      // After a reconnect the home repo's files may have changed.
      if (workflowsWanted && (opens > 1 || !workflowsKnown)) loadWorkflows();
    });
    // A refused upgrade looks like any failed connection: `/me` tells, and its 401 ends the session.
    let probing = false;
    const offRefused = socket.onRefused(() => {
      if (probing) return;
      probing = true;
      const done = (): void => {
        probing = false;
      };
      api.me().then(done, done);
    });
    const unsubscribe = socket.subscribe(TOPIC_SESSIONS, (msg) => {
      if (msg.op === "session_upserted") {
        upsert(msg.session);
      } else if (msg.op === "subscribed") {
        listComplete = true;
        setSessionsError(undefined);
        publishSessions();
      } else if (msg.op === "error") {
        setSessionsError(msg.message);
      }
    });
    setConnection(socket.state());
    loadHealth();

    const clock = setInterval(() => setNow(Date.now()), CLOCK_TICK_MS);
    // Phones drop the socket in the background; do not sit out the backoff on return.
    const wake = (): void => {
      if (document.visibilityState === "visible") socket.reconnectNow();
    };
    document.addEventListener("visibilitychange", wake);
    window.addEventListener("online", wake);

    return () => {
      catalog.setActive(false);
      offState();
      offRefused();
      unsubscribe();
      clearInterval(clock);
      document.removeEventListener("visibilitychange", wake);
      window.removeEventListener("online", wake);
    };
  }

  let running = false;
  let disconnect: (() => void) | undefined;
  let meInFlight = false;
  let meAttempt = 0;
  let meTimer: ReturnType<typeof setTimeout> | undefined;

  function allowed(next: AuthState): void {
    meAttempt = 0;
    changeAuth(next);
    disconnect ??= connect();
    if (next.status === "signed_in") reportRenewedSubscription();
  }

  /**
   * The worker renewed this browser's push subscription while no page could
   * tell the server (see `sw/worker.ts`): tell it now. Push exists in public mode only.
   */
  function reportRenewedSubscription(): void {
    if (device.pushSupport() !== "supported") return;
    void (async () => {
      try {
        if (!(await device.takeRenewal())) return;
        const current = await device.subscription();
        if (current) await api.createPushSubscription(current.request);
      } catch {
        // Settings offers "Notify this device" again if this did not reach the server.
      }
    })();
  }

  function onWorkerMessage(message: WorkerMessage): void {
    if (message.type === "navigate") {
      for (const listener of navigateListeners) listener(message.path);
    } else if (message.type === "refresh") {
      // The session is live over the socket; after a phone's sleep the socket may not be.
      socket.reconnectNow();
    } else if (message.type === "subscription-changed" && authNow.status === "signed_in") {
      reportRenewedSubscription();
    }
  }

  function checkMe(): void {
    if (meInFlight || !running) return;
    clearTimeout(meTimer);
    meTimer = undefined;
    meInFlight = true;
    api.me().then(
      (me) => {
        meInFlight = false;
        if (!running || authNow.status === "signed_out") return;
        allowed(
          me.mode === "local"
            ? { status: "local" }
            : { status: "signed_in", identity: me.identity ?? "" },
        );
      },
      (error: unknown) => {
        meInFlight = false;
        if (!running) return;
        // A 401 went to `endSession` through the http layer already.
        if (error instanceof HttpError && error.status === 401) return;
        // A server from before sign-in existed knows no `/me`, and no sign-in either.
        if (error instanceof HttpError && error.status === 404 && error.fromServer) {
          allowed({ status: "local" });
          return;
        }
        changeAuth({ status: "unreachable" });
        // The "offline" event may have fired before this page listened.
        setOnline(navigator.onLine);
        const delay = Math.min(ME_RETRY_MAX_MS, ME_RETRY_BASE_MS * 2 ** meAttempt);
        meAttempt += 1;
        meTimer = setTimeout(checkMe, delay);
      },
    );
  }

  /** The one reaction to a 401 anywhere (see `Api.onUnauthorized`): stop, forget, ask to sign in. */
  function endSession(ended: boolean): void {
    if (authNow.status === "signed_out") return;
    changeAuth({ status: "signed_out", ended });
    clearTimeout(meTimer);
    disconnect?.();
    disconnect = undefined;
    // For good: signing in again is a full navigation, which starts a new app.
    socket.close();
    list = [];
    listComplete = false;
    publishSessions();
    setSessionsError(undefined);
    catalog.forget();
    settingsKnown = settingsWanted = false;
    setSettings({ status: "loading" });
    workflowsKnown = workflowsWanted = false;
    setWorkflows({ status: "loading" });
    healthKnown = false;
    setHealth({ status: "loading" });
    takeLimits([]);
  }

  const identity = (): string | undefined => {
    const state = auth();
    return state.status === "signed_in" ? state.identity : undefined;
  };

  return {
    auth,
    identity,
    signIn() {
      const url = signInUrl(window.location);
      if (services.leave) services.leave(url);
      else window.location.assign(url);
    },
    async signOut() {
      try {
        await api.logout();
      } catch (error) {
        // Already signed out on the server: the screen only has to catch up.
        if (!(error instanceof HttpError && error.status === 401)) throw error;
      }
      endSession(false);
    },
    unsavedWork,
    setUnsavedWork: (description) => setUnsavedWork(description),
    listAuthSessions: () => api.listAuthSessions(),
    async revokeAuthSession(id) {
      try {
        await api.revokeAuthSession(id);
      } catch (error) {
        // api.rs: an unknown id is a 404, which here means it ended some other way already.
        if (!(error instanceof HttpError && error.status === 404)) throw error;
      }
    },
    listDeviceTokens: () => api.listDeviceTokens(),
    createDeviceToken: (name) => api.createDeviceToken({ name }),
    revokeDeviceToken: (id) => api.revokeDeviceToken(id),
    connection,
    health,
    githubProblem: () => knownHealth()?.github_problem ?? undefined,
    sessions,
    sessionsLoaded,
    sessionsError,
    repos: catalog.repos,
    repoSets: catalog.sets,
    loadRepos: () => catalog.load(),
    registerRepo: (request) => catalog.register(request),
    removeRepo: (id) => catalog.remove(id),
    cloneRepo: (id) => catalog.clone(id),
    rescanRepos: () => catalog.rescan(),
    workflows,
    loadWorkflows() {
      workflowsWanted = true;
      loadWorkflows();
    },
    settings,
    loadSettings() {
      settingsWanted = true;
      if (!settingsKnown) setSettings({ status: "loading" });
      loadSettings();
    },
    async saveSettings(next) {
      const stored = await api.putSettings(next);
      settingsKnown = true;
      setSettings({ status: "ready", settings: stored });
      // api.rs: a change re-reads the home repo and rescans, so the repo list is another one now.
      catalog.invalidate();
      // The workflows live in the home repo, which may be another one now.
      if (workflowsWanted) loadWorkflows();
      return stored;
    },
    now,
    limits,
    loadLimits,
    listIssues: (repoId, query) => api.listIssues(repoId, query),
    getIssue: (repoId, number) => api.getIssue(repoId, number),
    usage: (query) => api.getUsage(query),
    listAutomations: () => api.listAutomations(),
    getAutomation: (id) => api.getAutomation(id),
    saveAutomation: (id, request) =>
      id === undefined ? api.createAutomation(request) : api.putAutomation(id, request),
    deleteAutomation: (id) => api.deleteAutomation(id),
    previewCron: (expr) => api.previewCron(expr),
    runAutomation: (id, number) => api.runAutomation(id, number === undefined ? {} : { number }),
    automationRuns: (id) => api.listAutomationRuns(id),
    async createSession(request) {
      const { session } = await api.createSession(request);
      upsert(session);
      return session;
    },
    openSession: (id) =>
      createSessionStore(
        {
          api,
          socket,
          listed: (wanted) => sessions().find((session) => session.id === wanted),
          sessions,
          schedule: services.schedule,
          openTerminal: services.openTerminal,
          health: knownHealth,
          checkHealth: loadHealth,
          now,
          onLimits: (report) => takeLimits(mergeLimits(limitsNow, report)),
        },
        id,
      ),
    retry() {
      if (authNow.status === "unreachable" || authNow.status === "checking") {
        meAttempt = 0;
        checkMe();
        return;
      }
      if (disconnect === undefined) return;
      if (!healthKnown) setHealth({ status: "loading" });
      loadHealth();
      catalog.reload({ missingOnly: true });
      if (settingsWanted && !settingsKnown) loadSettings();
      if (workflowsWanted && !workflowsKnown) loadWorkflows();
      if (sessionsError() !== undefined) socket.resync(TOPIC_SESSIONS, 0);
      socket.reconnectNow();
    },
    online,
    updateReady,
    applyUpdate: () => services.worker?.applyUpdate(),
    onNavigateRequest(listener) {
      navigateListeners.add(listener);
      return () => navigateListeners.delete(listener);
    },
    device,
    pushKey: async () => (await api.getPushKey()).public_key,
    listPushSubscriptions: () => api.listPushSubscriptions(),
    registerPushSubscription: (request) => api.createPushSubscription(request),
    removePushSubscription: (id) => api.deletePushSubscription(id),
    pushPreferences: () => api.getPushPreferences(),
    savePushPreferences: (preferences) => api.putPushPreferences(preferences),
    sendTestPush: () => api.sendTestPush(),
    start() {
      running = true;
      // A 401 before anyone was signed in is a sign-in to do, not one that ended.
      const offUnauthorized = api.onUnauthorized(() =>
        endSession(authNow.status !== "checking" && authNow.status !== "unreachable"),
      );
      // Back online: ask for the server now rather than at the end of the backoff.
      const wentOnline = (): void => {
        setOnline(true);
        if (authNow.status === "unreachable") {
          meAttempt = 0;
          checkMe();
        }
      };
      const wentOffline = (): void => {
        setOnline(false);
      };
      window.addEventListener("online", wentOnline);
      window.addEventListener("offline", wentOffline);
      const offReady = services.worker?.onUpdateReady(() => setUpdateReady(true));
      const offMessages = services.worker?.onMessage(onWorkerMessage);
      checkMe();
      return () => {
        running = false;
        window.removeEventListener("online", wentOnline);
        window.removeEventListener("offline", wentOffline);
        offReady?.();
        offMessages?.();
        offUnauthorized();
        clearTimeout(meTimer);
        disconnect?.();
        disconnect = undefined;
      };
    },
  };
}
