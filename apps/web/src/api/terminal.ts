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

/** The only place a terminal socket is opened. Each call is one socket, one shell. */
export function createTerminalOpener(options: TerminalOptions = {}): OpenTerminal {
  return (request, handlers) => {
    const WebSocketImpl = options.WebSocket ?? WebSocket;
    const socket = new WebSocketImpl(terminalUrl(options.location ?? location, request));
    socket.binaryType = "arraybuffer";
    const encoder = new TextEncoder();

    let open = false;
    let over = false;
    // What was typed or resized while connecting goes out, in order, once the socket opens.
    let queued: Array<string | TerminalBytes> = [];

    function send(frame: string | TerminalBytes): void {
      if (over) return;
      if (open) socket.send(frame);
      else queued.push(frame);
    }

    function end(result: TerminalEnd | undefined): void {
      if (over) return;
      over = true;
      queued = [];
      socket.onopen = socket.onmessage = socket.onclose = socket.onerror = null;
      socket.close();
      if (result) handlers.onEnd(result);
    }

    socket.onopen = () => {
      open = true;
      for (const frame of queued) socket.send(frame);
      queued = [];
      handlers.onOpen?.();
    };
    socket.onmessage = (event) => {
      const data: unknown = event.data;
      if (data instanceof ArrayBuffer) {
        handlers.onData(new Uint8Array(data));
        return;
      }
      if (typeof data !== "string") return;
      const msg = parseOpFrame<TermServerMsg>(data);
      if (msg?.op === "exit") end({ kind: "exit", code: msg.code });
      else if (msg?.op === "error") end({ kind: "error", message: msg.message });
    };
    const lost = (): void => end({ kind: "lost", opened: open });
    socket.onclose = lost;
    socket.onerror = lost;

    return {
      write(data) {
        send(typeof data === "string" ? encoder.encode(data) : data);
      },
      resize(cols, rows) {
        const msg: TermClientMsg = { op: "resize", cols, rows };
        send(JSON.stringify(msg));
      },
      close() {
        end(undefined);
      },
    };
  };
}
