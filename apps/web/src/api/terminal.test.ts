import { describe, expect, it, vi } from "vite-plus/test";
import {
  createTerminalOpener,
  terminalUrl,
  type TerminalEnd,
  type TerminalSocketLike,
} from "./terminal";

class FakeWebSocket implements TerminalSocketLike {
  static instances: FakeWebSocket[] = [];
  binaryType: BinaryType = "blob";
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  readonly sent: Array<string | Uint8Array> = [];
  closed = 0;
  readonly url: string;

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }

  send(data: string | Uint8Array<ArrayBuffer>): void {
    this.sent.push(data);
  }

  close(): void {
    this.closed += 1;
  }

  open(): void {
    this.onopen?.(new Event("open"));
  }

  receive(data: unknown): void {
    this.onmessage?.(new MessageEvent("message", { data }));
  }

  drop(): void {
    this.onclose?.(new CloseEvent("close"));
  }
}

const here = { protocol: "http:", host: "calin.test:4680" };

function connect(request = { sessionId: "s1", cols: 80, rows: 24 }) {
  FakeWebSocket.instances = [];
  const open = createTerminalOpener({ WebSocket: FakeWebSocket, location: here });
  const output: string[] = [];
  const ends: TerminalEnd[] = [];
  const onOpen = vi.fn();
  const decoder = new TextDecoder();
  const client = open(request, {
    onOpen,
    onData: (bytes) => output.push(decoder.decode(bytes)),
    onEnd: (end) => ends.push(end),
  });
  const [socket] = FakeWebSocket.instances;
  return { client, socket, output, ends, onOpen };
}

const bytes = (text: string): ArrayBuffer => new TextEncoder().encode(text).buffer;
const asText = (frame: string | Uint8Array): string =>
  typeof frame === "string" ? `text:${frame}` : `bytes:${new TextDecoder().decode(frame)}`;

describe("terminal client", () => {
  it("opens the session's terminal socket with the size, and the repo when one is named", () => {
    const { socket } = connect({ sessionId: "s 1", cols: 132, rows: 43 });
    expect(socket.url).toBe(
      "ws://calin.test:4680/api/v1/sessions/s%201/terminals/ws?cols=132&rows=43",
    );
    expect(socket.binaryType).toBe("arraybuffer");

    const url = new URL(
      terminalUrl(
        { protocol: "https:", host: "calin.test" },
        { sessionId: "s1", sessionRepoId: "sr/1 &x", cols: 80, rows: 24 },
      ),
    );
    expect(url.protocol).toBe("wss:");
    expect(url.pathname).toBe("/api/v1/sessions/s1/terminals/ws");
    expect(Object.fromEntries(url.searchParams)).toEqual({
      session_repo_id: "sr/1 &x",
      cols: "80",
      rows: "24",
    });
  });

  it("relays output bytes in and typed input out as binary frames", () => {
    const { client, socket, output, onOpen } = connect();
    socket.open();
    expect(onOpen).toHaveBeenCalledTimes(1);

    socket.receive(bytes("api $ "));
    socket.receive(bytes("héllo\r\n"));
    expect(output).toEqual(["api $ ", "héllo\r\n"]);

    client.write("ls -la\r");
    client.write(new Uint8Array([0x1b, 0x5b, 0x41]));
    client.write("é");
    expect(socket.sent.map(asText)).toEqual(["bytes:ls -la\r", "bytes:\x1b[A", "bytes:é"]);
    // Text never goes out as a text frame: those are control messages.
    expect(socket.sent.every((frame) => frame instanceof Uint8Array)).toBe(true);
    expect(socket.sent[2]).toEqual(new Uint8Array([0xc3, 0xa9]));
  });

  it("sends a resize as a JSON control frame", () => {
    const { client, socket } = connect();
    socket.open();
    client.resize(120, 40);
    expect(socket.sent).toEqual(['{"op":"resize","cols":120,"rows":40}']);
  });

  it("holds what is typed and resized while connecting, and sends it in order once open", () => {
    const { client, socket } = connect();
    client.write("a");
    client.resize(90, 20);
    client.write("b");
    expect(socket.sent).toEqual([]);
    socket.open();
    expect(socket.sent.map(asText)).toEqual([
      "bytes:a",
      'text:{"op":"resize","cols":90,"rows":20}',
      "bytes:b",
    ]);
  });

  it("reports the exit frame with its code, once, and not the close that follows", () => {
    const { socket, ends } = connect();
    socket.open();
    socket.receive('{"op":"exit","code":3}');
    socket.drop();
    expect(ends).toEqual([{ kind: "exit", code: 3 }]);
    expect(socket.closed).toBe(1);
  });

  it("reports an exit without a code", () => {
    const { socket, ends } = connect();
    socket.open();
    socket.receive('{"op":"exit","code":null}');
    expect(ends).toEqual([{ kind: "exit", code: null }]);
  });

  it("reports the error frame with the server's message", () => {
    const { socket, ends } = connect();
    socket.open();
    socket.receive('{"op":"error","message":"the shell could not be started"}');
    socket.drop();
    expect(ends).toEqual([{ kind: "error", message: "the shell could not be started" }]);
  });

  it("reports a socket that drops as a lost shell, and one that never opened as such", () => {
    const dropped = connect();
    dropped.socket.open();
    dropped.socket.drop();
    expect(dropped.ends).toEqual([{ kind: "lost", opened: true }]);

    const refused = connect();
    refused.socket.onerror?.(new Event("error"));
    refused.socket.drop();
    expect(refused.ends).toEqual([{ kind: "lost", opened: false }]);
  });

  it("closes the socket on close(), and stays silent afterwards", () => {
    const { client, socket, ends, output } = connect();
    socket.open();
    client.close();
    expect(socket.closed).toBe(1);
    expect(socket.onmessage).toBeNull();
    expect(socket.onclose).toBeNull();

    client.write("ignored");
    client.resize(1, 1);
    client.close();
    expect(socket.sent).toEqual([]);
    expect(socket.closed).toBe(1);
    expect(ends).toEqual([]);
    expect(output).toEqual([]);
  });

  it("ignores frames it does not understand", () => {
    const { socket, ends, output } = connect();
    socket.open();
    socket.receive("not json");
    socket.receive('{"op":"bell"}');
    socket.receive("{}");
    socket.receive(new Blob(["x"]));
    expect(ends).toEqual([]);
    expect(output).toEqual([]);
  });
});
