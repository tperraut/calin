import type { Agent } from "../gen/Agent";
import type { Event } from "../gen/Event";
import type { EventBody } from "../gen/EventBody";
import type { Item } from "../gen/Item";
import type { ItemDelta } from "../gen/ItemDelta";
import type { PlanEntry } from "../gen/PlanEntry";
import type { SessionStatus } from "../gen/SessionStatus";

/** Running totals, in the shape of the `usage` event. */
export type Usage = Omit<Extract<EventBody, { type: "usage" }>, "type">;

/** One row of the transcript. `key` is stable for the life of the row. */
export type TranscriptEntry =
  | { kind: "user"; key: string; ts: number; text: string }
  | { kind: "item"; key: string; ts: number; agentId: string | null; item: Item }
  | { kind: "error"; key: string; ts: number; agentId: string | null; message: string };

/** Everything the session screen shows, as a fold of the session's event log. */
export type SessionView = {
  /** `seq` of the last event applied; 0 before the first one. */
  readonly seq: number;
  readonly status: SessionStatus | null;
  readonly statusReason: string | null;
  readonly agents: readonly Agent[];
  /** Sum of the latest totals of every agent that reported usage. */
  readonly usage: Usage | null;
  readonly usageByAgent: Readonly<Record<string, Usage>>;
  readonly transcript: readonly TranscriptEntry[];
  readonly plan: readonly PlanEntry[];
};

export const emptySessionView: SessionView = {
  seq: 0,
  status: null,
  statusReason: null,
  agents: [],
  usage: null,
  usageByAgent: {},
  transcript: [],
  plan: [],
};

/**
 * `duplicate`: the event was already applied and changed nothing.
 * `gap`: events are missing before this one; the caller resubscribes from `view.seq`.
 */
export type ApplyOutcome = "applied" | "duplicate" | "gap";
export type ApplyResult = { view: SessionView; outcome: ApplyOutcome };

const itemKey = (itemId: string): string => `item:${itemId}`;

function withEntry(
  transcript: readonly TranscriptEntry[],
  index: number,
  entry: TranscriptEntry,
): TranscriptEntry[] {
  const next = transcript.slice();
  next[index] = entry;
  return next;
}

/** Deltas target recent items, so the search starts from the end. */
function findItem(transcript: readonly TranscriptEntry[], itemId: string): number {
  const key = itemKey(itemId);
  return transcript.findLastIndex((entry) => entry.kind === "item" && entry.key === key);
}

function putItem(view: SessionView, event: Event, item: Item): SessionView {
  const index = findItem(view.transcript, item.id);
  const existing = index === -1 ? undefined : view.transcript[index];
  const entry: TranscriptEntry = {
    kind: "item",
    key: itemKey(item.id),
    // A completed item keeps the place and time at which it started.
    ts: existing?.ts ?? event.ts,
    agentId: event.agent_id,
    item,
  };
  const transcript =
    index === -1 ? [...view.transcript, entry] : withEntry(view.transcript, index, entry);
  return { ...view, transcript };
}

function appendDelta(item: Item, delta: ItemDelta): Item | undefined {
  const { content } = item;
  if (delta.kind === "text" && content.kind === "text") {
    return { ...item, content: { ...content, text: content.text + delta.text } };
  }
  if (delta.kind === "reasoning" && content.kind === "reasoning") {
    return { ...item, content: { ...content, text: content.text + delta.text } };
  }
  if (delta.kind === "output" && content.kind === "tool") {
    return { ...item, content: { ...content, output: (content.output ?? "") + delta.text } };
  }
  // A delta that does not fit its item, or of a kind this build does not know.
  return undefined;
}

function applyDelta(view: SessionView, itemId: string, delta: ItemDelta): SessionView {
  const index = findItem(view.transcript, itemId);
  const entry = index === -1 ? undefined : view.transcript[index];
  if (entry?.kind !== "item") return view;
  const item = appendDelta(entry.item, delta);
  if (!item) return view;
  return { ...view, transcript: withEntry(view.transcript, index, { ...entry, item }) };
}

function patchAgent(view: SessionView, agentId: string | null, patch: Partial<Agent>): SessionView {
  if (agentId === null || !view.agents.some((agent) => agent.id === agentId)) return view;
  return {
    ...view,
    agents: view.agents.map((agent) => (agent.id === agentId ? { ...agent, ...patch } : agent)),
  };
}

function applyUsage(view: SessionView, agentId: string | null, usage: Usage): SessionView {
  // Totals are kept even for an agent that was never announced, so cost is not lost.
  const usageByAgent = { ...view.usageByAgent, [agentId ?? ""]: usage };
  const total: Usage = { cost_usd: 0, tokens_in: 0, tokens_out: 0 };
  for (const one of Object.values(usageByAgent)) {
    total.cost_usd += one.cost_usd;
    total.tokens_in += one.tokens_in;
    total.tokens_out += one.tokens_out;
  }
  return { ...patchAgent(view, agentId, usage), usage: total, usageByAgent };
}

function applyBody(view: SessionView, event: Event): SessionView {
  // Widened on purpose: a frame without a body must not throw in the fold.
  const body = event.body as EventBody | null | undefined;
  if (!body) return view;
  switch (body.type) {
    case "session_status":
      return { ...view, status: body.status, statusReason: body.reason };
    case "user_message": {
      const entry: TranscriptEntry = {
        kind: "user",
        key: `event:${event.id}`,
        ts: event.ts,
        text: body.text,
      };
      return { ...view, transcript: [...view.transcript, entry] };
    }
    case "agent_spawned": {
      const others = view.agents.filter((agent) => agent.id !== body.agent.id);
      return { ...view, agents: [...others, body.agent] };
    }
    case "agent_status":
      return patchAgent(view, event.agent_id, { status: body.status });
    case "item_started":
    case "item_completed":
      return putItem(view, event, body.item);
    case "item_delta":
      return applyDelta(view, body.item_id, body.delta);
    case "plan_updated":
      return { ...view, plan: body.entries };
    case "usage":
      return applyUsage(view, event.agent_id, {
        cost_usd: body.cost_usd,
        tokens_in: body.tokens_in,
        tokens_out: body.tokens_out,
      });
    case "error": {
      const entry: TranscriptEntry = {
        kind: "error",
        key: `event:${event.id}`,
        ts: event.ts,
        agentId: event.agent_id,
        message: body.message,
      };
      return { ...view, transcript: [...view.transcript, entry] };
    }
    default:
      // An event type from a newer server: it still takes its place in the sequence.
      return view;
  }
}

/**
 * Folds one event into the view. Pure: `view` is never mutated.
 *
 * Only the event with `seq === view.seq + 1` is applied (ws.rs: delivered in
 * `seq` order, never twice); anything else is reported and leaves `view` as is.
 */
export function applyEvent(view: SessionView, event: Event): ApplyResult {
  if (event.seq <= view.seq) return { view, outcome: "duplicate" };
  if (event.seq > view.seq + 1) return { view, outcome: "gap" };
  return { view: { ...applyBody(view, event), seq: event.seq }, outcome: "applied" };
}

/** Folds events in order and stops at the first gap. */
export function applyEvents(view: SessionView, events: readonly Event[]): ApplyResult {
  let current = view;
  for (const event of events) {
    const result = applyEvent(current, event);
    if (result.outcome === "gap") return { view: current, outcome: "gap" };
    current = result.view;
  }
  return { view: current, outcome: "applied" };
}
