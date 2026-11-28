import type { TermClientMsg } from "../gen/TermClientMsg";
import type { TermServerMsg } from "../gen/TermServerMsg";
import { parseOpFrame, socketUrl } from "./frames";

/** Raw terminal bytes, as a binary frame carries them. */
export type TerminalBytes = Uint8Array<ArrayBuffer>;

/** The part of the browser `WebSocket` a terminal uses, so tests can fake it. */
export interface TerminalSocketLike {
  binaryType: BinaryType;
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent) => void) | null;
  onclose: ((event: CloseEvent) => void) | null;
  onerror: ((event: Event) => void) | null;
  send(data: string | TerminalBytes): void;
  close(): void;
}

export type TerminalRequest = {
  sessionId: string;
  /** The repo the shell starts in; the session's working directory when absent. */
  sessionRepoId?: string;
  cols: number;
  rows: number;
};

/**
 * How a shell ended. `lost`: the socket closed with neither an `exit` nor an
 * `error` frame; `opened` tells a dropped shell from one that never connected.
 */
export type TerminalEnd =
  | { kind: "exit"; code: number | null }
  | { kind: "error"; message: string }
  | { kind: "lost"; opened: boolean };

export type TerminalHandlers = {
  onOpen?: () => void;
  /** Output of the shell. */
  onData: (bytes: Uint8Array) => void;
  /** Called once, unless `close()` came first: one socket is one shell and it is over. */
  onEnd: (end: TerminalEnd) => void;
};

export interface TerminalClient {
  /** Input for the shell. Text goes out as its UTF-8 bytes; sent once the socket is open. */
  write(data: string | TerminalBytes): void;
  resize(cols: number, rows: number): void;
  /** Closes the socket, which kills the shell. No handler is called afterwards. */
  close(): void;
}

export type OpenTerminal = (request: TerminalRequest, handlers: TerminalHandlers) => TerminalClient;

export interface TerminalOptions {
  WebSocket?: new (url: string) => TerminalSocketLike;
  /** Defaults to the page's own. */
  location?: Pick<Location, "protocol" | "host">;
}

/** Address of the terminal socket (terminal.rs). */
export function terminalUrl(
  location: Pick<Location, "protocol" | "host">,
  request: TerminalRequest,
): string {
  const query = new URLSearchParams();
  if (request.sessionRepoId !== undefined) query.set("session_repo_id", request.sessionRepoId);
  query.set("cols", String(request.cols));
  query.set("rows", String(request.rows));
  const session = encodeURIComponent(request.sessionId);
  return socketUrl(location, `/sessions/${session}/terminals/ws?${query}`);
}

/** How a text frame ends the shell; `undefined` for a frame that does not. */
function frameEnd(data: string): TerminalEnd | undefined {
  const msg = parseOpFrame<TermServerMsg>(data);
  if (msg?.op === "exit") return { kind: "exit", code: msg.code };
  if (msg?.op === "error") return { kind: "error", message: msg.message };
  return undefined;
}

/** One socket, one shell. */
class TerminalConnection implements TerminalClient {
  private readonly socket: TerminalSocketLike;
  private readonly handlers: TerminalHandlers;
  private readonly encoder = new TextEncoder();
  private open = false;
  private over = false;
  // What was typed or resized while connecting goes out, in order, once the socket opens.
  private queued: Array<string | TerminalBytes> = [];

  constructor(socket: TerminalSocketLike, handlers: TerminalHandlers) {
    this.socket = socket;
    this.handlers = handlers;
    socket.binaryType = "arraybuffer";
    socket.onopen = () => this.opened();
    socket.onmessage = (event) => this.received(event.data);
    socket.onclose = socket.onerror = () => this.end({ kind: "lost", opened: this.open });
  }

  write(data: string | TerminalBytes): void {
    this.send(typeof data === "string" ? this.encoder.encode(data) : data);
  }

  resize(cols: number, rows: number): void {
    const msg: TermClientMsg = { op: "resize", cols, rows };
    this.send(JSON.stringify(msg));
  }

  close(): void {
    this.end(undefined);
  }

  private opened(): void {
    this.open = true;
    for (const frame of this.queued) this.socket.send(frame);
    this.queued = [];
    this.handlers.onOpen?.();
  }

  private received(data: unknown): void {
    if (data instanceof ArrayBuffer) {
      this.handlers.onData(new Uint8Array(data));
      return;
    }
    const result = typeof data === "string" ? frameEnd(data) : undefined;
    if (result) this.end(result);
  }

  private send(frame: string | TerminalBytes): void {
    if (this.over) return;
    if (this.open) this.socket.send(frame);
    else this.queued.push(frame);
  }

  /** `undefined`: closed from here, so nobody is told. */
  private end(result: TerminalEnd | undefined): void {
    if (this.over) return;
    this.over = true;
    this.queued = [];
    const socket = this.socket;
    socket.onopen = socket.onmessage = socket.onclose = socket.onerror = null;
    socket.close();
    if (result) this.handlers.onEnd(result);
  }
}

/** The only place a terminal socket is opened. Each call is one socket, one shell. */
export function createTerminalOpener(options: TerminalOptions = {}): OpenTerminal {
  return (request, handlers) => {
    const WebSocketImpl = options.WebSocket ?? WebSocket;
    const socket = new WebSocketImpl(terminalUrl(options.location ?? location, request));
    return new TerminalConnection(socket, handlers);
  };
}
