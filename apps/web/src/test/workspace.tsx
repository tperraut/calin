// A session store over fakes, for tests that mount one workspace panel on its own.
import { createSignal, flush, type Accessor } from "solid-js";
import type { Api } from "../api/http";
import type { OpenTerminal } from "../api/terminal";
import type { EventBody } from "../gen/EventBody";
import type { Health } from "../gen/Health";
import type { PullRequest } from "../gen/PullRequest";
import type { Session } from "../gen/Session";
import type { SessionRepo } from "../gen/SessionRepo";
import type { SessionStatus } from "../gen/SessionStatus";
import type { FileRequest, FileTarget, PanelId, PanelLinks } from "../panels/registry";
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
  /** How many times a panel asked for the ship form. */
  shipForms: () => number;
  /** Messages a panel drafted for the agent, in order. */
  drafts: string[];
  /** Panels, agents and items a panel asked the screen to show, in order. */
  panels: PanelId[];
  shownAgents: string[];
  shownItems: string[];
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
    pullRequests?: PullRequest[];
    status?: SessionStatus;
    openTerminal?: OpenTerminal;
    health?: Health;
    now?: number;
    /** What the `sessions` topic lists, for the session's tree. */
    sessions?: () => readonly Session[];
  } = {},
): Promise<Workspace> {
  const socket = new FakeSocket();
  const session = makeSession("s1", 1000, { status: options.status ?? "running" });
  const api = fakeApi({
    getSession: () =>
      Promise.resolve(makeDetail(session, options.repos ?? [], options.pullRequests ?? [])),
    ...options.routes,
  });
  const store = createSessionStore(
    {
      api,
      socket,
      schedule: immediately,
      openTerminal: options.openTerminal,
      health: () => options.health,
      sessions: options.sessions,
      now: () => options.now ?? 1000,
    },
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
  const drafts: string[] = [];
  const panels: PanelId[] = [];
  const shownAgents: string[] = [];
  const shownItems: string[] = [];
  let shipForms = 0;
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
      openShipForm: () => {
        shipForms += 1;
      },
      draftMessage: (text) => drafts.push(text),
      openPanel: (panel) => panels.push(panel),
      showAgent: (agentId) => shownAgents.push(agentId),
      showItem: (itemId) => shownItems.push(itemId),
    },
    opened,
    unsaved,
    shipForms: () => shipForms,
    drafts,
    panels,
    shownAgents,
    shownItems,
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
