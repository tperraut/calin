// A session store over fakes, for tests that mount one workspace panel on its own.
import { createSignal, flush, type Accessor } from "solid-js";
import type { Api } from "../api/http";
import type { OpenTerminal } from "../api/terminal";
import type { EventBody } from "../gen/EventBody";
import type { SessionRepo } from "../gen/SessionRepo";
import type { SessionStatus } from "../gen/SessionStatus";
import type { FileRequest, FileTarget, PanelLinks } from "../panels/registry";
import { createSessionStore, type SessionStore } from "../state/sessionStore";
import {
  fakeApi,
  FakeSocket,
  immediately,
  makeDetail,
  makeEvent,
  makeSession,
  settle,
} from "./fakes";

export type Workspace = {
  store: SessionStore;
  socket: FakeSocket;
  /** Delivers one session event, as the agent `a1` unless told otherwise. */
  emit: (body: EventBody, agentId?: string | null) => void;
  /** Ends the replay: from here events are live. */
  caughtUp: () => void;
  links: PanelLinks;
  /** Files other panels were asked to open, in order. */
  opened: FileTarget[];
  unsaved: Array<string | undefined>;
  /** Whether the panel under test is the one on screen. */
  active: Accessor<boolean>;
  setActive: (active: boolean) => void;
  /** Makes another panel ask for a file, as "Open in Files" does. */
  requestFile: (target: FileTarget) => void;
};

export async function openWorkspace(
  options: {
    routes?: Partial<Api>;
    repos?: SessionRepo[];
    status?: SessionStatus;
    openTerminal?: OpenTerminal;
  } = {},
): Promise<Workspace> {
  const socket = new FakeSocket();
  const session = makeSession("s1", 1000, { status: options.status ?? "running" });
  const api = fakeApi({
    getSession: () => Promise.resolve(makeDetail(session, options.repos ?? [])),
    ...options.routes,
  });
  const store = createSessionStore(
    { api, socket, schedule: immediately, openTerminal: options.openTerminal },
    "s1",
  );
  store.start();
  await settle();
  flush();

  let seq = 0;
  let requests = 0;
  const [active, setActive] = createSignal(true);
  const [fileRequest, setFileRequest] = createSignal<FileRequest>();
  const opened: FileTarget[] = [];
  const unsaved: Array<string | undefined> = [];
  return {
    store,
    socket,
    emit(body, agentId = "a1") {
      seq += 1;
      socket.emit("session:s1", {
        op: "event",
        topic: "session:s1",
        event: makeEvent(seq, body, agentId),
      });
      flush();
    },
    caughtUp() {
      socket.emit("session:s1", { op: "subscribed", topic: "session:s1", head_seq: seq });
      flush();
    },
    links: {
      openFile: (target) => opened.push(target),
      fileRequest,
      setUnsavedWork: (description) => unsaved.push(description),
    },
    opened,
    unsaved,
    active,
    setActive(next) {
      setActive(next);
      flush();
    },
    requestFile(target) {
      requests += 1;
      setFileRequest({ target, seq: requests });
      flush();
    },
  };
}

/** Lets pending requests answer and the screen follow, a few rounds over. */
export async function act(): Promise<void> {
  for (let round = 0; round < 5; round += 1) {
    await settle();
    flush();
  }
}
