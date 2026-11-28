// The orchestration views through the whole app: questions, nested sessions,
// the session's family in its header, and the way from the graph to the transcript.
import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { App } from "./App";
import type { Api } from "./api/http";
import type { EventBody } from "./gen/EventBody";
import type { Health } from "./gen/Health";
import type { Item } from "./gen/Item";
import type { Session } from "./gen/Session";
import { createAppState } from "./state/app";
import {
  conflict,
  fakeApi,
  FakeSocket,
  immediately,
  makeDetail,
  makeEvent,
  makeSession,
  settle,
} from "./test/fakes";
import { makeAgent, makeGraph, makeSubagent, makeTimeline } from "./test/orchestration";
import {
  click,
  control,
  find,
  findByText,
  mount,
  PHONE,
  setViewportWidth,
  unmountAll,
} from "./test/render";

const health: Health = {
  name: "calin",
  version: "0.3.0",
  dry_run: false,
  runners: ["mock"],
  github_problem: null,
};

const root = makeSession("s1", 1000, { title: "Ship the health check", status: "running" });
const childApi = makeSession("s2", 2000, {
  title: "API: add /health",
  status: "running",
  parent_session_id: "s1",
  root_session_id: "s1",
});
const childWeb = makeSession("s3", 3000, {
  title: "Web: status badge",
  status: "review",
  parent_session_id: "s1",
  root_session_id: "s1",
});

/** Mounts the app at `path` with the tree listed and the session `id` open and caught up. */
async function open(
  path: string,
  id: string,
  routes: Partial<Api> = {},
  sessions: Session[] = [root, childApi, childWeb],
) {
  history.replaceState(null, "", path);
  const socket = new FakeSocket();
  const record = sessions.find((one) => one.id === id) ?? root;
  const api = fakeApi({
    health: () => Promise.resolve(health),
    listRepos: () => Promise.resolve([]),
    getChanges: () => Promise.resolve([]),
    getSession: () => Promise.resolve(makeDetail(record)),
    ...routes,
  });
  const state = createAppState({ api, socket, schedule: immediately });
  const host = mount(() => <App state={state} />);
  await settle();
  flush();
  const upsert = (...list: Session[]): void => {
    for (const session of list) socket.emit("sessions", { op: "session_upserted", session });
    flush();
  };
  upsert(...sessions);
  socket.emit("sessions", { op: "subscribed", topic: "sessions", head_seq: 0 });
  let seq = 0;
  const emit = (body: EventBody, agentId: string | null = "a1"): void => {
    seq += 1;
    socket.emit(`session:${id}`, {
      op: "event",
      topic: `session:${id}`,
      event: makeEvent(seq, body, agentId),
    });
    flush();
  };
  const caughtUp = (): void => {
    socket.emit(`session:${id}`, { op: "subscribed", topic: `session:${id}`, head_seq: seq });
    flush();
  };
  return { host, main: find(host, "main"), emit, caughtUp, upsert };
}

const waitFor = (check: () => void): Promise<void> =>
  vi.waitFor(() => {
    flush();
    check();
  });

const question = {
  id: "q1",
  text: "The badge goes in the **header** or the footer?",
  options: ["Header", "Footer"],
  gate: false,
};
const tool = (
  id: string,
  title: string,
  kind: "task" | "read" = "read",
  status: Item["status"] = "completed",
): Item => ({
  id,
  status,
  content: {
    kind: "tool",
    name: "T",
    tool_kind: kind,
    title,
    input: {},
    output: null,
    locations: [],
  },
});

beforeEach(() => {
  history.replaceState(null, "", "/");
});
afterEach(() => {
  unmountAll();
  sessionStorage.clear();
});

describe("a question from the agent", () => {
  const bar = (main: HTMLElement) =>
    main.querySelector<HTMLElement>('section[aria-label="Question from the agent"]');

  it("shows above the message box, is answered through the route, and leaves with its event", async () => {
    const answerQuestion = vi.fn<Api["answerQuestion"]>().mockResolvedValue();
    const { main, emit, caughtUp } = await open("/s/s1", "s1", { answerQuestion });
    emit({ type: "session_status", status: "running", reason: null }, null);
    emit({ type: "question_asked", question });
    // Mid-replay nothing can be answered yet.
    expect(bar(main)).toBeNull();
    emit({ type: "session_status", status: "waiting", reason: null }, null);
    caughtUp();

    expect(document.title).toBe("Waiting for you · Ship the health check · Calin");
    expect(find(main, "header").textContent).toContain("Waiting");
    const shown = bar(main) as HTMLElement;
    expect(find(shown, "strong").textContent).toBe("header");
    // Same place as a permission request: after the transcript, before the message box.
    const box = find(main, 'textarea[placeholder="Message the agent"]');
    expect(shown.compareDocumentPosition(box) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(find(main, '[role="log"]').textContent).toContain("Question for you");

    await click(findByText(shown, "button", "Header"));
    expect(answerQuestion.mock.calls).toEqual([["s1", "q1", { answer: "Header" }]]);
    // Not optimistic.
    expect(bar(main)).not.toBeNull();
    expect(find(main, '[role="log"]').textContent).toContain("Waiting for you");

    emit({ type: "question_answered", question_id: "q1", answer: "Header", by: "user" });
    emit({ type: "session_status", status: "running", reason: null }, null);
    expect(bar(main)).toBeNull();
    expect(find(main, '[role="log"]').textContent).toContain("Answered by you");
    expect(find(main, '[role="log"]').textContent).toContain("Answer: Header");
    expect(document.title).toBe("Ship the health check · Calin");
  });

  it("shows the server's message when the answer is refused", async () => {
    const answerQuestion = vi
      .fn<Api["answerQuestion"]>()
      .mockRejectedValue(conflict("the session is restarting"));
    const { main, emit, caughtUp } = await open("/s/s1", "s1", { answerQuestion });
    emit({ type: "question_asked", question });
    caughtUp();
    const field = control(bar(main) as HTMLElement, "Your answer");
    field.value = "In the sidebar";
    field.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flush();
    await click(findByText(main, "button", "Send answer"));
    expect(answerQuestion.mock.calls).toEqual([["s1", "q1", { answer: "In the sidebar" }]]);
    expect(find(bar(main) as HTMLElement, '[role="alert"]').textContent).toBe(
      "the session is restarting",
    );
  });

  it("waits behind a pending permission request, and shows once that is answered", async () => {
    const { main, emit, caughtUp } = await open("/s/s1", "s1");
    emit({ type: "question_asked", question });
    emit({
      type: "permission_requested",
      request: {
        id: "p1",
        tool_name: "Bash",
        tool_kind: "execute",
        title: "cargo test",
        input: {},
        item_id: null,
      },
    });
    caughtUp();
    expect(main.querySelector('section[aria-label="Permission request"]')).not.toBeNull();
    expect(bar(main)).toBeNull();
    emit({
      type: "permission_resolved",
      request_id: "p1",
      decision: "allow",
      by: "user",
      message: null,
    });
    expect(main.querySelector('section[aria-label="Permission request"]')).toBeNull();
    expect(bar(main)).not.toBeNull();
    // Closed by the server when the session is stopped: nothing left to answer.
    emit({ type: "question_answered", question_id: "q1", answer: "", by: "expired" });
    expect(bar(main)).toBeNull();
    expect(find(main, '[role="log"]').textContent).toContain("No longer needed");
  });
});

describe("a session's family", () => {
  it("shows a parent's children as chips with their status, each leading to the child, kept current", async () => {
    const { main, upsert } = await open("/s/s1", "s1");
    const chips = [...find(main, 'ul[aria-label="Child sessions"]').querySelectorAll("a")];
    expect(chips.map((chip) => [chip.getAttribute("href"), chip.textContent])).toEqual([
      ["/s/s2", "API: add /healthRunning"],
      ["/s/s3", "Web: status badgeReview"],
    ]);
    expect(main.textContent).not.toContain("Child of");
    upsert({ ...childApi, status: "review" });
    expect(find(main, 'ul[aria-label="Child sessions"]').textContent).toContain(
      "API: add /healthReview",
    );
    // A fourth session of the tree arrives.
    upsert(
      makeSession("s4", 4000, { title: "Docs", parent_session_id: "s1", root_session_id: "s1" }),
    );
    expect(find(main, 'ul[aria-label="Child sessions"]').querySelectorAll("a")).toHaveLength(3);
  });

  it("shows a child as the child of its parent, linking to it", async () => {
    const { main } = await open("/s/s2", "s2");
    const link = findByText<HTMLAnchorElement>(find(main, "header"), "a", "Child of");
    expect(link.textContent).toBe("Child ofShip the health check");
    expect(link.getAttribute("href")).toBe("/s/s1");
    expect(main.querySelector('ul[aria-label="Child sessions"]')).toBeNull();
  });

  it("still leads a child to its parent when the parent is not in the list", async () => {
    const { main } = await open("/s/s2", "s2", {}, [childApi]);
    const family = find(find(main, "header"), '[data-part="family"]');
    const link = findByText<HTMLAnchorElement>(family, "a", "Child of");
    expect(link.textContent).toBe("Child ofits parent session");
    expect(link.getAttribute("href")).toBe("/s/s1");
  });

  it("nests the children under their parent in the list, on a phone and on a wide screen", async () => {
    const wide = await open("/s/s2", "s2", {}, [
      root,
      childApi,
      { ...childWeb, status: "done" },
      makeSession("zz", 50),
    ]);
    const hrefs = (host: HTMLElement) =>
      [...host.querySelectorAll("nav a")].map((link) => link.getAttribute("href"));
    expect(hrefs(wide.host)).toEqual(["/s/s1", "/s/s2", "/s/s3", "/s/zz"]);
    expect(
      find(wide.host, 'nav ul[aria-label="Child sessions of Ship the health check"]'),
    ).toBeTruthy();
    // The open child is marked.
    expect(find(wide.host, 'nav a[href="/s/s2"]').hasAttribute("data-active")).toBe(true);
    unmountAll();

    setViewportWidth(PHONE);
    const phone = await open("/", "s1", {}, [
      { ...root, status: "review" },
      { ...childApi, status: "done" },
      { ...childWeb, status: "done" },
    ]);
    // All settled: folded until asked for.
    expect(hrefs(phone.host)).toEqual(["/s/s1"]);
    await click(
      find(phone.host, 'nav button[aria-label="Child sessions of Ship the health check"]'),
    );
    expect(hrefs(phone.host)).toEqual(["/s/s1", "/s/s2", "/s/s3"]);
  });
});

describe("from the graph to the transcript", () => {
  const routes = (): Partial<Api> => ({
    getGraph: () => Promise.resolve(makeGraph()),
    getTimeline: () => Promise.resolve(makeTimeline()),
  });
  function play(emit: (body: EventBody, agentId?: string | null) => void): void {
    emit({ type: "agent_spawned", agent: makeAgent("a1") }, null);
    emit({
      type: "item_completed",
      item: { id: "m1", status: "completed", content: { kind: "text", text: "Looking around." } },
    });
    emit({ type: "item_started", item: tool("t1", "cargo test --workspace", "task", "running") });
    emit({ type: "agent_spawned", agent: makeSubagent("sub1", "explorer") });
    emit({ type: "item_completed", item: tool("x1", "rg Router") }, "sub1");
    emit({ type: "agent_status", status: "done" }, "sub1");
  }

  it("groups a subagent's items under its call as the events arrive", async () => {
    const { main, emit, caughtUp } = await open("/s/s1", "s1");
    play(emit);
    caughtUp();
    const group = find(main, '[data-subagent="sub1"]');
    expect(find(group, "button").textContent).toBe("▸explorer · 1 tool call · Done");
    expect(group.parentElement?.previousElementSibling?.textContent).toContain(
      "cargo test --workspace",
    );
    expect(find<HTMLSelectElement>(main, 'select[aria-label="Agent filter"]').options).toHaveLength(
      3,
    );
  });

  it("shows one agent's rows in the transcript from the graph's details, on a phone", async () => {
    setViewportWidth(PHONE);
    const { main, emit, caughtUp } = await open("/s/s1/graph", "s1", routes());
    play(emit);
    caughtUp();
    await waitFor(() => expect(main.querySelector('[data-node="agent:sub1"]')).not.toBeNull());
    find(main, '[data-node="agent:sub1"]').dispatchEvent(
      new MouseEvent("click", { bubbles: true }),
    );
    flush();
    await click(findByText(main, "button", "Show in transcript"));
    expect(location.pathname).toBe("/s/s1");
    const select = find<HTMLSelectElement>(main, 'select[aria-label="Agent filter"]');
    expect(select.value).toBe("sub1");
    const log = find(main, '[role="log"]');
    expect(log.textContent).toContain("rg Router");
    expect(log.textContent).not.toContain("Looking around.");
  });

  it("scrolls the transcript to a tool call picked in the timeline", async () => {
    setViewportWidth(PHONE);
    const { main, emit, caughtUp } = await open("/s/s1/graph", "s1", routes());
    play(emit);
    caughtUp();
    await waitFor(() => expect(main.querySelector('[aria-label="View"]')).not.toBeNull());
    await click(findByText(find(main, '[aria-label="View"]'), "button", "Timeline"));
    await waitFor(() => expect(main.querySelector('[data-span="a1/t1"]')).not.toBeNull());
    find(main, '[data-span="a1/t1"]').dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flush();
    await click(findByText(main, "button", "Show in transcript"));
    expect(location.pathname).toBe("/s/s1");
    expect(find(main, "[data-revealed]").getAttribute("data-item")).toBe("t1");
    // Everything shows again: the item may belong to any agent.
    expect(find<HTMLSelectElement>(main, 'select[aria-label="Agent filter"]').value).toBe("");
  });
});

describe("Orchestration: the queue and workflow gates", () => {
  it("says why a queued session waits, with Stop at hand", async () => {
    const queued = makeSession("s9", 9000, { title: "Waits its turn", status: "queued" });
    const { main, emit, caughtUp } = await open("/s/s9", "s9", {}, [queued]);
    emit(
      {
        type: "session_status",
        status: "queued",
        reason: "waiting for a free slot (4 of 4 running)",
      },
      null,
    );
    caughtUp();
    expect(find(main, '[data-part="queued"]').textContent).toBe(
      "Queued: waiting for a free slot (4 of 4 running)",
    );
    expect(findByText<HTMLButtonElement>(main, "button", "Stop").disabled).toBe(false);

    // Once it runs, the reason has gone with the queue.
    emit({ type: "session_status", status: "running", reason: null }, null);
    expect(main.querySelector('[data-part="queued"]')).toBeNull();
  });

  it("approves a gate with a comment", async () => {
    const answerQuestion = vi.fn<Api["answerQuestion"]>().mockResolvedValue();
    const { main, emit, caughtUp } = await open("/s/s1", "s1", { answerQuestion });
    emit(
      {
        type: "question_asked",
        question: {
          id: "g1",
          text: "Open pull requests?",
          options: ["approve", "reject"],
          gate: true,
        },
      },
      null,
    );
    caughtUp();
    const gateBar = find(main, 'section[aria-label="Workflow gate"]');
    expect(main.querySelector('section[aria-label="Question from the agent"]')).toBeNull();
    const comment = control<HTMLTextAreaElement>(gateBar, "Comment (optional)");
    comment.value = "Draft is fine";
    comment.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flush();
    await click(findByText(gateBar, "button", "Approve"));
    expect(answerQuestion.mock.calls).toEqual([
      ["s1", "g1", { answer: "approve", comment: "Draft is fine" }],
    ]);
  });
});
