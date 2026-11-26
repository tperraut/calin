import { createSignal, type Accessor } from "solid-js";
import { HttpError, type Api } from "../api/http";
import { sessionTopic, type SocketClient, type TopicHandler } from "../api/socket";
import type { Session } from "../gen/Session";
import type { SessionDetail } from "../gen/SessionDetail";
import type { SessionStatus } from "../gen/SessionStatus";
import { errorMessage } from "../lib/errors";
import { coalesce, type Schedule } from "../lib/schedule";
import { applyEvent, emptySessionView, type SessionView } from "./sessionReducer";

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
  /** True once the replay has caught up with the head of the log. */
  synced: Accessor<boolean>;
  /** True when the server keeps leaving the same gap; only a reload can help. */
  stalled: Accessor<boolean>;
  send(text: string): Promise<void>;
  stop(): Promise<void>;
  reload(): void;
  /** Subscribes and loads; returns the function that undoes both. */
  start(): () => void;
};

export type SessionStoreDeps = {
  api: Api;
  socket: SocketClient;
  /** The session as the `sessions` topic last reported it, if it did. */
  listed?: (id: string) => Session | undefined;
  schedule?: Schedule;
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

  // The fold runs on plain values as frames arrive; signals follow once per frame.
  let current = emptySessionView;
  let caughtUp = false;
  // While a replay is on its way, frames still in flight from before it are expected.
  let replaying = true;
  let fruitlessResyncs = 0;
  let active = false;
  let failed = false;
  let stopListening = (): void => {};

  const publish = coalesce(() => {
    setView(current);
    setSynced(caughtUp);
    setStalled(fruitlessResyncs >= MAX_FRUITLESS_RESYNCS);
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
    } else if (msg.op === "subscribed") {
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
    synced,
    stalled,
    send: (text) => api.sendMessage(id, { text }),
    stop: () => api.stopSession(id),
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
  };
}
