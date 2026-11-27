import { describe, expect, it } from "vite-plus/test";
import type { Agent } from "../gen/Agent";
import type { Event } from "../gen/Event";
import type { EventBody } from "../gen/EventBody";
import type { Item } from "../gen/Item";
import type { PermissionRequest } from "../gen/PermissionRequest";
import type { PullRequest } from "../gen/PullRequest";
import type { SessionStatus } from "../gen/SessionStatus";
import { makePullRequest, makeSessionRepo } from "../test/fakes";
import { makeMessage, makeSubagent } from "../test/orchestration";
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

  it.each(["review", "shipping", "failed", "cancelled", "done"] as const)(
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

  const expired: EventBody = {
    type: "permission_resolved",
    request_id: "p1",
    decision: "deny",
    by: "expired",
    message: "The turn ended.",
  };

  it("reads an expired request as no longer needed, not as a refusal", () => {
    const view = fold(
      log(
        status("running"),
        { type: "permission_requested", request: request("p1") },
        status("waiting"),
        expired,
        status("review"),
      ),
    );
    expect(view.transcript[0]).toMatchObject({
      kind: "permission",
      outcome: { state: "lapsed", message: "The turn ended." },
    });
    expect(view.pendingPermissions).toEqual([]);
  });

  it("reads it the same way when the expiry arrives after the turn is over", () => {
    const view = fold(
      log(
        status("waiting"),
        { type: "permission_requested", request: request("p1") },
        status("cancelled"),
        expired,
      ),
    );
    expect(view.transcript[0]).toMatchObject({
      kind: "permission",
      outcome: { state: "lapsed", message: "The turn ended." },
    });
  });

  it("takes a denial by policy at its word, whatever the session is doing", () => {
    // `policy` only ever means the permission mode answered: no guessing from the status.
    const denied: EventBody = { ...expired, by: "policy", message: "Edits are denied here." };
    for (const before of ["running", "waiting"] as const) {
      const view = fold(
        log(status(before), { type: "permission_requested", request: request("p1") }, denied),
      );
      expect(view.transcript[0]).toMatchObject({
        kind: "permission",
        outcome: {
          state: "resolved",
          decision: "deny",
          by: "policy",
          message: "Edits are denied here.",
        },
      });
    }
  });

  it("keeps a denial by the reader a denial, even while the session waits", () => {
    const view = fold(
      log(
        status("waiting"),
        { type: "permission_requested", request: request("p1") },
        { ...expired, by: "user", message: "No." },
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
        revision: 1,
      },
      {
        key: "sr1\nby-you.ts",
        sessionRepoId: "sr1",
        path: "by-you.ts",
        op: "edit",
        by: "user",
        agentId: null,
        ts: 1001,
        revision: 2,
      },
      {
        key: "\n/tmp/scratch.txt",
        sessionRepoId: null,
        path: "/tmp/scratch.txt",
        op: "create",
        by: "agent",
        agentId: "a1",
        ts: 1002,
        revision: 3,
      },
    ]);
  });
});

describe("a permission request that names its tool call", () => {
  const asks = (view: SessionView, itemId: string) => {
    const entry = view.transcript.find((one) => one.key === `item:${itemId}`);
    return entry?.kind === "item" ? entry.permissions : undefined;
  };
  const started = { type: "item_started", item: toolItem("t1", null) } as const;
  const asked = {
    type: "permission_requested",
    request: request("p1", { item_id: "t1" }),
  } as const;
  const answer = (
    by: "user" | "policy" | "expired",
    decision: "allow" | "deny",
    message: string | null = null,
  ): EventBody => ({ type: "permission_resolved", request_id: "p1", decision, by, message });

  it("goes into that call's card instead of a row of its own, and still waits in the bar", () => {
    const view = fold(log(started, asked));
    expect(kinds(view)).toEqual(["item"]);
    expect(asks(view, "t1")).toEqual([
      { request: request("p1", { item_id: "t1" }), outcome: { state: "pending" }, ts: 1001 },
    ]);
    expect(view.pendingPermissions.map((one) => one.request.id)).toEqual(["p1"]);
  });

  it.each([
    [
      "allowed by the reader",
      answer("user", "allow"),
      { state: "resolved", decision: "allow", by: "user", message: null },
    ],
    [
      "denied by the reader, with the reason",
      answer("user", "deny", "Not on main."),
      { state: "resolved", decision: "deny", by: "user", message: "Not on main." },
    ],
    [
      "allowed by policy",
      answer("policy", "allow"),
      { state: "resolved", decision: "allow", by: "policy", message: null },
    ],
    [
      "expired",
      answer("expired", "deny", "the turn ended"),
      { state: "lapsed", message: "the turn ended" },
    ],
  ])("records the outcome in the card: %s", (_name, resolution, outcome) => {
    const view = fold(log(started, asked, resolution));
    expect(kinds(view)).toEqual(["item"]);
    expect(asks(view, "t1")?.map((one) => one.outcome)).toEqual([outcome]);
    expect(view.pendingPermissions).toEqual([]);
  });

  it("keeps the outcome when the call then completes, and through its output", () => {
    const view = fold(
      log(
        started,
        asked,
        answer("user", "allow"),
        { type: "item_delta", item_id: "t1", delta: { kind: "output", text: "ok\n" } },
        { type: "item_completed", item: toolItem("t1", "ok\n", "completed") },
      ),
    );
    expect(items(view)).toEqual([toolItem("t1", "ok\n", "completed")]);
    expect(asks(view, "t1")?.map((one) => one.outcome.state)).toEqual(["resolved"]);
  });

  it("lapses in the card when the turn ends unanswered, and takes the server's note after", () => {
    const ended = fold(log(started, asked, status("review")));
    expect(asks(ended, "t1")?.map((one) => one.outcome)).toEqual([
      { state: "lapsed", message: null },
    ]);
    expect(ended.pendingPermissions).toEqual([]);
    expect(ended.transcript[0]).toMatchObject({ interrupted: true });

    const noted = fold(log(started, asked, status("review"), answer("expired", "deny", "stopped")));
    expect(asks(noted, "t1")?.map((one) => one.outcome)).toEqual([
      { state: "lapsed", message: "stopped" },
    ]);
  });

  it("keeps a row of its own when it names no call, a call it never saw, or one that is no tool", () => {
    const view = fold(
      log(
        { type: "item_started", item: textItem("m1", "Hello") },
        { type: "permission_requested", request: request("p1") },
        { type: "permission_requested", request: request("p2", { item_id: "ghost" }) },
        { type: "permission_requested", request: request("p3", { item_id: "m1" }) },
      ),
    );
    expect(kinds(view)).toEqual(["item", "permission", "permission", "permission"]);
    expect(asks(view, "m1")).toBeUndefined();
  });

  it("moves into the card when its call arrives after it", () => {
    const early = log(asked, started);
    const view = fold(early);
    expect(kinds(view)).toEqual(["item"]);
    expect(asks(view, "t1")).toEqual([
      { request: request("p1", { item_id: "t1" }), outcome: { state: "pending" }, ts: 1000 },
    ]);
    const answered = fold(log(asked, started, answer("user", "deny", "No.")));
    expect(asks(answered, "t1")?.map((one) => one.outcome.state)).toEqual(["resolved"]);
  });

  it("holds several requests for one call, each with its own outcome, and replaces a repeat", () => {
    const second = {
      type: "permission_requested",
      request: request("p2", { item_id: "t1" }),
    } as const;
    const view = fold(log(started, asked, second, asked, answer("user", "allow")));
    expect(asks(view, "t1")?.map((one) => [one.request.id, one.outcome.state])).toEqual([
      ["p2", "pending"],
      ["p1", "resolved"],
    ]);
    expect(view.pendingPermissions.map((one) => one.request.id)).toEqual(["p2"]);
  });

  it("does not mutate the view it is given", () => {
    const before = fold(log(started, asked));
    const snapshot = structuredClone(before);
    applyEvent(before, {
      ...log(answer("user", "allow"))[0],
      seq: before.seq + 1,
    });
    expect(before).toEqual(snapshot);
  });
});

describe("a repo attached while the session is under way", () => {
  const attach = (name: string, agentId: string | null = null): [string | null, EventBody] => [
    agentId,
    { type: "repo_attached", repo: makeSessionRepo(name) },
  ];
  const repoRows = (view: SessionView) =>
    view.transcript.flatMap((entry) =>
      entry.kind === "repo" ? [[entry.repo.name, entry.by, entry.agentId]] : [],
    );

  it("is not a transcript row while the session is being prepared", () => {
    const view = fold(
      log(
        status("queued"),
        [null, { type: "user_message", text: "Fix it" }],
        status("preparing"),
        attach("api"),
        attach("web"),
      ),
    );
    expect(view.turnStarted).toBe(false);
    expect(repoRows(view)).toEqual([]);
    expect(view.repos.map((repo) => repo.name)).toEqual(["api", "web"]);
  });

  it("is a row once the first turn began, saying who attached it", () => {
    const view = fold(
      log(
        status("preparing"),
        attach("api"),
        status("running"),
        [null, { type: "agent_spawned", agent }],
        attach("web"),
        attach("docs", AGENT),
      ),
    );
    expect(view.turnStarted).toBe(true);
    expect(repoRows(view)).toEqual([
      ["web", "user", null],
      ["docs", "agent", AGENT],
    ]);
    expect(view.transcript.at(-1)).toEqual({
      kind: "repo",
      key: "event:e6",
      ts: 1005,
      repo: makeSessionRepo("docs"),
      by: "agent",
      agentId: AGENT,
    });
    expect(view.repos.map((repo) => repo.name)).toEqual(["api", "web", "docs"]);
  });

  it("stays a row through review and a later turn, and counts a first item as the turn having begun", () => {
    const later = fold(log(status("running"), status("review"), attach("web"), status("running")));
    expect(repoRows(later)).toEqual([["web", "user", null]]);

    const fromItem = fold(
      log({ type: "item_started", item: textItem("m1", "Hi") }, attach("web", AGENT)),
    );
    expect(repoRows(fromItem)).toEqual([["web", "agent", AGENT]]);
  });

  it("is not news when the session already has the repo", () => {
    const view = fold(
      log(status("preparing"), attach("api"), status("running"), attach("web"), [
        null,
        { type: "repo_attached", repo: makeSessionRepo("api", { branch: "calin/fix" }) },
      ]),
    );
    expect(repoRows(view)).toEqual([["web", "user", null]]);
    expect(view.repos.find((repo) => repo.name === "api")?.branch).toBe("calin/fix");
  });
});

describe("pull requests", () => {
  const opened = (repo: string, number: number, patch: Partial<PullRequest> = {}): EventBody => ({
    type: "pull_request_opened",
    pull_request: makePullRequest(repo, number, patch),
  });
  const updated = (repo: string, number: number, patch: Partial<PullRequest> = {}): EventBody => ({
    type: "pull_request_updated",
    pull_request: makePullRequest(repo, number, patch),
  });
  const rows = (view: SessionView) =>
    view.transcript.flatMap((entry) =>
      entry.kind === "pull_request"
        ? [`#${entry.pullRequest.number} ${entry.what} (${entry.pullRequest.session_repo_id})`]
        : [],
    );

  it("starts with none", () => {
    expect(emptySessionView.pullRequests).toEqual([]);
  });

  it("inserts the pull request of each repo as it is opened, in the order they came", () => {
    const view = fold(log(status("review"), opened("web", 7), opened("api", 12)));
    expect(view.pullRequests).toEqual([makePullRequest("web", 7), makePullRequest("api", 12)]);
  });

  it("replaces a repo's pull request with each update, always the full value, and keeps its place", () => {
    const view = fold(
      log(
        opened("api", 12, { state: "draft" }),
        opened("web", 7, { state: "draft" }),
        updated("api", 12, { state: "open", checks: "pending", updated_at: 2000 }),
        updated("api", 12, { state: "open", checks: "failing", updated_at: 3000 }),
      ),
    );
    expect(view.pullRequests).toEqual([
      makePullRequest("api", 12, { state: "open", checks: "failing", updated_at: 3000 }),
      makePullRequest("web", 7, { state: "draft" }),
    ]);
  });

  it("keeps one pull request per repo: another one opened for it takes its place", () => {
    const view = fold(log(opened("api", 12, { state: "closed" }), opened("api", 13)));
    expect(view.pullRequests).toEqual([makePullRequest("api", 13)]);
  });

  it("takes an update for a pull request it never saw opened", () => {
    const view = fold(log(updated("api", 12, { checks: "passing" })));
    expect(view.pullRequests).toEqual([makePullRequest("api", 12, { checks: "passing" })]);
    expect(rows(view)).toEqual([]);
  });

  it("says in the transcript when one is opened, and when it is merged or closed, and nothing else", () => {
    const view = fold(
      log(
        status("review"),
        opened("api", 12, { state: "draft" }),
        opened("web", 7, { state: "draft" }),
        status("shipping"),
        updated("api", 12, { state: "draft", checks: "pending" }),
        updated("api", 12, { state: "open", checks: "failing" }),
        updated("api", 12, { state: "open", checks: "passing", review: "approved" }),
        updated("api", 12, { state: "merged", checks: "passing", review: "approved" }),
        updated("web", 7, { state: "closed" }),
        status("done"),
      ),
    );
    expect(rows(view)).toEqual([
      "#12 opened (api)",
      "#7 opened (web)",
      "#12 merged (api)",
      "#7 closed (web)",
    ]);
    // Only these rows: a pull request event is never a message of its own.
    expect(view.transcript).toHaveLength(4);
    const [row] = view.transcript;
    expect(row).toMatchObject({
      kind: "pull_request",
      key: "pull_request:api:12:opened",
      ts: 1001,
      what: "opened",
      pullRequest: makePullRequest("api", 12, { state: "draft" }),
    });
  });

  it("does not say it twice when shipping again announces the same pull request, or an update repeats", () => {
    const first = log(
      opened("api", 12),
      updated("api", 12, { checks: "pending" }),
      // Shipping again: the open pull request of the branch is found, and announced.
      opened("api", 12, { checks: "pending" }),
      updated("api", 12, { state: "merged" }),
      updated("api", 12, { state: "merged", review: "approved" }),
    );
    const view = fold(first);
    expect(rows(view)).toEqual(["#12 opened (api)", "#12 merged (api)"]);

    // A replay delivers what was already applied: nothing is added, the view is the same one.
    const replayed = applyEvents(view, first);
    expect(replayed.view).toBe(view);
    expect(rows(replayed.view)).toEqual(["#12 opened (api)", "#12 merged (api)"]);
  });

  it("says both at once for a pull request found already merged", () => {
    expect(rows(fold(log(opened("api", 12, { state: "merged" }))))).toEqual([
      "#12 opened (api)",
      "#12 merged (api)",
    ]);
  });

  it("keeps a state, a check result or a review this build has no name for", () => {
    const strange = {
      ...makePullRequest("api", 12),
      state: "queued",
      checks: "skipped",
      review: "dismissed",
      merge_queue_position: 3,
    } as unknown as PullRequest;
    const view = fold(
      log(
        { type: "pull_request_opened", pull_request: strange },
        { type: "user_message", text: "x" },
      ),
    );
    expect(view.pullRequests).toEqual([strange]);
    expect(rows(view)).toEqual(["#12 opened (api)"]);
    expect(view.seq).toBe(2);
  });

  it("ignores a pull request event that names no pull request, and keeps its place in the sequence", () => {
    const [broken, next] = log({ type: "pull_request_updated" } as unknown as EventBody, {
      type: "user_message",
      text: "still works",
    });
    const view = fold([broken, next]);
    expect(view.pullRequests).toEqual([]);
    expect(view.seq).toBe(2);
    expect(view.transcript.map((entry) => entry.kind)).toEqual(["user"]);
  });

  it("does not mutate the view it is given", () => {
    const before = fold(log(opened("api", 12)));
    const frozen = structuredClone(before);
    const [, event] = log(
      { type: "user_message", text: "x" },
      updated("api", 12, { state: "merged" }),
    );
    applyEvent(before, event);
    expect(before).toEqual(frozen);
  });
});

describe("applyEvent: messages between agents", () => {
  const dispatch = makeMessage("m1", { kind: "dispatch", body: "Add /health" });
  const note = makeMessage("m2", {
    kind: "note",
    from_session_id: "s2",
    from_agent_id: "a2",
    to_session_id: "s1",
    to_agent_id: "a1",
  });

  it("shows a message sent from this session and one received in it, each on its side", () => {
    const view = fold(
      log({ type: "message_sent", message: dispatch }, [
        null,
        { type: "message_received", message: note },
      ]),
    );
    expect(view.transcript).toEqual([
      {
        kind: "message",
        key: "message:m1",
        ts: 1000,
        agentId: "a1",
        direction: "sent",
        message: dispatch,
      },
      {
        kind: "message",
        key: "message:m2",
        ts: 1001,
        agentId: "a1",
        direction: "received",
        message: note,
      },
    ]);
  });

  it("never shows the same message twice: a note to this session's user is sent and received here", () => {
    const toUser = makeMessage("m3", { to_session_id: "s1", to_agent_id: null, body: "Half way." });
    const view = fold(
      log(
        { type: "message_sent", message: toUser },
        [null, { type: "message_received", message: toUser }],
        { type: "message_sent", message: toUser },
      ),
    );
    expect(view.transcript).toHaveLength(1);
    expect(view.transcript[0]).toMatchObject({ kind: "message", direction: "sent", agentId: "a1" });
    expect(view.seq).toBe(3);
  });

  it("attributes a received message to the agent it is for, and none when it is for the user", () => {
    const fromUserElsewhere = makeMessage("m4", {
      from_agent_id: null,
      to_session_id: "s1",
      to_agent_id: null,
    });
    const view = fold(
      log(
        [null, { type: "message_received", message: note }],
        [null, { type: "message_received", message: fromUserElsewhere }],
      ),
    );
    expect(
      view.transcript.map((entry) => (entry.kind === "message" ? entry.agentId : "?")),
    ).toEqual(["a1", null]);
  });

  it("ignores an event that carries no message, and keeps its place in the sequence", () => {
    const view = fold(
      log({ type: "message_sent" } as unknown as EventBody, { type: "user_message", text: "x" }),
    );
    expect(kinds(view)).toEqual(["user"]);
    expect(view.seq).toBe(2);
  });
});

describe("applyEvent: questions", () => {
  const question = { id: "q1", text: "Header or footer?", options: ["Header", "Footer"] };

  it("keeps an asked question open and shows it where it was asked", () => {
    const view = fold(
      log(
        { type: "user_message", text: "go" },
        { type: "question_asked", question },
        status("waiting"),
      ),
    );
    expect(view.openQuestions).toEqual([{ question, agentId: "a1", ts: 1001, seq: 2 }]);
    expect(view.transcript[1]).toEqual({
      kind: "question",
      key: "question:q1",
      ts: 1001,
      agentId: "a1",
      question,
      outcome: { state: "pending" },
    });
    // Waiting on the user is not the end of a turn: the question stays open.
    expect(view.status).toBe("waiting");
  });

  it("closes the question with the user's answer", () => {
    const view = fold(
      log(
        { type: "question_asked", question },
        { type: "question_answered", question_id: "q1", answer: "Header", by: "user" },
      ),
    );
    expect(view.openQuestions).toEqual([]);
    expect(view.transcript).toHaveLength(1);
    expect(view.transcript[0]).toMatchObject({
      kind: "question",
      outcome: { state: "answered", answer: "Header", by: "user" },
    });
  });

  it("shows a question the session ended before as no longer needed, not as answered", () => {
    const view = fold(
      log(
        { type: "question_asked", question },
        { type: "question_answered", question_id: "q1", answer: "", by: "expired" },
      ),
    );
    expect(view.openQuestions).toEqual([]);
    expect(view.transcript[0]).toMatchObject({ kind: "question", outcome: { state: "expired" } });
  });

  it("keeps several questions open, oldest first, and closes the one that is answered", () => {
    const second = { id: "q2", text: "Which color?", options: [] };
    const view = fold(
      log(
        { type: "question_asked", question },
        { type: "question_asked", question: second },
        { type: "question_answered", question_id: "q1", answer: "Footer", by: "user" },
      ),
    );
    expect(view.openQuestions.map((one) => one.question.id)).toEqual(["q2"]);
    expect(kinds(view)).toEqual(["question", "question"]);
  });

  it("ignores an answer to a question the log never asked", () => {
    const view = fold(
      log({ type: "question_answered", question_id: "nope", answer: "x", by: "user" }),
    );
    expect(view.transcript).toEqual([]);
    expect(view.openQuestions).toEqual([]);
  });
});

describe("applyEvent: subagents", () => {
  const task = (id: string, state: Item["status"] = "running"): Item => ({
    id,
    status: state,
    content: {
      kind: "tool",
      name: "Task",
      tool_kind: "task",
      title: `task ${id}`,
      input: {},
      output: null,
      locations: [],
    },
  });
  const sub = (id: string, role: string): Agent => makeSubagent(id, role);

  it("adds a subagent to the session and attributes what it emits to it", () => {
    const view = fold(
      log(
        { type: "agent_spawned", agent },
        { type: "item_started", item: task("t1") },
        ["a1", { type: "agent_spawned", agent: sub("sub1", "explorer") }],
        ["sub1", { type: "item_started", item: toolItem("t1a", null) }],
        ["sub1", { type: "error", message: "rate limited" }],
        ["sub1", { type: "usage", cost_usd: 0.01, tokens_in: 10, tokens_out: 2 }],
        ["sub1", { type: "agent_status", status: "done" }],
      ),
    );
    expect(view.agents.map((one) => [one.id, one.kind, one.status, one.cost_usd])).toEqual([
      ["a1", "process", "starting", 0],
      ["sub1", "subagent", "done", 0.01],
    ]);
    expect(view.transcript.map((entry) => ("agentId" in entry ? entry.agentId : null))).toEqual([
      "a1",
      "sub1",
      "sub1",
    ]);
  });

  it("links a subagent to its parent's newest delegation call still running", () => {
    const view = fold(
      log(
        { type: "agent_spawned", agent },
        { type: "item_started", item: task("t0") },
        { type: "item_completed", item: task("t0", "completed") },
        { type: "item_started", item: task("t1") },
        ["a1", { type: "agent_spawned", agent: sub("sub1", "explorer") }],
        { type: "item_started", item: task("t2") },
        ["a1", { type: "agent_spawned", agent: sub("sub2", "reviewer") }],
        // A third one with no call left to claim.
        ["a1", { type: "agent_spawned", agent: sub("sub3", "tester") }],
      ),
    );
    expect(view.subagentCalls).toEqual({ sub1: "t1", sub2: "t2" });
  });

  it("prefers the call the spawn names as its cause", () => {
    const events = log(
      { type: "agent_spawned", agent },
      { type: "item_started", item: task("t1") },
      { type: "item_started", item: task("t2") },
      ["a1", { type: "agent_spawned", agent: sub("sub1", "explorer") }],
    );
    // `t1` started with the event `e2`.
    events[3] = { ...events[3], parent_id: "e2" };
    expect(fold(events).subagentCalls).toEqual({ sub1: "t1" });
  });

  it("keeps the link when the subagent is announced again", () => {
    const view = fold(
      log(
        { type: "agent_spawned", agent },
        { type: "item_started", item: task("t1") },
        ["a1", { type: "agent_spawned", agent: sub("sub1", "explorer") }],
        { type: "item_started", item: task("t2") },
        ["a1", { type: "agent_spawned", agent: { ...sub("sub1", "explorer"), status: "done" } }],
      ),
    );
    expect(view.subagentCalls).toEqual({ sub1: "t1" });
    expect(view.agents.find((one) => one.id === "sub1")?.status).toBe("done");
  });
});
