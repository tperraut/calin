import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { ClientMsg } from "../gen/ClientMsg";
import type { Event } from "../gen/Event";
import type { ServerMsg } from "../gen/ServerMsg";
import { makeSession } from "../test/fakes";
import {
  createSocket,
  defaultSocketUrl,
  type ConnectionState,
  type SocketClient,
  type SocketOptions,
  type TopicMsg,
  type WebSocketLike,
} from "./socket";

/** A WebSocket the test drives by hand. Every instance is kept in `sockets`. */
class FakeWebSocket implements WebSocketLike {
  static sockets: FakeWebSocket[] = [];

  onopen: ((event: Event_) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  onerror: ((event: Event_) => void) | null = null;
  readonly url: string;
  readonly sent: ClientMsg[] = [];
  closed = false;
  /** As a browser's: 0 connecting, 1 open, 2 closing, 3 closed. */
  readyState = 0;

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.sockets.push(this);
  }

  send(data: string): void {
    this.sent.push(JSON.parse(data) as ClientMsg);
  }

  close(): void {
    this.closed = true;
  }

  // What the server side of the test does.
  accept(): void {
    this.readyState = 1;
    this.onopen?.(new Event("open"));
  }

  push(msg: ServerMsg): void {
    this.pushRaw(JSON.stringify(msg));
  }

  pushRaw(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }

  hangUp(): void {
    this.readyState = 3;
    this.onclose?.({} as CloseEvent);
  }

  /** Frames sent since the last call, so each step asserts only its own. */
  take(): ClientMsg[] {
    return this.sent.splice(0);
  }
}

// The DOM `Event`, which the wire type of the same name shadows in this file.
type Event_ = globalThis.Event;

const sessionEvent = (seq: number): Event => ({
  id: `e${seq}`,
  session_id: "s1",
  seq,
  ts: seq,
  agent_id: null,
  parent_id: null,
  body: { type: "user_message", text: `message ${seq}` },
});

const latest = (): FakeWebSocket => {
  const socket = FakeWebSocket.sockets.at(-1);
  if (!socket) throw new Error("no socket was opened");
  return socket;
};

function connect(options: SocketOptions = {}): SocketClient {
  return createSocket({
    url: "ws://test/api/v1/ws",
    WebSocket: FakeWebSocket,
    pingIntervalMs: 1000,
    pongTimeoutMs: 400,
    backoffBaseMs: 100,
    backoffMaxMs: 800,
    // The top of the jitter range: the delay is exactly the backoff ceiling.
    random: () => 1,
    ...options,
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  FakeWebSocket.sockets = [];
});

afterEach(() => {
  vi.useRealTimers();
});

describe("socket client", () => {
  it("sends nothing on a socket the server is closing, before the close reaches the page", () => {
    const client = connect();
    const unsubscribe = client.subscribe("session:s1", () => {});
    latest().accept();
    latest().take();
    // The close frame is in: the browser says CLOSING, but onclose has not run yet.
    latest().readyState = 2;
    unsubscribe();
    vi.advanceTimersByTime(1000);
    expect(latest().take()).toEqual([]);
  });

  it("opens one connection, on first use, for any number of topics", () => {
    const client = connect();
    expect(FakeWebSocket.sockets).toHaveLength(0);

    client.subscribe("sessions", () => {});
    client.subscribe("session:s1", () => {});
    client.subscribe("session:s2", () => {});

    expect(FakeWebSocket.sockets).toHaveLength(1);
    expect(latest().url).toBe("ws://test/api/v1/ws");
  });

  it("derives the URL from the page location", () => {
    expect(defaultSocketUrl({ protocol: "http:", host: "localhost:5173" })).toBe(
      "ws://localhost:5173/api/v1/ws",
    );
    expect(defaultSocketUrl({ protocol: "https:", host: "calin.example" })).toBe(
      "wss://calin.example/api/v1/ws",
    );
  });

  it("subscribes topics requested before the socket opened, once it opens", () => {
    const client = connect();
    client.subscribe("sessions", () => {});
    expect(latest().sent).toEqual([]);

    latest().accept();
    expect(latest().take()).toEqual([{ op: "subscribe", topic: "sessions" }]);
  });

  it("counts references: one subscribe for the first, one unsubscribe after the last", () => {
    const client = connect();
    const first = client.subscribe("session:s1", () => {});
    latest().accept();
    expect(latest().take()).toEqual([{ op: "subscribe", topic: "session:s1" }]);

    const second = client.subscribe("session:s1", () => {});
    const third = client.subscribe("session:s1", () => {});
    expect(latest().take()).toEqual([]);

    first();
    second();
    expect(latest().take()).toEqual([]);

    third();
    expect(latest().take()).toEqual([{ op: "unsubscribe", topic: "session:s1" }]);

    // Calling a spent unsubscribe again must not send, nor disturb a newer subscriber.
    const fourth = client.subscribe("session:s1", () => {});
    expect(latest().take()).toEqual([{ op: "subscribe", topic: "session:s1" }]);
    third();
    first();
    expect(latest().take()).toEqual([]);
    fourth();
    expect(latest().take()).toEqual([{ op: "unsubscribe", topic: "session:s1" }]);
  });

  it("routes frames to the subscribers of their topic", () => {
    const client = connect();
    const sessions: TopicMsg[] = [];
    const one: TopicMsg[] = [];
    const alsoOne: TopicMsg[] = [];
    const two: TopicMsg[] = [];
    client.subscribe("sessions", (msg) => sessions.push(msg));
    client.subscribe("session:s1", (msg) => one.push(msg));
    client.subscribe("session:s1", (msg) => alsoOne.push(msg));
    client.subscribe("session:s2", (msg) => two.push(msg));
    latest().accept();

    const event: ServerMsg = { op: "event", topic: "session:s1", event: sessionEvent(1) };
    const subscribed: ServerMsg = { op: "subscribed", topic: "session:s2", head_seq: 0 };
    latest().push(event);
    latest().push(subscribed);
    latest().push({ op: "subscribed", topic: "sessions", head_seq: 0 });

    expect(one).toEqual([event]);
    expect(alsoOne).toEqual([event]);
    expect(two).toEqual([subscribed]);
    expect(sessions).toEqual([{ op: "subscribed", topic: "sessions", head_seq: 0 }]);
  });

  it("hands a session upsert to the subscribers of the session list only", () => {
    const client = connect();
    const sessions: TopicMsg[] = [];
    const one: TopicMsg[] = [];
    client.subscribe("sessions", (msg) => sessions.push(msg));
    client.subscribe("session:s1", (msg) => one.push(msg));
    latest().accept();

    const upsert: ServerMsg = { op: "session_upserted", session: makeSession("s1") };
    latest().push(upsert);

    expect(sessions).toEqual([upsert]);
    expect(one).toEqual([]);
  });

  it("skips frames for a topic nobody holds", () => {
    const client = connect();
    const received: TopicMsg[] = [];
    client.subscribe("session:s1", (msg) => received.push(msg));
    latest().accept();

    latest().push({ op: "event", topic: "session:s9", event: sessionEvent(1) });
    latest().push({ op: "subscribed", topic: "session:s9", head_seq: 0 });
    latest().push({ op: "session_upserted", session: makeSession("s9") });

    expect(received).toEqual([]);
    expect(client.state()).toBe("open");
  });

  it("ignores frames that are not valid JSON or not a message", () => {
    const client = connect();
    const received: TopicMsg[] = [];
    const errors: string[] = [];
    client.subscribe("session:s1", (msg) => received.push(msg));
    client.onState(() => errors.push("state changed"));
    latest().accept();
    errors.length = 0;

    for (const junk of ["{not json", "", "null", "42", '"text"', "[]", "{}", '{"op":7}']) {
      latest().pushRaw(junk);
    }
    latest().pushRaw(new ArrayBuffer(4));
    latest().pushRaw('{"op":"from_a_newer_server","topic":"session:s1"}');

    expect(received).toEqual([]);
    expect(errors).toEqual([]);
    expect(client.state()).toBe("open");

    // The connection is still usable afterwards.
    latest().push({ op: "event", topic: "session:s1", event: sessionEvent(1) });
    expect(received).toHaveLength(1);
  });

  it("reports server error frames without closing", () => {
    const onServerError = vi.fn();
    const client = connect({ onServerError });
    client.subscribe("sessions", () => {});
    latest().accept();

    latest().push({ op: "error", message: "unknown topic" });

    expect(onServerError).toHaveBeenCalledWith("unknown topic");
    expect(client.state()).toBe("open");
  });

  it("reconnects with capped exponential backoff and resets after a success", () => {
    const client = connect();
    const states: ConnectionState[] = [];
    client.onState((state) => states.push(state));
    expect(client.state()).toBe("connecting");
    client.subscribe("sessions", () => {});

    // The base is 100 ms and the cap 800 ms; each failed attempt doubles the wait.
    for (const delay of [100, 200, 400, 800, 800]) {
      const before = FakeWebSocket.sockets.length;
      latest().hangUp();
      expect(client.state()).toBe("reconnecting");
      vi.advanceTimersByTime(delay - 1);
      expect(FakeWebSocket.sockets).toHaveLength(before);
      vi.advanceTimersByTime(1);
      expect(FakeWebSocket.sockets).toHaveLength(before + 1);
    }

    latest().accept();
    expect(client.state()).toBe("open");
    expect(states).toEqual(["reconnecting", "open"]);

    const before = FakeWebSocket.sockets.length;
    latest().hangUp();
    vi.advanceTimersByTime(100);
    expect(FakeWebSocket.sockets).toHaveLength(before + 1);
  });

  it("spreads reconnect delays with jitter", () => {
    const client = connect({ random: () => 0 });
    client.subscribe("sessions", () => {});
    latest().hangUp();

    // The bottom of the jitter range is half the ceiling.
    vi.advanceTimersByTime(49);
    expect(FakeWebSocket.sockets).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(FakeWebSocket.sockets).toHaveLength(2);
  });

  describe("with no options", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
      vi.restoreAllMocks();
    });

    it("opens the browser's socket on the page's address and backs off from 500 ms to 15 s", () => {
      vi.stubGlobal("WebSocket", FakeWebSocket);
      // The top of the jitter range: the delay is exactly the backoff ceiling.
      vi.spyOn(Math, "random").mockReturnValue(1);
      const client = createSocket();
      client.subscribe("sessions", () => {});
      expect(latest().url).toBe(defaultSocketUrl(location));

      for (const delay of [500, 1000, 2000, 4000, 8000, 15_000, 15_000]) {
        const before = FakeWebSocket.sockets.length;
        latest().hangUp();
        vi.advanceTimersByTime(delay - 1);
        expect(FakeWebSocket.sockets).toHaveLength(before);
        vi.advanceTimersByTime(1);
        expect(FakeWebSocket.sockets).toHaveLength(before + 1);
      }
      client.close();
    });
  });

  it("resubscribes every active topic after a reconnect, from the last applied seq", () => {
    const client = connect();
    let applied = 0;
    client.subscribe("sessions", () => {});
    client.subscribe(
      "session:s1",
      (msg) => {
        if (msg.op === "event") applied = msg.event.seq;
      },
      () => applied,
    );
    const gone = client.subscribe("session:s2", () => {});
    latest().accept();
    expect(latest().take()).toEqual([
      { op: "subscribe", topic: "sessions" },
      // Nothing applied yet: the whole log is wanted, so no `after_seq`.
      { op: "subscribe", topic: "session:s1" },
      { op: "subscribe", topic: "session:s2" },
    ]);
    for (const seq of [1, 2, 3]) {
      latest().push({ op: "event", topic: "session:s1", event: sessionEvent(seq) });
    }
    gone();
    latest().take();

    latest().hangUp();
    vi.advanceTimersByTime(100);
    latest().accept();

    expect(FakeWebSocket.sockets).toHaveLength(2);
    expect(latest().take()).toEqual([
      { op: "subscribe", topic: "sessions" },
      { op: "subscribe", topic: "session:s1", after_seq: 3 },
    ]);
  });

  it("asks for what follows the very first event once it has it", () => {
    const client = connect();
    client.subscribe(
      "session:s1",
      () => {},
      () => 1,
    );
    latest().accept();
    expect(latest().take()).toEqual([{ op: "subscribe", topic: "session:s1", after_seq: 1 }]);
  });

  it("takes a server error frame in its stride when nobody listens for one", () => {
    const client = connect();
    client.subscribe("sessions", () => {});
    latest().accept();
    expect(() => latest().push({ op: "error", message: "unknown topic" })).not.toThrow();
    expect(client.state()).toBe("open");
  });

  it("asks for the oldest position when subscribers of a topic differ", () => {
    const client = connect();
    client.subscribe(
      "session:s1",
      () => {},
      () => 9,
    );
    client.subscribe(
      "session:s1",
      () => {},
      () => 4,
    );
    latest().accept();
    expect(latest().take()).toEqual([{ op: "subscribe", topic: "session:s1", after_seq: 4 }]);
  });

  it("resyncs a topic by leaving and rejoining from the given seq", () => {
    const client = connect();
    client.subscribe("session:s1", () => {});
    latest().accept();
    latest().take();

    client.resync("session:s1", 7);
    client.resync("session:nobody", 1);

    expect(latest().take()).toEqual([
      { op: "unsubscribe", topic: "session:s1" },
      { op: "subscribe", topic: "session:s1", after_seq: 7 },
    ]);
  });

  it("pings on a timer and keeps the connection while pongs come back", () => {
    const client = connect();
    client.subscribe("sessions", () => {});
    latest().accept();
    latest().take();

    for (let round = 0; round < 3; round += 1) {
      // Each round is one ping interval: 700 ms of quiet, the ping, the pong 300 ms later.
      vi.advanceTimersByTime(round === 0 ? 1000 : 700);
      expect(latest().take()).toEqual([{ op: "ping" }]);
      vi.advanceTimersByTime(300);
      latest().push({ op: "pong" });
    }
    // Well past the pong timeout of the last ping, which was answered.
    vi.advanceTimersByTime(650);

    expect(FakeWebSocket.sockets).toHaveLength(1);
    expect(client.state()).toBe("open");
  });

  it("treats a missing pong as a dead connection and reconnects", () => {
    const client = connect();
    client.subscribe(
      "session:s1",
      () => {},
      () => 5,
    );
    const dead = latest();
    dead.accept();
    dead.take();

    vi.advanceTimersByTime(1000);
    expect(dead.take()).toEqual([{ op: "ping" }]);
    vi.advanceTimersByTime(399);
    expect(client.state()).toBe("open");
    vi.advanceTimersByTime(1);

    expect(client.state()).toBe("reconnecting");
    expect(dead.closed).toBe(true);

    vi.advanceTimersByTime(100);
    expect(latest()).not.toBe(dead);
    latest().accept();
    expect(client.state()).toBe("open");
    expect(latest().take()).toEqual([{ op: "subscribe", topic: "session:s1", after_seq: 5 }]);

    // The dead socket is fully let go: a late event from it changes nothing.
    dead.hangUp();
    dead.accept();
    expect(client.state()).toBe("open");
    expect(FakeWebSocket.sockets).toHaveLength(2);
  });

  it("reconnects at once when asked, and not at all once closed", () => {
    const client = connect();
    client.subscribe("sessions", () => {});
    latest().hangUp();

    client.reconnectNow();
    expect(FakeWebSocket.sockets).toHaveLength(2);
    // The backoff timer it replaced must not open a third socket.
    vi.advanceTimersByTime(10_000);
    expect(FakeWebSocket.sockets).toHaveLength(2);

    client.reconnectNow();
    expect(FakeWebSocket.sockets).toHaveLength(2);

    client.close();
    expect(latest().closed).toBe(true);
    vi.advanceTimersByTime(60_000);
    expect(FakeWebSocket.sockets).toHaveLength(2);
  });

  it("hands a refused subscribe to the subscribers of its topic, not to the error log", () => {
    const onServerError = vi.fn();
    const client = connect({ onServerError });
    const session: TopicMsg[] = [];
    const sessions: TopicMsg[] = [];
    client.subscribe("session:gone", (msg) => session.push(msg));
    client.subscribe("sessions", (msg) => sessions.push(msg));
    latest().accept();

    latest().push({ op: "error", message: "unknown session", topic: "session:gone" });

    expect(session).toEqual([{ op: "error", message: "unknown session", topic: "session:gone" }]);
    expect(sessions).toEqual([]);
    expect(onServerError).not.toHaveBeenCalled();
    expect(client.state()).toBe("open");
  });

  it("drops a refusal that arrives after everyone has left the topic", () => {
    const onServerError = vi.fn();
    const client = connect({ onServerError });
    const received: TopicMsg[] = [];
    client.subscribe("sessions", (msg) => received.push(msg));
    // The page learned over REST that the session does not exist, and left first.
    client.subscribe("session:gone", (msg) => received.push(msg))();
    latest().accept();

    latest().push({ op: "error", message: "unknown session", topic: "session:gone" });

    expect(received).toEqual([]);
    expect(onServerError).not.toHaveBeenCalled();
  });

  it("asks again for a refused topic without leaving it first", () => {
    const client = connect();
    const unsubscribe = client.subscribe("session:s1", () => {});
    latest().accept();
    latest().take();
    latest().push({ op: "error", message: "unknown session", topic: "session:s1" });

    client.resync("session:s1", 0);
    expect(latest().take()).toEqual([{ op: "subscribe", topic: "session:s1" }]);

    // Still refused: there is nothing on the server to leave.
    unsubscribe();
    expect(latest().take()).toEqual([]);
  });

  it("treats a topic as joined again once the server confirms it", () => {
    const client = connect();
    const unsubscribe = client.subscribe("session:s1", () => {});
    latest().accept();
    latest().push({ op: "error", message: "not yet", topic: "session:s1" });
    client.resync("session:s1", 0);
    latest().push({ op: "subscribed", topic: "session:s1", head_seq: 0 });
    latest().take();

    unsubscribe();
    expect(latest().take()).toEqual([{ op: "unsubscribe", topic: "session:s1" }]);
  });

  it("forgets a refusal when it reconnects", () => {
    const client = connect();
    client.subscribe("session:s1", () => {});
    latest().accept();
    latest().push({ op: "error", message: "unknown session", topic: "session:s1" });
    latest().hangUp();
    vi.advanceTimersByTime(100);
    latest().accept();
    expect(latest().take()).toEqual([{ op: "subscribe", topic: "session:s1" }]);

    client.resync("session:s1", 3);
    expect(latest().take()).toEqual([
      { op: "unsubscribe", topic: "session:s1" },
      { op: "subscribe", topic: "session:s1", after_seq: 3 },
    ]);
  });

  describe("refusals", () => {
    it("reports an attempt that ended before it opened, and keeps retrying", () => {
      const client = connect();
      const refused = vi.fn();
      client.onRefused(refused);
      client.subscribe("sessions", () => {});

      latest().hangUp();
      expect(refused).toHaveBeenCalledTimes(1);
      expect(client.state()).toBe("reconnecting");

      vi.advanceTimersByTime(100);
      expect(FakeWebSocket.sockets).toHaveLength(2);
    });

    it("does not report a connection that opened and then dropped", () => {
      const client = connect();
      const refused = vi.fn();
      client.onRefused(refused);
      client.subscribe("sessions", () => {});
      latest().accept();

      latest().hangUp();
      expect(refused).not.toHaveBeenCalled();
    });

    it("stops for good when the listener closes it", () => {
      const client = connect();
      client.onRefused(() => client.close());
      client.subscribe("sessions", () => {});

      latest().hangUp();
      vi.advanceTimersByTime(10_000);
      client.reconnectNow();
      expect(FakeWebSocket.sockets).toHaveLength(1);
    });
  });
});
