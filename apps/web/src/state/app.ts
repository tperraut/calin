import { createContext, createSignal, type Accessor } from "solid-js";
import type { Api } from "../api/http";
import { TOPIC_SESSIONS, type ConnectionState, type SocketClient } from "../api/socket";
import type { OpenTerminal } from "../api/terminal";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { Health } from "../gen/Health";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoSet } from "../gen/RepoSet";
import type { Session } from "../gen/Session";
import type { Settings } from "../gen/Settings";
import { errorMessage } from "../lib/errors";
import { coalesce, type Schedule } from "../lib/schedule";
import { createCatalog, type RepoRegistry } from "./catalog";
import { createSessionStore, type SessionStore } from "./sessionStore";
import { upsertSession } from "./sessionsList";

export type HealthState =
  | { status: "loading" }
  | { status: "ready"; health: Health }
  | { status: "unreachable" };

export type { RepoRegistry } from "./catalog";

/** The machine's settings, as the server last answered. */
export type SettingsState =
  | { status: "loading" }
  | { status: "ready"; settings: Settings }
  | { status: "error"; message: string };

export type AppServices = {
  api: Api;
  socket: SocketClient;
  schedule?: Schedule;
  /** Opens a terminal socket; see `createTerminalOpener`. */
  openTerminal?: OpenTerminal;
};

/** Everything components read and do. They get it from `AppContext`, never from `src/api`. */
export type AppState = {
  connection: Accessor<ConnectionState>;
  health: Accessor<HealthState>;
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
  /** Stays `loading` until a screen calls `loadSettings`. */
  settings: Accessor<SettingsState>;
  loadSettings(): void;
  /** Replaces the settings. Rejects with the server's reason; the repos follow a change. */
  saveSettings(settings: Settings): Promise<Settings>;
  /** A clock that ticks slowly, for relative times. */
  now: Accessor<number>;
  createSession(request: CreateSessionRequest): Promise<Session>;
  openSession(id: string): SessionStore;
  /** Asks again for the server, without waiting for the backoff. */
  retry(): void;
  /** Connects and loads; returns the function that undoes both. */
  start(): () => void;
};

export const AppContext = createContext<AppState>();

const CLOCK_TICK_MS = 30_000;

export function createAppState(services: AppServices): AppState {
  const { api, socket } = services;

  const [connection, setConnection] = createSignal<ConnectionState>(socket.state());
  const [health, setHealth] = createSignal<HealthState>({ status: "loading" });
  const [sessions, setSessions] = createSignal<readonly Session[]>([]);
  const [sessionsLoaded, setSessionsLoaded] = createSignal(false);
  const [sessionsError, setSessionsError] = createSignal<string>();
  const [settings, setSettings] = createSignal<SettingsState>({ status: "loading" });
  const [now, setNow] = createSignal(Date.now());

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

  return {
    connection,
    health,
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
      return stored;
    },
    now,
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
          schedule: services.schedule,
          openTerminal: services.openTerminal,
        },
        id,
      ),
    retry() {
      if (!healthKnown) setHealth({ status: "loading" });
      loadHealth();
      catalog.reload({ missingOnly: true });
      if (settingsWanted && !settingsKnown) loadSettings();
      if (sessionsError() !== undefined) socket.resync(TOPIC_SESSIONS, 0);
      socket.reconnectNow();
    },
    start() {
      catalog.setActive(true);
      let opens = 0;
      const offState = socket.onState((state) => {
        setConnection(state);
        if (state !== "open") return;
        opens += 1;
        // After a reconnect the server may be another build with other runners.
        if (opens > 1 || !healthKnown) loadHealth();
        catalog.reload({ missingOnly: opens === 1 });
        if (settingsWanted && !settingsKnown) loadSettings();
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
        unsubscribe();
        clearInterval(clock);
        document.removeEventListener("visibilitychange", wake);
        window.removeEventListener("online", wake);
      };
    },
  };
}
