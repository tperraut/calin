import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { Agent } from "../gen/Agent";
import type { Item } from "../gen/Item";
import { speakerFor } from "../lib/messages";
import type { TranscriptEntry } from "../state/sessionReducer";
import { makeSession } from "../test/fakes";
import { makeAgent, makeMessage, makeSubagent } from "../test/orchestration";
import { choose, click, find, findByText, mount, unmountAll } from "../test/render";
import { Transcript, type RevealRequest } from "./Transcript";

afterEach(unmountAll);

const item = (
  id: string,
  agentId: string,
  content: Item["content"],
  status: Item["status"] = "completed",
): TranscriptEntry => ({
  kind: "item",
  key: `item:${id}`,
  ts: 0,
  agentId,
  item: { id, status, content },
  interrupted: false,
});
const text = (id: string, agentId: string, value: string): TranscriptEntry =>
  item(id, agentId, { kind: "text", text: value });
const call = (
  id: string,
  agentId: string,
  title: string,
  task = false,
  status: Item["status"] = "completed",
): TranscriptEntry =>
  item(
    id,
    agentId,
    {
      kind: "tool",
      name: task ? "Task" : "Read",
      tool_kind: task ? "task" : "read",
      title,
      input: {},
      output: null,
      locations: [],
    },
    status,
  );

const lead = makeAgent("a1");
const entries: TranscriptEntry[] = [
  { kind: "user", key: "event:u", ts: 0, text: "Ship it" },
  text("m1", "a1", "Looking around first."),
  call("t1", "a1", "Explore the routes", true),
  call("x1", "sub1", "rg Router"),
  call("x2", "sub1", "src/main.rs"),
  text("x3", "sub1", "The routes live in main.rs."),
  call("t2", "a1", "Review the page", true, "running"),
  call("y1", "sub2", "src/Status.tsx", false, "running"),
  text("m2", "a1", "Splitting the work."),
];
const calls = { sub1: "t1", sub2: "t2" };

function setup(
  agents: Agent[] = [
    lead,
    makeSubagent("sub1", "explorer", { status: "done" }),
    makeSubagent("sub2", "reviewer"),
  ],
) {
  const [filter, setFilter] = createSignal<string | null>(null);
  const [reveal, setReveal] = createSignal<RevealRequest>();
  const [list, setList] = createSignal(agents);
  const host = mount(() => (
    <Transcript
      entries={entries}
      synced={true}
      agents={list()}
      subagentCalls={calls}
      filter={filter()}
      onFilter={setFilter}
      reveal={reveal()}
    />
  ));
  const group = (id: string) => find(host, `[data-subagent="${id}"]`);
  return {
    host,
    group,
    summary: (id: string) => find<HTMLButtonElement>(group(id), "button[aria-expanded]"),
    filter,
    setFilter: (next: string | null) => {
      setFilter(next);
      flush();
    },
    setAgents: (next: Agent[]) => {
      setList(next);
      flush();
    },
    setReveal: (next: RevealRequest) => {
      setReveal(next);
      flush();
    },
    rows: () => [...find(host, '[role="log"] ol').children].map((row) => row.textContent),
  };
}

describe("Transcript: subagents", () => {
  it("groups a subagent's rows under the tool call that started it, labelled with its role", () => {
    const { host, group } = setup();
    const top = [...find(host, '[role="log"] ol').children];
    // You, the lead's text, the call, its subagent, the second call, its subagent, the lead's text.
    expect(top).toHaveLength(7);
    expect(top[2].textContent).toContain("Explore the routes");
    expect(top[3].firstElementChild).toBe(group("sub1"));
    expect(top[5].firstElementChild).toBe(group("sub2"));
    expect(group("sub1").getAttribute("aria-label")).toBe("Subagent explorer");
    // Indented under the call.
    expect(group("sub1").className).toContain("ml-3");
  });

  it("folds a finished subagent into one line: role, tool calls, status; a running one stays open", async () => {
    const { group, summary } = setup();
    expect(summary("sub1").textContent).toBe("▸explorer · 2 tool calls · Done");
    expect(summary("sub1").getAttribute("aria-expanded")).toBe("false");
    expect(group("sub1").querySelector("ol")).toBeNull();
    expect(summary("sub2").textContent).toBe("▸reviewer · 1 tool call · Running");
    expect(summary("sub2").getAttribute("aria-expanded")).toBe("true");
    expect(group("sub2").textContent).toContain("src/Status.tsx");

    await click(summary("sub1"));
    expect(summary("sub1").getAttribute("aria-expanded")).toBe("true");
    expect(
      [...find(group("sub1"), "ol").children].map((row) => row.getAttribute("data-item")),
    ).toEqual(["x1", "x2", "x3"]);
    expect(group("sub1").textContent).toContain("The routes live in main.rs.");
  });

  it("folds a subagent when it ends, unless the reader chose otherwise", async () => {
    const { summary, setAgents } = setup();
    await click(summary("sub1"));
    setAgents([
      lead,
      makeSubagent("sub1", "explorer", { status: "done" }),
      makeSubagent("sub2", "reviewer", { status: "failed" }),
    ]);
    expect(summary("sub2").getAttribute("aria-expanded")).toBe("false");
    expect(summary("sub2").textContent).toContain("Failed");
    // The one the reader opened stays open.
    expect(summary("sub1").getAttribute("aria-expanded")).toBe("true");
  });

  it("opens the group an item is in when asked to reveal it, and marks the row", async () => {
    const { group, summary, setReveal, host } = setup();
    const scrolled = vi.fn();
    Element.prototype.scrollIntoView = scrolled;
    setReveal({ itemId: "x2", seq: 1 });
    expect(summary("sub1").getAttribute("aria-expanded")).toBe("true");
    const row = find(group("sub1"), '[data-item="x2"]');
    expect(row.getAttribute("data-revealed")).toBe("true");
    await new Promise((done) => setTimeout(done, 5));
    expect(scrolled).toHaveBeenCalledTimes(1);
    // Another one: the mark moves.
    setReveal({ itemId: "m2", seq: 2 });
    expect(host.querySelectorAll("[data-revealed]")).toHaveLength(1);
    expect(find(host, "[data-revealed]").getAttribute("data-item")).toBe("m2");
  });
});

describe("Transcript: the agent filter", () => {
  it("offers all agents or one of them, named by role", () => {
    const { host } = setup();
    const select = find<HTMLSelectElement>(host, 'select[aria-label="Agent filter"]');
    expect([...select.options].map((option) => [option.value, option.textContent])).toEqual([
      ["", "All agents"],
      ["a1", "lead"],
      ["sub1", "explorer (subagent)"],
      ["sub2", "reviewer (subagent)"],
    ]);
    expect(select.value).toBe("");
  });

  it("shows only the chosen agent's items, flat, and everything again with All agents", () => {
    const { host, rows, filter } = setup();
    const select = find<HTMLSelectElement>(host, 'select[aria-label="Agent filter"]');
    choose(select, "sub1");
    expect(filter()).toBe("sub1");
    expect(rows()).toHaveLength(3);
    expect(host.querySelector("[data-subagent]")).toBeNull();
    expect(find(host, '[role="log"]').textContent).toContain("The routes live in main.rs.");
    expect(find(host, '[role="log"]').textContent).not.toContain("Splitting the work.");

    choose(select, "a1");
    expect(rows()).toHaveLength(4);
    expect(find(host, '[role="log"]').textContent).not.toContain("Ship it");

    choose(select, "");
    expect(filter()).toBeNull();
    expect(rows()).toHaveLength(7);
  });

  it("follows a filter set from outside, and says so when the agent has nothing yet", () => {
    const quiet = makeSubagent("sub9", "tester");
    const { host, setFilter, setAgents } = setup();
    setAgents([lead, makeSubagent("sub1", "explorer"), makeSubagent("sub2", "reviewer"), quiet]);
    setFilter("sub2");
    expect(find<HTMLSelectElement>(host, "select").value).toBe("sub2");
    expect(find(host, '[role="log"]').textContent).toContain("src/Status.tsx");
    setFilter("sub9");
    expect(find(host, '[role="log"]').textContent).toBe("Nothing from this agent yet.");
    // An agent the session does not have: everything shows.
    setFilter("nobody");
    expect(find<HTMLSelectElement>(host, "select").value).toBe("");
  });

  it("has no filter where the session has a single agent and none is set", () => {
    const { host, setFilter } = setup([lead]);
    expect(host.querySelector("select")).toBeNull();
    // Set from the graph, it shows so it can be cleared.
    setFilter("a1");
    expect(find<HTMLSelectElement>(host, "select").value).toBe("a1");
  });
});

describe("Transcript: messages and questions", () => {
  const parent = makeSession("s1", 1000, { title: "Ship the health check" });
  const child = makeSession("s2", 2000, {
    title: "API: add /health",
    parent_session_id: "s1",
    root_session_id: "s1",
  });
  const speaker = speakerFor(parent, [lead], [parent, child]);
  const message = (
    id: string,
    direction: "sent" | "received",
    patch: Parameters<typeof makeMessage>[1],
  ): TranscriptEntry => ({
    kind: "message",
    key: `message:${id}`,
    ts: 0,
    agentId: "a1",
    direction,
    message: makeMessage(id, patch),
  });
  const fromChild = {
    from_session_id: "s2",
    from_agent_id: "a2",
    to_session_id: "s1",
    to_agent_id: "a1",
  };

  it("says who to whom and what kind, with the body as Markdown", () => {
    const host = mount(() => (
      <Transcript
        synced={true}
        speaker={speaker}
        entries={[
          message("d1", "sent", { kind: "dispatch", body: "Add `GET /health`." }),
          message("n1", "received", { ...fromChild, kind: "note", body: "Reuse **/status**?" }),
          message("r1", "received", { ...fromChild, kind: "report", body: "Done." }),
          message("n2", "sent", {
            kind: "note",
            to_session_id: "s1",
            to_agent_id: null,
            body: "Half way.",
          }),
        ]}
      />
    ));
    const rows = [...host.querySelectorAll<HTMLElement>("[data-message]")];
    expect(rows.map((row) => find(row, "p").textContent)).toEqual([
      "Dispatched child session API: add /health",
      "Note from API: add /health",
      "Report from API: add /health",
      "Note to you",
    ]);
    expect(rows.map((row) => [row.dataset.message, row.dataset.direction])).toEqual([
      ["dispatch", "sent"],
      ["note", "received"],
      ["report", "received"],
      ["note", "sent"],
    ]);
    expect(find(rows[0], "code").textContent).toBe("GET /health");
    expect(find(rows[1], "strong").textContent).toBe("/status");
    // Short bodies are whole: nothing to unfold.
    expect(host.querySelector("[data-message] button")).toBeNull();
  });

  it("folds a long body, and unfolds it on request", async () => {
    const long = Array.from({ length: 30 }, (_, index) => `Line ${index + 1}`).join("\n");
    const host = mount(() => (
      <Transcript
        synced={true}
        speaker={speaker}
        entries={[message("d1", "sent", { kind: "dispatch", body: long })]}
      />
    ));
    const row = find(host, "[data-message]");
    const body = find(row, ".markdown").parentElement as HTMLElement;
    expect(body.className).toContain("max-h-28");
    const toggle = findByText<HTMLButtonElement>(row, "button", "Show the whole message");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    await click(toggle);
    expect(body.className).not.toContain("max-h-28");
    expect(findByText(row, "button", "Show less").getAttribute("aria-expanded")).toBe("true");
  });

  it("shows a question where it was asked, then who answered and what, or that it is no longer needed", () => {
    const question = { id: "q1", text: "Header or **footer**?", options: ["Header", "Footer"] };
    const row = (
      outcome: Extract<TranscriptEntry, { kind: "question" }>["outcome"],
    ): TranscriptEntry => ({
      kind: "question",
      key: `question:${outcome.state}`,
      ts: 0,
      agentId: "a1",
      question,
      outcome,
    });
    const host = mount(() => (
      <Transcript
        synced={true}
        entries={[
          row({ state: "pending" }),
          row({ state: "answered", answer: "Header", by: "user" }),
          row({ state: "expired" }),
        ]}
      />
    ));
    const [pending, answered, expired] = [...host.querySelectorAll<HTMLElement>("[data-question]")];
    expect(pending.textContent).toContain("Question for you");
    expect(pending.textContent).toContain("Waiting for you");
    expect(find(pending, "strong").textContent).toBe("footer");
    expect(answered.textContent).toContain("Answered by you");
    expect(answered.textContent).toContain("Answer: Header");
    expect(expired.textContent).toContain("No longer needed");
    expect(expired.textContent).not.toContain("Answer:");
  });
});
