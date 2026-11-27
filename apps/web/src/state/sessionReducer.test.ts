import { describe, expect, it } from "vite-plus/test";
import type { Agent } from "../gen/Agent";
import type { Event } from "../gen/Event";
import type { EventBody } from "../gen/EventBody";
import type { Item } from "../gen/Item";
import type { PermissionRequest } from "../gen/PermissionRequest";
import type { SessionStatus } from "../gen/SessionStatus";
import { makeSessionRepo } from "../test/fakes";
import {
  applyEvent,
  applyEvents,
  emptySessionView,
  mergeRepos,
  type SessionView,
  type TranscriptEntry,
} from "./sessionReducer";

const SESSION = "s1";
const AGENT = "a1";

/** Builds an event log with consecutive `seq`, the way the server writes one. */
function log(...bodies: Array<EventBody | [agentId: string | null, body: EventBody]>): Event[] {
  return bodies.map((entry, index) => {
    const [agentId, body] = Array.isArray(entry) ? entry : [AGENT, entry];
    return {
      id: `e${index + 1}`,
      session_id: SESSION,
      seq: index + 1,
      ts: 1000 + index,
      agent_id: agentId,
      parent_id: null,
      body,
    };
  });
}

function fold(events: Event[], from: SessionView = emptySessionView): SessionView {
  const result = applyEvents(from, events);
  expect(result.outcome).toBe("applied");
  return result.view;
}

const agent: Agent = {
  id: AGENT,
  session_id: SESSION,
  parent_agent_id: null,
  kind: "process",
  role: "lead",
  runner: "mock",
  model: null,
  native_session_id: null,
  status: "starting",
  cost_usd: 0,
  tokens_in: 0,
  tokens_out: 0,
};

const textItem = (id: string, text: string, status: Item["status"] = "running"): Item => ({
  id,
  status,
  content: { kind: "text", text },
});

const toolItem = (id: string, output: string | null, status: Item["status"] = "running"): Item => ({
  id,
  status,
  content: {
    kind: "tool",
    name: "Bash",
    tool_kind: "execute",
    title: "ls -la",
    input: { command: "ls -la" },
    output,
    locations: [],
  },
});

const request = (id: string, patch: Partial<PermissionRequest> = {}): PermissionRequest => ({
  id,
  tool_name: "Bash",
  tool_kind: "execute",
  title: `run ${id}`,
  input: { command: `run ${id}` },
  item_id: null,
  ...patch,
});

const status = (value: SessionStatus): [null, EventBody] => [
  null,
  { type: "session_status", status: value, reason: null },
];

const kinds = (view: SessionView): string[] => view.transcript.map((entry) => entry.kind);

const items = (view: SessionView): Item[] =>
  view.transcript.flatMap((entry) => (entry.kind === "item" ? [entry.item] : []));

describe("applyEvent", () => {
  it("accumulates text deltas on the item they name", () => {
    const view = fold(
      log(
        { type: "item_started", item: textItem("i1", "") },
        { type: "item_delta", item_id: "i1", delta: { kind: "text", text: "Hel" } },
        { type: "item_delta", item_id: "i1", delta: { kind: "text", text: "lo" } },
      ),
    );
    expect(items(view)).toEqual([textItem("i1", "Hello")]);
    expect(view.seq).toBe(3);
  });

  it("accumulates reasoning deltas and tool output deltas", () => {
    const view = fold(
      log(
        {
          type: "item_started",
          item: { id: "r1", status: "running", content: { kind: "reasoning", text: "" } },
        },
        { type: "item_delta", item_id: "r1", delta: { kind: "reasoning", text: "think " } },
        { type: "item_delta", item_id: "r1", delta: { kind: "reasoning", text: "harder" } },
        { type: "item_started", item: toolItem("t1", null) },
        { type: "item_delta", item_id: "t1", delta: { kind: "output", text: "a\n" } },
        { type: "item_delta", item_id: "t1", delta: { kind: "output", text: "b\n" } },
      ),
    );
    expect(items(view)).toEqual([
      { id: "r1", status: "running", content: { kind: "reasoning", text: "think harder" } },
      toolItem("t1", "a\nb\n"),
    ]);
  });

  it("replaces the accumulated item when it completes, in place", () => {
    const view = fold(
      log(
        { type: "item_started", item: textItem("i1", "") },
        { type: "item_delta", item_id: "i1", delta: { kind: "text", text: "draft" } },
        { type: "item_started", item: textItem("i2", "second") },
        { type: "item_completed", item: textItem("i1", "final text", "completed") },
      ),
    );
    expect(items(view)).toEqual([
      textItem("i1", "final text", "completed"),
      textItem("i2", "second"),
    ]);
    // The row keeps its key and start time, so the list does not reorder or remount.
    expect(view.transcript[0]).toMatchObject({ key: "item:i1", ts: 1000 });
  });

  it("adds an item that completes without having started", () => {
    const view = fold(log({ type: "item_completed", item: textItem("i1", "done", "completed") }));
    expect(items(view)).toEqual([textItem("i1", "done", "completed")]);
  });

  it("ignores duplicates and stale events", () => {
    const events = log(
      { type: "user_message", text: "hi" },
      { type: "item_started", item: textItem("i1", "") },
      { type: "item_delta", item_id: "i1", delta: { kind: "text", text: "once" } },
    );
    const view = fold(events);
    for (const event of events) {
      const again = applyEvent(view, event);
      expect(again.outcome).toBe("duplicate");
      expect(again.view).toBe(view);
    }
    expect(items(view)).toEqual([textItem("i1", "once")]);
  });

  it("reports a gap and leaves the view untouched", () => {
    const events = log(
      { type: "user_message", text: "one" },
      { type: "user_message", text: "two" },
      { type: "user_message", text: "three" },
    );
    const view = fold(events.slice(0, 1));
    const skipped = applyEvent(view, events[2]);
    expect(skipped.outcome).toBe("gap");
    expect(skipped.view).toBe(view);
    expect(view.seq).toBe(1);

    const batch = applyEvents(emptySessionView, [events[0], events[2]]);
    expect(batch.outcome).toBe("gap");
    expect(batch.view.seq).toBe(1);
  });

  it("does not mutate the view it is given", () => {
    const before = fold(
      log(
        { type: "item_started", item: textItem("i1", "a") },
        { type: "plan_updated", entries: [{ content: "step", status: "pending" }] },
      ),
    );
    const frozen = structuredClone(before);
    const next = log(
      { type: "user_message", text: "x" },
      { type: "user_message", text: "x" },
      { type: "item_delta", item_id: "i1", delta: { kind: "text", text: "b" } },
    )[2];
    const after = applyEvent(before, next).view;
    expect(before).toEqual(frozen);
    expect(after).not.toBe(before);
    expect(after.transcript).not.toBe(before.transcript);
  });

  it("replaces the plan with each update", () => {
    const view = fold(
      log(
        { type: "plan_updated", entries: [{ content: "a", status: "in_progress" }] },
        {
          type: "plan_updated",
          entries: [
            { content: "a", status: "completed" },
            { content: "b", status: "in_progress" },
          ],
        },
      ),
    );
    expect(view.plan).toEqual([
      { content: "a", status: "completed" },
      { content: "b", status: "in_progress" },
    ]);
  });

  it("tracks agents, their status and their usage", () => {
    const view = fold(
      log(
        [null, { type: "agent_spawned", agent }],
        { type: "agent_status", status: "running" },
        { type: "usage", cost_usd: 0.01, tokens_in: 100, tokens_out: 20 },
        { type: "usage", cost_usd: 0.03, tokens_in: 250, tokens_out: 60 },
      ),
    );
    expect(view.agents).toEqual([
      { ...agent, status: "running", cost_usd: 0.03, tokens_in: 250, tokens_out: 60 },
    ]);
    // Usage events carry running totals, so the latest one replaces the earlier.
    expect(view.usage).toEqual({ cost_usd: 0.03, tokens_in: 250, tokens_out: 60 });
  });

  it("records the runner's own conversation id and model on the agent that reports them", () => {
    const other: Agent = { ...agent, id: "a2", model: "haiku" };
    const view = fold(
      log(
        [null, { type: "agent_spawned", agent }],
        [null, { type: "agent_spawned", agent: other }],
        { type: "agent_session", native_session_id: "conv-1", model: "opus" },
        // A runner that does not know its model must not erase the one on record.
        ["a2", { type: "agent_session", native_session_id: "conv-2", model: null }],
      ),
    );
    expect(view.agents).toEqual([
      { ...agent, native_session_id: "conv-1", model: "opus" },
      { ...other, native_session_id: "conv-2", model: "haiku" },
    ]);
    // It is bookkeeping: nothing for the transcript.
    expect(view.transcript).toEqual([]);
    expect(view.seq).toBe(4);
  });

  it("follows a conversation id that changes when the agent is restarted", () => {
    const view = fold(
      log(
        [null, { type: "agent_spawned", agent }],
        { type: "agent_session", native_session_id: "conv-1", model: "opus" },
        { type: "agent_session", native_session_id: "conv-2", model: "sonnet" },
      ),
    );
    expect(view.agents).toEqual([{ ...agent, native_session_id: "conv-2", model: "sonnet" }]);
  });

  it("ignores a conversation id from an agent that was never announced", () => {
    const view = fold(
      log(["ghost", { type: "agent_session", native_session_id: "conv-9", model: "opus" }]),
    );
    expect(view.agents).toEqual([]);
    expect(view.seq).toBe(1);
  });

  it("sums usage across agents and has none before the first usage event", () => {
    expect(emptySessionView.usage).toBeNull();
    const view = fold(
      log(
        ["a1", { type: "usage", cost_usd: 1, tokens_in: 10, tokens_out: 1 }],
        ["a2", { type: "usage", cost_usd: 2, tokens_in: 20, tokens_out: 2 }],
        ["a1", { type: "usage", cost_usd: 3, tokens_in: 30, tokens_out: 3 }],
      ),
    );
    expect(view.usage).toEqual({ cost_usd: 5, tokens_in: 50, tokens_out: 5 });
  });

  it("records session status and its reason", () => {
    const view = fold(
      log(
        [null, { type: "session_status", status: "running", reason: null }],
        [null, { type: "session_status", status: "failed", reason: "runner exited" }],
      ),
    );
    expect(view.status).toBe("failed");
    expect(view.statusReason).toBe("runner exited");
  });

  it("ignores deltas for an unknown item without throwing", () => {
    const [event] = log({
      type: "item_delta",
      item_id: "ghost",
      delta: { kind: "text", text: "boo" },
    });
    const result = applyEvent(emptySessionView, event);
    expect(result.outcome).toBe("applied");
    expect(result.view.transcript).toEqual([]);
    expect(result.view.seq).toBe(1);
  });

  it("ignores a delta that does not fit its item", () => {
    const view = fold(
      log(
        { type: "item_started", item: textItem("i1", "text") },
        { type: "item_delta", item_id: "i1", delta: { kind: "output", text: "nope" } },
      ),
    );
    expect(items(view)).toEqual([textItem("i1", "text")]);
  });

  it("ignores unknown event types and delta kinds, and keeps their place in the sequence", () => {
    const [future, futureDelta, known] = log(
      { type: "user_message", text: "placeholder" },
      { type: "user_message", text: "placeholder" },
      { type: "user_message", text: "still works" },
    );
    // What a newer server could send: shapes this build has no type for.
    const fromTheFuture = {
      ...future,
      body: { type: "pull_request_opened", url: "https://example.test/pr/1" },
    } as unknown as Event;
    const deltaFromTheFuture = {
      ...futureDelta,
      body: { type: "item_delta", item_id: "i1", delta: { kind: "image", url: "x" } },
    } as unknown as Event;
    const started = fold(log({ type: "item_started", item: textItem("i1", "kept") }));
    const shifted = [fromTheFuture, deltaFromTheFuture, known].map((event) => ({
      ...event,
      seq: event.seq + 1,
    }));

    const view = fold(shifted, started);
    expect(view.seq).toBe(4);
    expect(view.transcript.map((entry) => entry.kind)).toEqual(["item", "user"]);
    expect(items(view)).toEqual([textItem("i1", "kept")]);
  });

  it("does not throw on an event with no body", () => {
    const [event] = log({ type: "user_message", text: "x" });
    const broken = { ...event, body: undefined } as unknown as Event;
    expect(applyEvent(emptySessionView, broken)).toMatchObject({ outcome: "applied" });
  });

  it("folds a realistic two-turn session into the expected transcript", () => {
    const events = log(
      [null, { type: "session_status", status: "queued", reason: null }],
      [null, { type: "user_message", text: "List the files, then summarize." }],
      [null, { type: "session_status", status: "preparing", reason: null }],
      [null, { type: "agent_spawned", agent }],
      [null, { type: "session_status", status: "running", reason: null }],
      { type: "agent_status", status: "running" },
      // Turn 1: reasoning, a plan, a tool call, an answer.
      {
        type: "item_started",
        item: { id: "r1", status: "running", content: { kind: "reasoning", text: "" } },
      },
      { type: "item_delta", item_id: "r1", delta: { kind: "reasoning", text: "I should " } },
      { type: "item_delta", item_id: "r1", delta: { kind: "reasoning", text: "look first." } },
      {
        type: "item_completed",
        item: {
          id: "r1",
          status: "completed",
          content: { kind: "reasoning", text: "I should look first." },
        },
      },
      {
        type: "plan_updated",
        entries: [
          { content: "List files", status: "in_progress" },
          { content: "Summarize", status: "pending" },
        ],
      },
      { type: "item_started", item: toolItem("t1", null) },
      { type: "item_delta", item_id: "t1", delta: { kind: "output", text: "README.md\n" } },
      { type: "item_delta", item_id: "t1", delta: { kind: "output", text: "src\n" } },
      { type: "item_completed", item: toolItem("t1", "README.md\nsrc\n", "completed") },
      {
        type: "plan_updated",
        entries: [
          { content: "List files", status: "completed" },
          { content: "Summarize", status: "in_progress" },
        ],
      },
      { type: "item_started", item: textItem("m1", "") },
      { type: "item_delta", item_id: "m1", delta: { kind: "text", text: "There are " } },
      { type: "item_delta", item_id: "m1", delta: { kind: "text", text: "two entries." } },
      { type: "item_completed", item: textItem("m1", "There are two entries.", "completed") },
      { type: "usage", cost_usd: 0.012, tokens_in: 1200, tokens_out: 80 },
      { type: "agent_status", status: "idle" },
      [null, { type: "session_status", status: "review", reason: null }],
      // Turn 2: the user follows up, a tool is declined, the agent errors, then answers.
      [null, { type: "user_message", text: "Now delete src." }],
      [null, { type: "session_status", status: "running", reason: null }],
      { type: "agent_status", status: "running" },
      { type: "item_started", item: toolItem("t2", null) },
      { type: "item_completed", item: toolItem("t2", null, "declined") },
      { type: "error", message: "The tool call was refused by policy." },
      { type: "item_started", item: textItem("m2", "") },
      { type: "item_delta", item_id: "m2", delta: { kind: "text", text: "I was not allowed to." } },
      { type: "item_completed", item: textItem("m2", "I was not allowed to.", "completed") },
      {
        type: "plan_updated",
        entries: [
          { content: "List files", status: "completed" },
          { content: "Summarize", status: "completed" },
        ],
      },
      { type: "usage", cost_usd: 0.02, tokens_in: 2100, tokens_out: 140 },
      { type: "agent_status", status: "idle" },
      [null, { type: "session_status", status: "review", reason: null }],
    );

    const view = fold(events);

    const expected: TranscriptEntry[] = [
      { kind: "user", key: "event:e2", ts: 1001, text: "List the files, then summarize." },
      {
        kind: "item",
        key: "item:r1",
        ts: 1006,
        agentId: AGENT,
        item: {
          id: "r1",
          status: "completed",
          content: { kind: "reasoning", text: "I should look first." },
        },
        interrupted: false,
      },
      {
        kind: "item",
        key: "item:t1",
        ts: 1011,
        agentId: AGENT,
        item: toolItem("t1", "README.md\nsrc\n", "completed"),
        interrupted: false,
      },
      {
        kind: "item",
        key: "item:m1",
        ts: 1016,
        agentId: AGENT,
        item: textItem("m1", "There are two entries.", "completed"),
        interrupted: false,
      },
      { kind: "user", key: "event:e24", ts: 1023, text: "Now delete src." },
      {
        kind: "item",
        key: "item:t2",
        ts: 1026,
        agentId: AGENT,
        item: toolItem("t2", null, "declined"),
        interrupted: false,
      },
      {
        kind: "error",
        key: "event:e29",
        ts: 1028,
        agentId: AGENT,
        message: "The tool call was refused by policy.",
      },
      {
        kind: "item",
        key: "item:m2",
        ts: 1029,
        agentId: AGENT,
        item: textItem("m2", "I was not allowed to.", "completed"),
        interrupted: false,
      },
    ];
    expect(view.transcript).toEqual(expected);
    expect(view.seq).toBe(events.length);
    expect(view.status).toBe("review");
    expect(view.plan.every((entry) => entry.status === "completed")).toBe(true);
    expect(view.agents).toEqual([
      { ...agent, status: "idle", cost_usd: 0.02, tokens_in: 2100, tokens_out: 140 },
    ]);
    expect(view.usage).toEqual({ cost_usd: 0.02, tokens_in: 2100, tokens_out: 140 });

    // Replaying the whole log over the result changes nothing: every event is a duplicate.
    expect(events.every((event) => applyEvent(view, event).outcome === "duplicate")).toBe(true);
    // Folding the log in two halves, as a reconnect does, gives the same view.
    expect(fold(events.slice(20), fold(events.slice(0, 20)))).toEqual(view);
  });
});

describe("repo set", () => {
  it("collects attached repos with the primary one first", () => {
    const view = fold(
      log(
        [null, { type: "repo_attached", repo: makeSessionRepo("web") }],
        [null, { type: "repo_attached", repo: makeSessionRepo("api", { primary: true }) }],
        [null, { type: "repo_attached", repo: makeSessionRepo("docs") }],
      ),
    );
    expect(view.repos.map((repo) => repo.name)).toEqual(["api", "web", "docs"]);
    // The repo set is not a transcript matter.
    expect(view.transcript).toEqual([]);
  });

  it("updates a repo that is attached again, in place", () => {
    const view = fold(
      log(
        [null, { type: "repo_attached", repo: makeSessionRepo("api", { primary: true }) }],
        [null, { type: "repo_attached", repo: makeSessionRepo("web") }],
        [
          null,
          {
            type: "repo_attached",
            repo: makeSessionRepo("api", { primary: true, branch: "calin/fix-login" }),
          },
        ],
      ),
    );
    expect(view.repos).toEqual([
      makeSessionRepo("api", { primary: true, branch: "calin/fix-login" }),
      makeSessionRepo("web"),
    ]);
  });

  it("seeds from the REST record and lets events update it", () => {
    const seed = [makeSessionRepo("api", { primary: true }), makeSessionRepo("web")];
    expect(mergeRepos(seed, [])).toBe(seed);

    const attached = [
      makeSessionRepo("web", { branch: "calin/fix-login" }),
      makeSessionRepo("docs", { mode: "live" }),
    ];
    expect(mergeRepos(seed, attached)).toEqual([
      makeSessionRepo("api", { primary: true }),
      makeSessionRepo("web", { branch: "calin/fix-login" }),
      makeSessionRepo("docs", { mode: "live" }),
    ]);
    // The seed is a signal's value: it must not change under its readers.
    expect(seed).toEqual([makeSessionRepo("api", { primary: true }), makeSessionRepo("web")]);
  });
});

describe("permissions", () => {
  it("puts a request in the transcript where it was asked, pending", () => {
    const view = fold(
      log(
        { type: "item_started", item: textItem("m1", "Let me run the tests.", "completed") },
        { type: "permission_requested", request: request("p1") },
        { type: "item_started", item: textItem("m2", "Meanwhile…") },
      ),
    );
    expect(kinds(view)).toEqual(["item", "permission", "item"]);
    expect(view.transcript[1]).toEqual({
      kind: "permission",
      key: "permission:p1",
      ts: 1001,
      agentId: AGENT,
      request: request("p1"),
      outcome: { state: "pending" },
    });
    expect(view.pendingPermissions).toEqual([
      { request: request("p1"), agentId: AGENT, ts: 1001, seq: 2 },
    ]);
  });

  it("lists every pending request of the session, oldest first", () => {
    const view = fold(
      log(
        ["a1", { type: "permission_requested", request: request("p1") }],
        ["a2", { type: "permission_requested", request: request("p2") }],
        ["a1", { type: "permission_requested", request: request("p3") }],
        [
          "a2",
          {
            type: "permission_resolved",
            request_id: "p2",
            decision: "allow",
            by: "user",
            message: null,
          },
        ],
      ),
    );
    expect(view.pendingPermissions.map((one) => one.request.id)).toEqual(["p1", "p3"]);
    // The list is exactly the transcript's pending rows.
    const pendingRows = view.transcript.flatMap((entry) =>
      entry.kind === "permission" && entry.outcome.state === "pending" ? [entry.request.id] : [],
    );
    expect(pendingRows).toEqual(["p1", "p3"]);
  });

  it("records who allowed or denied a request, and why, in place", () => {
    const view = fold(
      log(
        { type: "permission_requested", request: request("p1") },
        { type: "permission_requested", request: request("p2") },
        { type: "permission_requested", request: request("p3") },
        {
          type: "permission_resolved",
          request_id: "p1",
          decision: "allow",
          by: "user",
          message: null,
        },
        {
          type: "permission_resolved",
          request_id: "p2",
          decision: "deny",
          by: "user",
          message: "Not on main.",
        },
        {
          type: "permission_resolved",
          request_id: "p3",
          decision: "allow",
          by: "policy",
          message: null,
        },
      ),
    );
    const outcomes = view.transcript.map((entry) =>
      entry.kind === "permission" ? entry.outcome : undefined,
    );
    expect(outcomes).toEqual([
      { state: "resolved", decision: "allow", by: "user", message: null },
      { state: "resolved", decision: "deny", by: "user", message: "Not on main." },
      { state: "resolved", decision: "allow", by: "policy", message: null },
    ]);
    expect(view.transcript.map((entry) => entry.key)).toEqual([
      "permission:p1",
      "permission:p2",
      "permission:p3",
    ]);
    expect(view.pendingPermissions).toEqual([]);
  });

  it("ignores an answer to a request it never saw", () => {
    const before = fold(log({ type: "permission_requested", request: request("p1") }));
    const [, answer] = log(
      { type: "user_message", text: "placeholder" },
      {
        type: "permission_resolved",
        request_id: "ghost",
        decision: "deny",
        by: "policy",
        message: null,
      },
    );
    const after = applyEvent(before, answer).view;
    expect(after.transcript).toBe(before.transcript);
    expect(after.pendingPermissions).toBe(before.pendingPermissions);
    expect(after.seq).toBe(2);
  });

  it.each(["review", "failed", "cancelled", "done"] as const)(
    "lets unanswered requests lapse when the session goes to %s",
    (ended) => {
      const view = fold(
        log(
          status("waiting"),
          { type: "permission_requested", request: request("p1") },
          status(ended),
        ),
      );
      expect(view.pendingPermissions).toEqual([]);
      expect(view.transcript[0]).toMatchObject({
        kind: "permission",
        outcome: { state: "lapsed", message: null },
      });
    },
  );

  const closedByServer: EventBody = {
    type: "permission_resolved",
    request_id: "p1",
    decision: "deny",
    by: "policy",
    message: "The turn ended.",
  };

  it("reads the server closing a waiting request as no longer needed, not as a refusal", () => {
    const view = fold(
      log(
        status("running"),
        { type: "permission_requested", request: request("p1") },
        status("waiting"),
        closedByServer,
        status("review"),
      ),
    );
    expect(view.transcript[0]).toMatchObject({
      kind: "permission",
      outcome: { state: "lapsed", message: "The turn ended." },
    });
    expect(view.pendingPermissions).toEqual([]);
  });

  it("reads it the same way when the closure arrives after the turn is over", () => {
    const view = fold(
      log(
        status("waiting"),
        { type: "permission_requested", request: request("p1") },
        status("cancelled"),
        closedByServer,
      ),
    );
    expect(view.transcript[0]).toMatchObject({
      kind: "permission",
      outcome: { state: "lapsed", message: "The turn ended." },
    });
  });

  it("still records a refusal by policy that nobody was asked about", () => {
    const view = fold(
      log(
        status("running"),
        { type: "permission_requested", request: request("p1") },
        { ...closedByServer, message: "Edits are not allowed in a read-only repo." },
      ),
    );
    expect(view.transcript[0]).toMatchObject({
      kind: "permission",
      outcome: { state: "resolved", decision: "deny", by: "policy" },
    });
  });

  it("keeps a denial by the reader a denial, even while the session waits", () => {
    const view = fold(
      log(
        status("waiting"),
        { type: "permission_requested", request: request("p1") },
        { ...closedByServer, by: "user", message: "No." },
      ),
    );
    expect(view.transcript[0]).toMatchObject({
      kind: "permission",
      outcome: { state: "resolved", decision: "deny", by: "user", message: "No." },
    });
  });

  it("keeps requests pending through running and waiting", () => {
    const view = fold(
      log(
        status("running"),
        { type: "permission_requested", request: request("p1") },
        status("waiting"),
        status("running"),
      ),
    );
    expect(view.pendingPermissions.map((one) => one.request.id)).toEqual(["p1"]);
  });
});

describe("interrupted items", () => {
  const interrupted = (view: SessionView): Array<[string, boolean]> =>
    view.transcript.flatMap((entry): Array<[string, boolean]> =>
      entry.kind === "item" ? [[entry.item.id, entry.interrupted]] : [],
    );

  it.each(["review", "failed", "cancelled"] as const)(
    "marks what was still running when the session goes to %s",
    (ended) => {
      const view = fold(
        log(
          status("running"),
          { type: "item_started", item: toolItem("t1", "ok", "completed") },
          { type: "item_started", item: toolItem("t2", null) },
          { type: "item_started", item: textItem("m1", "Half a sen") },
          status(ended),
        ),
      );
      expect(interrupted(view)).toEqual([
        ["t1", false],
        ["t2", true],
        ["m1", true],
      ]);
      // The item itself is untouched: what the wire said stays what the wire said.
      expect(items(view)[1]).toEqual(toolItem("t2", null));
    },
  );

  it("does not mark items while the session is running or waiting", () => {
    const view = fold(
      log(
        status("running"),
        { type: "item_started", item: toolItem("t1", null) },
        status("waiting"),
        status("running"),
      ),
    );
    expect(interrupted(view)).toEqual([["t1", false]]);
  });

  it("stays interrupted when the session continues, until the item itself reports", () => {
    const stopped = fold(
      log(
        status("running"),
        { type: "item_started", item: toolItem("t1", null) },
        status("cancelled"),
        [null, { type: "user_message", text: "carry on" }],
        status("running"),
        { type: "item_started", item: toolItem("t2", null) },
      ),
    );
    expect(interrupted(stopped)).toEqual([
      ["t1", true],
      ["t2", false],
    ]);

    const [late] = log({ type: "item_completed", item: toolItem("t1", "done", "completed") });
    const finished = applyEvent(stopped, { ...late, seq: stopped.seq + 1 }).view;
    expect(interrupted(finished)).toEqual([
      ["t1", false],
      ["t2", false],
    ]);
  });
});

describe("changed files", () => {
  const change = (
    path: string,
    op: "create" | "edit" | "delete" | "rename",
    sessionRepoId: string | null = "sr1",
  ): EventBody => ({ type: "file_changed", session_repo_id: sessionRepoId, path, op });

  it("keeps one entry per file with its last operation, and counts every change", () => {
    const view = fold(
      log(
        change("src/main.rs", "create"),
        change("README.md", "edit"),
        change("src/main.rs", "edit"),
        change("README.md", "edit", "sr2"),
        change("README.md", "delete"),
      ),
    );
    expect(view.changedFiles.map((file) => [file.sessionRepoId, file.path, file.op])).toEqual([
      ["sr1", "src/main.rs", "edit"],
      ["sr1", "README.md", "delete"],
      ["sr2", "README.md", "edit"],
    ]);
    expect(view.changesRevision).toBe(5);
    // File changes are for the Changes panel, not the transcript.
    expect(view.transcript).toEqual([]);
  });

  it("goes up by exactly one per change, and not at all for other events", () => {
    const events = log(
      change("a.txt", "create"),
      { type: "user_message", text: "hi" },
      change("a.txt", "edit"),
      change("a.txt", "edit"),
    );
    const revisions: number[] = [];
    let view = emptySessionView;
    for (const event of events) {
      view = applyEvent(view, event).view;
      revisions.push(view.changesRevision);
    }
    expect(revisions).toEqual([1, 1, 2, 3]);
  });

  it("tells the agent's changes from the reader's own saves", () => {
    const view = fold(
      log(
        ["a1", change("by-agent.ts", "edit")],
        [null, change("by-you.ts", "edit")],
        ["a1", change("/tmp/scratch.txt", "create", null)],
      ),
    );
    expect(view.changedFiles).toEqual([
      {
        key: "sr1\nby-agent.ts",
        sessionRepoId: "sr1",
        path: "by-agent.ts",
        op: "edit",
        by: "agent",
        agentId: "a1",
        ts: 1000,
      },
      {
        key: "sr1\nby-you.ts",
        sessionRepoId: "sr1",
        path: "by-you.ts",
        op: "edit",
        by: "user",
        agentId: null,
        ts: 1001,
      },
      {
        key: "\n/tmp/scratch.txt",
        sessionRepoId: null,
        path: "/tmp/scratch.txt",
        op: "create",
        by: "agent",
        agentId: "a1",
        ts: 1002,
      },
    ]);
  });
});
