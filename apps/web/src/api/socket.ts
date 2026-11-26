import type { ClientMsg } from "../gen/ClientMsg";
import type { ServerMsg } from "../gen/ServerMsg";
import { API_BASE } from "./http";

// Topic names mirror `TOPIC_SESSIONS` and `session_topic` in calin-proto/src/lib.rs.
export const TOPIC_SESSIONS = "sessions";
export const sessionTopic = (sessionId: string): string => `session:${sessionId}`;

export type ConnectionState = "connecting" | "open" | "reconnecting";

/** Frames that belong to a topic; `pong` and `error` never reach a subscriber. */
export type TopicMsg = Extract<ServerMsg, { op: "subscribed" | "event" | "session_upserted" }>;
export type TopicHandler = (msg: TopicMsg) => void;

/** The last `seq` a subscriber applied, or `undefined` to replay from the start. */
export type Cursor = () => number | undefined;

/** The part of the browser `WebSocket` the client uses, so tests can fake it. */
export interface WebSocketLike {
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent) => void) | null;
  onclose: ((event: CloseEvent) => void) | null;
  onerror: ((event: Event) => void) | null;
  send(data: string): void;
  close(): void;
}

export interface SocketOptions {
  /** Defaults to `/api/v1/ws` on the page's origin. */
  url?: string;
  WebSocket?: new (url: string) => WebSocketLike;
  pingIntervalMs?: number;
  pongTimeoutMs?: number;
  backoffBaseMs?: number;
  backoffMaxMs?: number;
  /** Source of jitter, in [0, 1). */
  random?: () => number;
  /** Called with the text of a server `error` frame. */
  onServerError?: (message: string) => void;
}

export interface SocketClient {
  /**
   * Starts receiving `topic`. The first subscriber of a topic sends
   * `subscribe`; the returned function unsubscribes, and the last one out
   * sends `unsubscribe`.
   */
  subscribe(topic: string, handler: TopicHandler, cursor?: Cursor): () => void;
  /** Asks the server to replay `topic` again from `afterSeq`, after a gap. */
  resync(topic: string, afterSeq: number): void;
  state(): ConnectionState;
  onState(listener: (state: ConnectionState) => void): () => void;
  /** Skips the backoff delay, for example when the page becomes visible again. */
  reconnectNow(): void;
  close(): void;
}

type Subscriber = { handler: TopicHandler; cursor: Cursor | undefined };

export function defaultSocketUrl(location: Pick<Location, "protocol" | "host">): string {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}${API_BASE}/ws`;
}

function parseFrame(data: unknown): ServerMsg | undefined {
  if (typeof data !== "string") return undefined;
  let value: unknown;
  try {
    value = JSON.parse(data);
  } catch {
    return undefined;
  }
  if (typeof value !== "object" || value === null || !("op" in value)) return undefined;
  if (typeof value.op !== "string") return undefined;
  // Shapes beyond `op` are the contract's business; unknown ops are dropped by the caller.
  return value as ServerMsg;
}

export function createSocket(options: SocketOptions = {}): SocketClient {
  const pingIntervalMs = options.pingIntervalMs ?? 20_000;
  const pongTimeoutMs = options.pongTimeoutMs ?? 10_000;
  const backoffBaseMs = options.backoffBaseMs ?? 500;
  const backoffMaxMs = options.backoffMaxMs ?? 15_000;
  const random = options.random ?? Math.random;

  const topics = new Map<string, Set<Subscriber>>();
  const stateListeners = new Set<(state: ConnectionState) => void>();

  let ws: WebSocketLike | null = null;
  let state: ConnectionState = "connecting";
  let started = false;
  let closed = false;
  let attempt = 0;
  let reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  let pingTimer: ReturnType<typeof setInterval> | undefined;
  let pongTimer: ReturnType<typeof setTimeout> | undefined;

  function setState(next: ConnectionState): void {
    if (state === next) return;
    state = next;
    for (const listener of stateListeners) listener(next);
  }

  function send(msg: ClientMsg): void {
    if (state === "open" && ws) ws.send(JSON.stringify(msg));
  }

  function sendSubscribe(topic: string, afterSeq: number | undefined): void {
    send(
      afterSeq !== undefined && afterSeq > 0
        ? { op: "subscribe", topic, after_seq: afterSeq }
        : { op: "subscribe", topic },
    );
  }

  /** The oldest position any subscriber of `topic` is at; later ones skip duplicates. */
  function cursorOf(subscribers: Set<Subscriber>): number | undefined {
    let lowest: number | undefined;
    for (const { cursor } of subscribers) {
      const seq = cursor?.();
      if (seq === undefined) return undefined;
      lowest = lowest === undefined ? seq : Math.min(lowest, seq);
    }
    return lowest;
  }

  function stopHeartbeat(): void {
    clearInterval(pingTimer);
    clearTimeout(pongTimer);
    pingTimer = undefined;
    pongTimer = undefined;
  }

  function startHeartbeat(): void {
    pingTimer = setInterval(() => {
      send({ op: "ping" });
      pongTimer ??= setTimeout(drop, pongTimeoutMs);
    }, pingIntervalMs);
  }

  function detach(): void {
    stopHeartbeat();
    if (!ws) return;
    ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
    ws.close();
    ws = null;
  }

  /** The connection is gone or presumed dead: tear it down and try again later. */
  function drop(): void {
    detach();
    if (closed) return;
    setState("reconnecting");
    const ceiling = Math.min(backoffMaxMs, backoffBaseMs * 2 ** attempt);
    attempt += 1;
    // Half fixed, half random, so clients do not reconnect in step after a restart.
    reconnectTimer = setTimeout(open, ceiling * (0.5 + random() * 0.5));
  }

  function dispatch(msg: ServerMsg): void {
    switch (msg.op) {
      case "subscribed":
      case "event":
        for (const { handler } of topics.get(msg.topic) ?? []) handler(msg);
        break;
      case "session_upserted":
        for (const { handler } of topics.get(TOPIC_SESSIONS) ?? []) handler(msg);
        break;
      case "error":
        options.onServerError?.(msg.message);
        break;
      case "pong":
      default:
        // `pong` only proves liveness; an op this build does not know is skipped.
        break;
    }
  }

  function open(): void {
    reconnectTimer = undefined;
    if (closed) return;
    const WebSocketImpl = options.WebSocket ?? WebSocket;
    const socket = new WebSocketImpl(options.url ?? defaultSocketUrl(location));
    ws = socket;
    socket.onopen = () => {
      attempt = 0;
      setState("open");
      for (const [topic, subscribers] of topics) sendSubscribe(topic, cursorOf(subscribers));
      startHeartbeat();
    };
    socket.onmessage = (event) => {
      // Any frame shows the connection is alive, not only the `pong` itself:
      // a long replay on a slow link must not look like a dead socket.
      clearTimeout(pongTimer);
      pongTimer = undefined;
      const msg = parseFrame(event.data);
      if (msg) dispatch(msg);
    };
    socket.onclose = drop;
    socket.onerror = drop;
  }

  function start(): void {
    if (started || closed) return;
    started = true;
    open();
  }

  return {
    subscribe(topic, handler, cursor) {
      const subscriber: Subscriber = { handler, cursor };
      let subscribers = topics.get(topic);
      if (subscribers) {
        subscribers.add(subscriber);
      } else {
        subscribers = new Set([subscriber]);
        topics.set(topic, subscribers);
        sendSubscribe(topic, cursor?.());
      }
      start();
      return () => {
        const current = topics.get(topic);
        if (!current?.delete(subscriber) || current.size > 0) return;
        topics.delete(topic);
        send({ op: "unsubscribe", topic });
      };
    },
    resync(topic, afterSeq) {
      if (!topics.has(topic)) return;
      // Leaving first makes the server start one clean replay instead of two feeds.
      send({ op: "unsubscribe", topic });
      sendSubscribe(topic, afterSeq);
    },
    state: () => state,
    onState(listener) {
      stateListeners.add(listener);
      return () => stateListeners.delete(listener);
    },
    reconnectNow() {
      if (reconnectTimer === undefined) return;
      clearTimeout(reconnectTimer);
      open();
    },
    close() {
      closed = true;
      clearTimeout(reconnectTimer);
      detach();
    },
  };
}
