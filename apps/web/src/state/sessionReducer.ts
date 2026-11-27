import type { Agent } from "../gen/Agent";
import type { DecisionSource } from "../gen/DecisionSource";
import type { Event } from "../gen/Event";
import type { EventBody } from "../gen/EventBody";
import type { FileOp } from "../gen/FileOp";
import type { Item } from "../gen/Item";
import type { ItemDelta } from "../gen/ItemDelta";
import type { PermissionDecision } from "../gen/PermissionDecision";
import type { PermissionRequest } from "../gen/PermissionRequest";
import type { PlanEntry } from "../gen/PlanEntry";
import type { SessionRepo } from "../gen/SessionRepo";
import type { SessionStatus } from "../gen/SessionStatus";

/** Running totals, in the shape of the `usage` event. */
export type Usage = Omit<Extract<EventBody, { type: "usage" }>, "type">;

/**
 * Where a permission request stands. `lapsed`: nobody had answered when the
 * turn ended, so the answer is no longer needed; `message` is the server's note, if any.
 */
export type PermissionOutcome =
  | { state: "pending" }
  | { state: "resolved"; decision: PermissionDecision; by: DecisionSource; message: string | null }
  | { state: "lapsed"; message: string | null };

/** One row of the transcript. `key` is stable for the life of the row. */
export type TranscriptEntry =
  | { kind: "user"; key: string; ts: number; text: string }
  | {
      kind: "item";
      key: string;
      ts: number;
      agentId: string | null;
      item: Item;
      /** True when the turn ended while the item was still `running`; it will not finish. */
      interrupted: boolean;
    }
  | {
      kind: "permission";
      key: string;
      ts: number;
      /** The agent that asked. */
      agentId: string | null;
      request: PermissionRequest;
      outcome: PermissionOutcome;
    }
  | { kind: "error"; key: string; ts: number; agentId: string | null; message: string };

/** A permission request nobody has answered yet. */
export type PendingPermission = {
  request: PermissionRequest;
  agentId: string | null;
  ts: number;
  /** `seq` of the event that asked, so a newer request can be told from an older one. */
  seq: number;
};

/** The last thing that happened to one file, from `file_changed` events. */
export type FileChange = {
  key: string;
  /** `null` when the path is outside every attached repo; `path` is then absolute. */
  sessionRepoId: string | null;
  path: string;
  op: FileOp;
  /** `user`: saved from the editor. `agent`: an edit tool call. */
  by: "agent" | "user";
  agentId: string | null;
  ts: number;
};

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
  /** Repos attached by events, the primary one first. The store adds those the REST record lists. */
  readonly repos: readonly SessionRepo[];
  /** Oldest first. Each one also has a `permission` row in the transcript. */
  readonly pendingPermissions: readonly PendingPermission[];
  /** One entry per file, in the order they were first touched. */
  readonly changedFiles: readonly FileChange[];
  /** Goes up by one with every `file_changed` event; watch it to know when to refetch. */
  readonly changesRevision: number;
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
  repos: [],
  pendingPermissions: [],
  changedFiles: [],
  changesRevision: 0,
};

/**
 * `duplicate`: the event was already applied and changed nothing.
 * `gap`: events are missing before this one; the caller resubscribes from `view.seq`.
 */
export type ApplyOutcome = "applied" | "duplicate" | "gap";
export type ApplyResult = { view: SessionView; outcome: ApplyOutcome };

const itemKey = (itemId: string): string => `item:${itemId}`;
const permissionKey = (requestId: string): string => `permission:${requestId}`;
const fileKey = (sessionRepoId: string | null, path: string): string =>
  `${sessionRepoId ?? ""}\n${path}`;

/** Statuses in which no turn is in progress: what was still open will not finish (api.rs, Semantics). */
const turnOver: readonly SessionStatus[] = ["review", "done", "failed", "cancelled"];

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
    interrupted: false,
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

/** Inserts `repo` or replaces the one with its id, keeping the primary repo first. Pure. */
export function upsertRepo(list: readonly SessionRepo[], repo: SessionRepo): SessionRepo[] {
  const index = list.findIndex((one) => one.id === repo.id);
  const next = index === -1 ? [...list, repo] : list.with(index, repo);
  return [...next.filter((one) => one.primary), ...next.filter((one) => !one.primary)];
}

/** The repo set: what the REST record listed, updated by the `repo_attached` events seen since. */
export function mergeRepos(
  seed: readonly SessionRepo[],
  attached: readonly SessionRepo[],
): readonly SessionRepo[] {
  return attached.length === 0 ? seed : attached.reduce(upsertRepo, seed.slice());
}

/** The turn ended: running items become interrupted, unanswered requests lapse. */
function closeTurn(view: SessionView): SessionView {
  const open = (entry: TranscriptEntry): boolean =>
    (entry.kind === "item" && entry.item.status === "running" && !entry.interrupted) ||
    (entry.kind === "permission" && entry.outcome.state === "pending");
  if (!view.transcript.some(open)) return view;
  const transcript = view.transcript.map((entry): TranscriptEntry => {
    if (!open(entry)) return entry;
    return entry.kind === "item"
      ? { ...entry, interrupted: true }
      : entry.kind === "permission"
        ? { ...entry, outcome: { state: "lapsed", message: null } }
        : entry;
  });
  return { ...view, transcript, pendingPermissions: [] };
}

function requestPermission(
  view: SessionView,
  event: Event,
  request: PermissionRequest,
): SessionView {
  const key = permissionKey(request.id);
  const entry: TranscriptEntry = {
    kind: "permission",
    key,
    ts: event.ts,
    agentId: event.agent_id,
    request,
    outcome: { state: "pending" },
  };
  const pending: PendingPermission = {
    request,
    agentId: event.agent_id,
    ts: event.ts,
    seq: event.seq,
  };
  // Ids are unique within a session; a repeat replaces the row instead of doubling it.
  const index = view.transcript.findLastIndex((one) => one.key === key);
  return {
    ...view,
    transcript:
      index === -1 ? [...view.transcript, entry] : withEntry(view.transcript, index, entry),
    pendingPermissions: [
      ...view.pendingPermissions.filter((one) => one.request.id !== request.id),
      pending,
    ],
  };
}

function resolvePermission(
  view: SessionView,
  body: Extract<EventBody, { type: "permission_resolved" }>,
): SessionView {
  const key = permissionKey(body.request_id);
  const index = view.transcript.findLastIndex((one) => one.key === key);
  const entry = index === -1 ? undefined : view.transcript[index];
  // An answer to a request this log never showed has no row to update.
  if (entry?.kind !== "permission") return view;
  // The server closes a request left open by the end of a turn as a denial by
  // policy. Policy answers without asking, so a denial of a request that was
  // waiting for the reader is that closure, not a refusal. The wire has no
  // field for it; the session waiting, or the turn already over, is the sign.
  const closed =
    body.decision === "deny" &&
    body.by === "policy" &&
    (entry.outcome.state === "lapsed" || view.status === "waiting");
  const outcome: PermissionOutcome = closed
    ? { state: "lapsed", message: body.message }
    : { state: "resolved", decision: body.decision, by: body.by, message: body.message };
  return {
    ...view,
    transcript: withEntry(view.transcript, index, { ...entry, outcome }),
    pendingPermissions: view.pendingPermissions.filter((one) => one.request.id !== body.request_id),
  };
}

function changeFile(
  view: SessionView,
  event: Event,
  body: Extract<EventBody, { type: "file_changed" }>,
): SessionView {
  const key = fileKey(body.session_repo_id, body.path);
  const change: FileChange = {
    key,
    sessionRepoId: body.session_repo_id,
    path: body.path,
    op: body.op,
    // event.rs: the user's own saves carry no agent.
    by: event.agent_id === null ? "user" : "agent",
    agentId: event.agent_id,
    ts: event.ts,
  };
  const index = view.changedFiles.findIndex((one) => one.key === key);
  return {
    ...view,
    changedFiles:
      index === -1 ? [...view.changedFiles, change] : view.changedFiles.with(index, change),
    changesRevision: view.changesRevision + 1,
  };
}

function applyBody(view: SessionView, event: Event): SessionView {
  // Widened on purpose: a frame without a body must not throw in the fold.
  const body = event.body as EventBody | null | undefined;
  if (!body) return view;
  switch (body.type) {
    case "session_status": {
      const next = { ...view, status: body.status, statusReason: body.reason };
      return turnOver.includes(body.status) ? closeTurn(next) : next;
    }
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
    case "agent_session":
      return patchAgent(view, event.agent_id, {
        native_session_id: body.native_session_id,
        // A runner that does not name its model leaves the one already known in place.
        ...(body.model === null ? {} : { model: body.model }),
      });
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
    case "repo_attached":
      return { ...view, repos: upsertRepo(view.repos, body.repo) };
    case "permission_requested":
      return requestPermission(view, event, body.request);
    case "permission_resolved":
      return resolvePermission(view, body);
    case "file_changed":
      return changeFile(view, event, body);
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
