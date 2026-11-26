import { createContext, createSignal, type Accessor } from "solid-js";
import type { Api } from "../api/http";
import { TOPIC_SESSIONS, type ConnectionState, type SocketClient } from "../api/socket";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { Health } from "../gen/Health";
import type { Session } from "../gen/Session";
import { coalesce, type Schedule } from "../lib/schedule";
import { createSessionStore, type SessionStore } from "./sessionStore";
import { upsertSession } from "./sessionsList";

export type HealthState =
  | { status: "loading" }
  | { status: "ready"; health: Health }
  | { status: "unreachable" };

export type AppServices = {
  api: Api;
  socket: SocketClient;
  schedule?: Schedule;
};

/** Everything components read and do. They get it from `AppContext`, never from `src/api`. */
export type AppState = {
  connection: Accessor<ConnectionState>;
  health: Accessor<HealthState>;
  /** Newest first. */
  sessions: Accessor<readonly Session[]>;
  /** True once the server has sent every existing session. */
  sessionsLoaded: Accessor<boolean>;
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
        },
        id,
      ),
    retry() {
      if (!healthKnown) setHealth({ status: "loading" });
      loadHealth();
      socket.reconnectNow();
    },
    start() {
      let opens = 0;
      const offState = socket.onState((state) => {
        setConnection(state);
        if (state !== "open") return;
        opens += 1;
        // After a reconnect the server may be another build with other runners.
        if (opens > 1 || !healthKnown) loadHealth();
      });
      const unsubscribe = socket.subscribe(TOPIC_SESSIONS, (msg) => {
        if (msg.op === "session_upserted") {
          upsert(msg.session);
        } else if (msg.op === "subscribed") {
          listComplete = true;
          publishSessions();
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
        offState();
        unsubscribe();
        clearInterval(clock);
        document.removeEventListener("visibilitychange", wake);
        window.removeEventListener("online", wake);
      };
    },
  };
}
