import type { ClientMsg } from "../gen/ClientMsg";
import type { ServerMsg } from "../gen/ServerMsg";
import { parseOpFrame, socketUrl } from "./frames";

// Topic names mirror `TOPIC_SESSIONS` and `session_topic` in calin-proto/src/lib.rs.
export const TOPIC_SESSIONS = "sessions";
export const sessionTopic = (sessionId: string): string => `session:${sessionId}`;

export type ConnectionState = "connecting" | "open" | "reconnecting";

/** The server refused to `subscribe` this topic; nothing arrives on it until it is asked again. */
export type TopicRefusal = Extract<ServerMsg, { op: "error" }> & { topic: string };

/** Frames that belong to a topic; `pong` and an `error` that names no topic never reach a subscriber. */
export type TopicMsg =
  | Extract<ServerMsg, { op: "subscribed" | "event" | "session_upserted" }>
  | TopicRefusal;
export type TopicHandler = (msg: TopicMsg) => void;

/** The last `seq` a subscriber applied, or `undefined` to replay from the start. */
export type Cursor = () => number | undefined;

/** The part of the browser `WebSocket` the client uses, so tests can fake it. */
export interface WebSocketLike {
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent) => void) | null;
  onclose: ((event: CloseEvent) => void) | null;
  onerror: ((event: Event) => void) | null;
  /** The browser's own state: 1 is open. It moves on before `onclose` runs. */
  readonly readyState: number;
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
  /** Called with the text of a server `error` frame that names no topic. */
  onServerError?: (message: string) => void;
}

export interface SocketClient {
  /**
   * Starts receiving `topic`. The first subscriber of a topic sends
   * `subscribe`; the returned function unsubscribes, and the last one out
   * sends `unsubscribe`.
   */
  subscribe(topic: string, handler: TopicHandler, cursor?: Cursor): () => void;
  /** Asks the server to replay `topic` again from `afterSeq`, after a gap or a refusal. */
  resync(topic: string, afterSeq: number): void;
  state(): ConnectionState;
  onState(listener: (state: ConnectionState) => void): () => void;
  /**
   * Calls `listener` when a connection attempt ends before it opened. A browser
   * is not told why (a refused sign-in and a network failure look alike), so
   * the listener finds out over HTTP; the socket keeps retrying meanwhile.
   */
  onRefused(listener: () => void): () => void;
  /** Skips the backoff delay, for example when the page becomes visible again. */
  reconnectNow(): void;
  close(): void;
}

/** `WebSocket.OPEN`, which a test's stand-in does not define. */
const OPEN = 1;

type Subscriber = { handler: TopicHandler; cursor: Cursor | undefined };

export function defaultSocketUrl(location: Pick<Location, "protocol" | "host">): string {
  return socketUrl(location, "/ws");
}

function parseFrame(data: unknown): ServerMsg | undefined {
  return typeof data === "string" ? parseOpFrame<ServerMsg>(data) : undefined;
}

function subscribeMsg(topic: string, afterSeq: number | undefined): ClientMsg {
  return afterSeq !== undefined && afterSeq > 0
    ? { op: "subscribe", topic, after_seq: afterSeq }
    : { op: "subscribe", topic };
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

/** The wait before attempt number `attempt` (from 0), with `jitter` in [0, 1). */
function backoffDelay(attempt: number, baseMs: number, maxMs: number, jitter: number): number {
  const ceiling = Math.min(maxMs, baseMs * 2 ** attempt);
  // Half fixed, half random, so clients do not reconnect in step after a restart.
  return ceiling * (0.5 + jitter * 0.5);
}

export function createSocket(options: SocketOptions = {}): SocketClient {
  return new EventSocket(options);
}

class EventSocket implements SocketClient {
  private readonly pingIntervalMs: number;
  private readonly pongTimeoutMs: number;
  private readonly backoffBaseMs: number;
  private readonly backoffMaxMs: number;
  private readonly random: () => number;
  private readonly options: SocketOptions;

  private readonly topics = new Map<string, Set<Subscriber>>();
  // Topics whose last `subscribe` was refused: the server holds nothing to leave.
  private readonly refused = new Set<string>();
  private readonly stateListeners = new Set<(state: ConnectionState) => void>();
  private readonly refusedListeners = new Set<() => void>();

  private ws: WebSocketLike | null = null;
  private current: ConnectionState = "connecting";
  private started = false;
  private closed = false;
  private attempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  private pingTimer: ReturnType<typeof setInterval> | undefined;
  private pongTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(options: SocketOptions) {
    this.options = options;
    this.pingIntervalMs = options.pingIntervalMs ?? 20_000;
    this.pongTimeoutMs = options.pongTimeoutMs ?? 10_000;
    this.backoffBaseMs = options.backoffBaseMs ?? 500;
    this.backoffMaxMs = options.backoffMaxMs ?? 15_000;
    this.random = options.random ?? Math.random;
  }

  subscribe(topic: string, handler: TopicHandler, cursor?: Cursor): () => void {
    const subscriber: Subscriber = { handler, cursor };
    const subscribers = this.topics.get(topic);
    if (subscribers) {
      subscribers.add(subscriber);
    } else {
      this.topics.set(topic, new Set([subscriber]));
      this.send(subscribeMsg(topic, cursor?.()));
    }
    this.start();
    return () => this.leave(topic, subscriber);
  }

  resync(topic: string, afterSeq: number): void {
    if (!this.topics.has(topic)) return;
    // Leaving first makes the server start one clean replay instead of two feeds.
    if (!this.refused.has(topic)) this.send({ op: "unsubscribe", topic });
    this.send(subscribeMsg(topic, afterSeq));
  }

  state(): ConnectionState {
    return this.current;
  }

  onState(listener: (state: ConnectionState) => void): () => void {
    this.stateListeners.add(listener);
    return () => this.stateListeners.delete(listener);
  }

  onRefused(listener: () => void): () => void {
    this.refusedListeners.add(listener);
    return () => this.refusedListeners.delete(listener);
  }

  reconnectNow(): void {
    if (this.reconnectTimer === undefined) return;
    clearTimeout(this.reconnectTimer);
    this.open();
  }

  close(): void {
    this.closed = true;
    clearTimeout(this.reconnectTimer);
    this.detach();
  }

  private leave(topic: string, subscriber: Subscriber): void {
    const current = this.topics.get(topic);
    if (!current?.delete(subscriber) || current.size > 0) return;
    this.topics.delete(topic);
    if (!this.refused.delete(topic)) this.send({ op: "unsubscribe", topic });
  }

  private setState(next: ConnectionState): void {
    if (this.current === next) return;
    this.current = next;
    for (const listener of this.stateListeners) listener(next);
  }

  private send(msg: ClientMsg): void {
    // The server may have closed it already (signing out does): a send then only
    // makes the browser log an error.
    if (this.current === "open" && this.ws?.readyState === OPEN) this.ws.send(JSON.stringify(msg));
  }

  private stopHeartbeat(): void {
    clearInterval(this.pingTimer);
    clearTimeout(this.pongTimer);
    this.pingTimer = undefined;
    this.pongTimer = undefined;
  }

  private startHeartbeat(): void {
    this.pingTimer = setInterval(() => this.ping(), this.pingIntervalMs);
  }

  private ping(): void {
    this.send({ op: "ping" });
    this.pongTimer ??= setTimeout(() => this.drop(), this.pongTimeoutMs);
  }

  private detach(): void {
    this.stopHeartbeat();
    if (!this.ws) return;
    this.ws.onopen = this.ws.onmessage = this.ws.onclose = this.ws.onerror = null;
    this.ws.close();
    this.ws = null;
  }

  /** The connection is gone or presumed dead: tear it down and try again later. */
  private drop(): void {
    this.detach();
    if (this.closed) return;
    this.setState("reconnecting");
    const delay = backoffDelay(this.attempt, this.backoffBaseMs, this.backoffMaxMs, this.random());
    this.attempt += 1;
    this.reconnectTimer = setTimeout(() => this.open(), delay);
  }

  private deliver(topic: string, msg: TopicMsg): void {
    for (const { handler } of this.topics.get(topic) ?? []) handler(msg);
  }

  private dispatch(msg: ServerMsg): void {
    switch (msg.op) {
      case "subscribed":
        this.refused.delete(msg.topic);
        this.deliver(msg.topic, msg);
        break;
      case "event":
        this.deliver(msg.topic, msg);
        break;
      case "session_upserted":
        this.deliver(TOPIC_SESSIONS, msg);
        break;
      case "error":
        this.serverError(msg);
        break;
      case "pong":
      default:
        // `pong` only proves liveness; an op this build does not know is skipped.
        break;
    }
  }

  private serverError(msg: Extract<ServerMsg, { op: "error" }>): void {
    if (msg.topic === undefined) {
      this.options.onServerError?.(msg.message);
      return;
    }
    // A topic everyone has left since asking: the refusal concerns nobody anymore.
    if (!this.topics.has(msg.topic)) return;
    this.refused.add(msg.topic);
    this.deliver(msg.topic, { op: "error", message: msg.message, topic: msg.topic });
  }

  private open(): void {
    this.reconnectTimer = undefined;
    if (this.closed) return;
    const WebSocketImpl = this.options.WebSocket ?? WebSocket;
    const socket = new WebSocketImpl(this.options.url ?? defaultSocketUrl(location));
    this.ws = socket;
    socket.onopen = () => this.opened();
    socket.onmessage = (event) => this.received(event);
    socket.onclose = () => this.lost();
    socket.onerror = () => this.lost();
  }

  private opened(): void {
    this.attempt = 0;
    // A new connection, possibly to a server that now knows the topic.
    this.refused.clear();
    this.setState("open");
    for (const [topic, subscribers] of this.topics) {
      this.send(subscribeMsg(topic, cursorOf(subscribers)));
    }
    this.startHeartbeat();
  }

  private received(event: MessageEvent): void {
    // Any frame shows the connection is alive, not only the `pong` itself:
    // a long replay on a slow link must not look like a dead socket.
    clearTimeout(this.pongTimer);
    this.pongTimer = undefined;
    const msg = parseFrame(event.data);
    if (msg) this.dispatch(msg);
  }

  private lost(): void {
    // Only an attempt that opened ever sets "open".
    const neverOpened = this.current !== "open";
    this.drop();
    if (neverOpened) for (const listener of this.refusedListeners) listener();
  }

  private start(): void {
    if (this.started || this.closed) return;
    this.started = true;
    this.open();
  }
}
