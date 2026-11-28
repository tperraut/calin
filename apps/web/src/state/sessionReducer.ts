import type { Agent } from "../gen/Agent";
import type { AgentMessage } from "../gen/AgentMessage";
import type { DecisionSource } from "../gen/DecisionSource";
import type { Event } from "../gen/Event";
import type { EventBody } from "../gen/EventBody";
import type { FileOp } from "../gen/FileOp";
import type { Item } from "../gen/Item";
import type { ItemDelta } from "../gen/ItemDelta";
import type { PermissionDecision } from "../gen/PermissionDecision";
import type { PermissionRequest } from "../gen/PermissionRequest";
import type { PlanEntry } from "../gen/PlanEntry";
import type { PullRequest } from "../gen/PullRequest";
import type { Question } from "../gen/Question";
import type { SessionRepo } from "../gen/SessionRepo";
import type { SessionStatus } from "../gen/SessionStatus";
import type { WorkflowNodeInfo } from "../gen/WorkflowNodeInfo";
import type { WorkflowRun } from "../gen/WorkflowRun";
import type { WorkflowStatus } from "../gen/WorkflowStatus";
import { finishRun, foldNode, isFailurePort, startRun } from "../lib/workflow";

/** Running totals, in the shape of the `usage` event. */
export type Usage = Omit<Extract<EventBody, { type: "usage" }>, "type">;

/** Who can answer a request; `expired` is nobody answering in time. */
export type Answerer = Exclude<DecisionSource, "expired">;

/**
 * Where a permission request stands. `lapsed`: nobody had answered when the
 * turn ended (an `expired` resolution, or the turn seen ending before one
 * arrived), so the answer is no longer needed; `message` is the server's note, if any.
 */
export type PermissionOutcome =
  | { state: "pending" }
  | { state: "resolved"; decision: PermissionDecision; by: Answerer; message: string | null }
  | { state: "lapsed"; message: string | null };

/** A permission request made for one tool call, shown in that call's card. */
export type ToolPermission = { request: PermissionRequest; outcome: PermissionOutcome; ts: number };

/**
 * Where a question stands. `expired`: the session ended before anyone answered
 * (event.rs), so the answer is no longer needed.
 */
export type QuestionOutcome =
  | { state: "pending" }
  | { state: "answered"; answer: string; by: Answerer }
  | { state: "expired" };

/**
 * What a workflow run did, as one light transcript row (workflows spec, section 6).
 * A node's row says what it is about; a finished node has a row only when its
 * result is news: a failure port, a verdict, a gate's decision, nothing to commit.
 */
export type WorkflowNote =
  | { what: "started"; name: string }
  | {
      what: "node_started";
      nodeId: string;
      nodeKind: string;
      label: string;
      /** Which start of the node this is, from 1. */
      run: number;
    }
  | {
      what: "node_finished";
      nodeId: string;
      nodeKind: string;
      label: string;
      port: string;
      outcome: string | null;
      failure: boolean;
    }
  | { what: "finished"; status: WorkflowStatus; reason: string | null };

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
      /** The requests that named this tool call as their `item_id`, oldest first. Absent when none did. */
      permissions?: readonly ToolPermission[];
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
  | {
      /** A repo that joined after the session's first turn began; the starting set is not news. */
      kind: "repo";
      key: string;
      ts: number;
      repo: SessionRepo;
      /** Who attached it: the reader, or an agent through the tool endpoint (api.rs). */
      by: "user" | "agent";
      agentId: string | null;
    }
  | {
      /** A pull request was first opened, or was merged or closed: its other changes are not rows. */
      kind: "pull_request";
      key: string;
      ts: number;
      what: "opened" | "merged" | "closed";
      /** As it was then: the row links to it. */
      pullRequest: PullRequest;
    }
  | {
      /** A message between two participants of the session tree, as this session saw it. */
      kind: "message";
      key: string;
      ts: number;
      /** The agent of this session it concerns: the sender of one sent, the recipient of one received. */
      agentId: string | null;
      /** `sent`: it left this session (`message_sent`). `received`: it arrived here. */
      direction: "sent" | "received";
      message: AgentMessage;
    }
  | {
      kind: "question";
      key: string;
      ts: number;
      /** The agent that asked. */
      agentId: string | null;
      question: Question;
      outcome: QuestionOutcome;
    }
  | { kind: "error"; key: string; ts: number; agentId: string | null; message: string }
  | { kind: "workflow"; key: string; ts: number; agentId: null; note: WorkflowNote };

/** A question an agent asked that nobody has answered yet. */
export type OpenQuestion = {
  question: Question;
  agentId: string | null;
  ts: number;
  /** `seq` of the event that asked, so a newer question can be told from an older one. */
  seq: number;
};

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
  /** The `changesRevision` this change brought the view to. */
  revision: number;
};

/** Everything the session screen shows, as a fold of the session's event log. */
export type SessionView = {
  /** `seq` of the last event applied; 0 before the first one. */
  readonly seq: number;
  readonly status: SessionStatus | null;
  readonly statusReason: string | null;
  /** True once an agent turn began: repos attached from then on were added to a session under way. */
  readonly turnStarted: boolean;
  readonly agents: readonly Agent[];
  /** Sum of the latest totals of every agent that reported usage. */
  readonly usage: Usage | null;
  readonly usageByAgent: Readonly<Record<string, Usage>>;
  readonly transcript: readonly TranscriptEntry[];
  readonly plan: readonly PlanEntry[];
  /** Repos attached by events, the primary one first. The store adds those the REST record lists. */
  readonly repos: readonly SessionRepo[];
  /** Oldest first. Each one also shows in the transcript: in its tool call's card, or in a row of its own. */
  readonly pendingPermissions: readonly PendingPermission[];
  /** One entry per file, in the order they were first touched. */
  readonly changedFiles: readonly FileChange[];
  /** Goes up by one with every `file_changed` event; watch it to know when to refetch. */
  readonly changesRevision: number;
  /** One per repo, as the events last described it, in the order they were first seen. */
  readonly pullRequests: readonly PullRequest[];
  /** Oldest first. Each one also shows in the transcript, where it was asked. */
  readonly openQuestions: readonly OpenQuestion[];
  /**
   * Subagent id to the id of the tool call that started it, for the subagents
   * whose call could be told; the transcript groups a subagent's rows under it.
   */
  readonly subagentCalls: Readonly<Record<string, string>>;
  /** Event id to item id, for the delegation (`task`) tool calls: what an `agent_spawned` may name as its cause. */
  readonly taskStarts: Readonly<Record<string, string>>;
  /**
   * The workflow run, as its events tell it; `null` when the session runs none.
   * Loop limits and the tree's cost are not in the events: the store adds them.
   */
  readonly workflow: WorkflowRun | null;
};

export const emptySessionView: SessionView = {
  seq: 0,
  status: null,
  statusReason: null,
  turnStarted: false,
  agents: [],
  usage: null,
  usageByAgent: {},
  transcript: [],
  plan: [],
  repos: [],
  pendingPermissions: [],
  changedFiles: [],
  changesRevision: 0,
  pullRequests: [],
  openQuestions: [],
  subagentCalls: {},
  taskStarts: {},
  workflow: null,
};

/**
 * `duplicate`: the event was already applied and changed nothing.
 * `gap`: events are missing before this one; the caller resubscribes from `view.seq`.
 */
export type ApplyOutcome = "applied" | "duplicate" | "gap";
export type ApplyResult = { view: SessionView; outcome: ApplyOutcome };

const itemKey = (itemId: string): string => `item:${itemId}`;
const permissionKey = (requestId: string): string => `permission:${requestId}`;
const messageKey = (messageId: string): string => `message:${messageId}`;
const questionKey = (questionId: string): string => `question:${questionId}`;
const fileKey = (sessionRepoId: string | null, path: string): string =>
  `${sessionRepoId ?? ""}\n${path}`;

const pullRequestKey = (pullRequest: PullRequest, what: string): string =>
  `pull_request:${pullRequest.session_repo_id}:${pullRequest.number}:${what}`;

/**
 * Statuses in which no turn is in progress: what was still open will not
 * finish (api.rs, Semantics). `shipping` is one: a message starts a turn from it.
 */
const turnOver: readonly SessionStatus[] = ["review", "shipping", "done", "failed", "cancelled"];

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

/** True for the row, or the tool card, that shows the request `requestId`. */
const showsRequest = (entry: TranscriptEntry, requestId: string): boolean =>
  entry.kind === "permission"
    ? entry.request.id === requestId
    : entry.kind === "item" &&
      entry.permissions?.some((asked) => asked.request.id === requestId) === true;

function appendRow(view: SessionView, entry: TranscriptEntry): SessionView {
  return { ...view, transcript: [...view.transcript, entry] };
}

/** Ids are unique within a session; a repeat replaces the row instead of doubling it. */
function putRow(transcript: readonly TranscriptEntry[], entry: TranscriptEntry): TranscriptEntry[] {
  const index = transcript.findLastIndex((one) => one.key === entry.key);
  return index === -1 ? [...transcript, entry] : withEntry(transcript, index, entry);
}

/** A delegation call: what a subagent's `agent_spawned` may point back to. */
const isTask = (item: Item): boolean =>
  item.content.kind === "tool" && item.content.tool_kind === "task";

function itemEntry(
  event: Event,
  item: Item,
  ts: number,
  permissions: readonly ToolPermission[] | undefined,
): TranscriptEntry {
  return {
    kind: "item",
    key: itemKey(item.id),
    ts,
    agentId: event.agent_id,
    item,
    interrupted: false,
    ...(permissions ? { permissions } : {}),
  };
}

/** Requests that named this tool call before it arrived, each waiting in a row of its own. */
function earlyRequests(view: SessionView, item: Item): readonly PendingPermission[] {
  if (item.content.kind !== "tool") return [];
  return view.pendingPermissions.filter((one) => one.request.item_id === item.id);
}

function addItem(view: SessionView, event: Event, item: Item): SessionView {
  // An early request leaves its row for the card.
  const early = earlyRequests(view, item);
  const moved = new Set(early.map((one) => one.request.id));
  const transcript = view.transcript.filter(
    (entry) => entry.kind !== "permission" || !moved.has(entry.request.id),
  );
  const permissions = early.map((one): ToolPermission => ({
    request: one.request,
    outcome: { state: "pending" },
    ts: one.ts,
  }));
  const entry = itemEntry(event, item, event.ts, early.length > 0 ? permissions : undefined);
  return {
    ...view,
    turnStarted: true,
    taskStarts: isTask(item) ? { ...view.taskStarts, [event.id]: item.id } : view.taskStarts,
    transcript: [...transcript, entry],
  };
}

function putItem(view: SessionView, event: Event, item: Item): SessionView {
  const index = findItem(view.transcript, item.id);
  if (index === -1) return addItem(view, event, item);
  const existing = view.transcript[index];
  const permissions = existing.kind === "item" ? existing.permissions : undefined;
  // A completed item keeps the place and time at which it started.
  const entry = itemEntry(event, item, existing.ts, permissions);
  return { ...view, turnStarted: true, transcript: withEntry(view.transcript, index, entry) };
}

/** A delegation call of `parentId` still running and not yet taken by another subagent. */
const isOpenTask = (
  entry: TranscriptEntry,
  parentId: string | null,
  taken: ReadonlySet<string>,
): boolean =>
  entry.kind === "item" &&
  entry.agentId === parentId &&
  entry.item.status === "running" &&
  isTask(entry.item) &&
  !taken.has(entry.item.id);

/**
 * The tool call that started `agent`. The contract does not name it, so: the
 * call whose start event the `agent_spawned` gives as its cause, when it does;
 * otherwise the parent's newest delegation call still running and not yet
 * taken by another subagent.
 */
function callOf(view: SessionView, event: Event, agent: Agent): string | undefined {
  const named = event.parent_id === null ? undefined : view.taskStarts[event.parent_id];
  if (named !== undefined) return named;
  const taken = new Set(Object.values(view.subagentCalls));
  const call = view.transcript.findLast((entry) => isOpenTask(entry, agent.parent_agent_id, taken));
  return call?.kind === "item" ? call.item.id : undefined;
}

function spawnAgent(view: SessionView, event: Event, agent: Agent): SessionView {
  const others = view.agents.filter((one) => one.id !== agent.id);
  const next = { ...view, agents: [...others, agent] };
  if (agent.kind !== "subagent" || view.subagentCalls[agent.id] !== undefined) return next;
  const call = callOf(view, event, agent);
  return call === undefined
    ? next
    : { ...next, subagentCalls: { ...view.subagentCalls, [agent.id]: call } };
}

/**
 * A message that left this session or arrived in it. One id is one row: a note
 * to this session's own user is both sent and received here (event.rs).
 */
function putMessage(
  view: SessionView,
  event: Event,
  described: AgentMessage,
  direction: "sent" | "received",
): SessionView {
  // Widened on purpose, as the body is: an event that carries no message changes nothing.
  const message = described as AgentMessage | null | undefined;
  if (!message) return view;
  const key = messageKey(message.id);
  if (view.transcript.some((entry) => entry.key === key)) return view;
  const entry: TranscriptEntry = {
    kind: "message",
    key,
    ts: event.ts,
    agentId: direction === "sent" ? message.from_agent_id : message.to_agent_id,
    direction,
    message,
  };
  return { ...view, transcript: [...view.transcript, entry] };
}

function askQuestion(view: SessionView, event: Event, question: Question): SessionView {
  const key = questionKey(question.id);
  const entry: TranscriptEntry = {
    kind: "question",
    key,
    ts: event.ts,
    agentId: event.agent_id,
    question,
    outcome: { state: "pending" },
  };
  const open: OpenQuestion = { question, agentId: event.agent_id, ts: event.ts, seq: event.seq };
  return {
    ...view,
    transcript: putRow(view.transcript, entry),
    openQuestions: [...view.openQuestions.filter((one) => one.question.id !== question.id), open],
  };
}

function answerQuestion(
  view: SessionView,
  body: Extract<EventBody, { type: "question_answered" }>,
): SessionView {
  const openQuestions = view.openQuestions.filter((one) => one.question.id !== body.question_id);
  const index = view.transcript.findLastIndex((one) => one.key === questionKey(body.question_id));
  const entry = index === -1 ? undefined : view.transcript[index];
  // An answer to a question this log never showed has no row to update.
  if (entry?.kind !== "question") return { ...view, openQuestions };
  const outcome: QuestionOutcome =
    body.by === "expired"
      ? { state: "expired" }
      : { state: "answered", answer: body.answer, by: body.by };
  return {
    ...view,
    transcript: withEntry(view.transcript, index, { ...entry, outcome }),
    openQuestions,
  };
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

const lapsed: PermissionOutcome = { state: "lapsed", message: null };

const isPending = (asked: { outcome: PermissionOutcome }): boolean =>
  asked.outcome.state === "pending";

const isRunning = (entry: TranscriptEntry): boolean =>
  entry.kind === "item" && entry.item.status === "running" && !entry.interrupted;

const isAsking = (entry: TranscriptEntry): boolean =>
  entry.kind === "permission"
    ? isPending(entry)
    : entry.kind === "item" && entry.permissions?.some(isPending) === true;

/** What a turn's end changes: something still running or still asking. */
const isOpen = (entry: TranscriptEntry): boolean => isRunning(entry) || isAsking(entry);

const lapse = (asked: ToolPermission): ToolPermission =>
  isPending(asked) ? { ...asked, outcome: lapsed } : asked;

function closeEntry(entry: TranscriptEntry): TranscriptEntry {
  if (entry.kind === "permission") return isPending(entry) ? { ...entry, outcome: lapsed } : entry;
  if (entry.kind !== "item" || !isOpen(entry)) return entry;
  const closed = { ...entry, interrupted: entry.interrupted || isRunning(entry) };
  return entry.permissions ? { ...closed, permissions: entry.permissions.map(lapse) } : closed;
}

/** The turn ended: running items become interrupted, unanswered requests lapse. */
function closeTurn(view: SessionView): SessionView {
  if (!view.transcript.some(isOpen)) return view;
  return { ...view, transcript: view.transcript.map(closeEntry), pendingPermissions: [] };
}

function requestPermission(
  view: SessionView,
  event: Event,
  request: PermissionRequest,
): SessionView {
  const pending: PendingPermission = {
    request,
    agentId: event.agent_id,
    ts: event.ts,
    seq: event.seq,
  };
  const pendingPermissions = [
    ...view.pendingPermissions.filter((one) => one.request.id !== request.id),
    pending,
  ];
  // permission.rs: `item_id` names the tool call the request is for; the two are one card.
  const itemIndex = request.item_id === null ? -1 : findItem(view.transcript, request.item_id);
  const call = itemIndex === -1 ? undefined : view.transcript[itemIndex];
  if (call?.kind === "item" && call.item.content.kind === "tool") {
    const asked: ToolPermission = { request, outcome: { state: "pending" }, ts: event.ts };
    const others = (call.permissions ?? []).filter((one) => one.request.id !== request.id);
    return {
      ...view,
      transcript: withEntry(view.transcript, itemIndex, {
        ...call,
        permissions: [...others, asked],
      }),
      pendingPermissions,
    };
  }
  const entry: TranscriptEntry = {
    kind: "permission",
    key: permissionKey(request.id),
    ts: event.ts,
    agentId: event.agent_id,
    request,
    outcome: { state: "pending" },
  };
  return { ...view, transcript: putRow(view.transcript, entry), pendingPermissions };
}

function resolvePermission(
  view: SessionView,
  body: Extract<EventBody, { type: "permission_resolved" }>,
): SessionView {
  const index = view.transcript.findLastIndex((one) => showsRequest(one, body.request_id));
  const entry = index === -1 ? undefined : view.transcript[index];
  // An answer to a request this log never showed has nothing to update.
  if (!entry || (entry.kind !== "permission" && entry.kind !== "item")) return view;
  // permission.rs: `expired` is the server closing a request nobody answered.
  const outcome: PermissionOutcome =
    body.by === "expired"
      ? { state: "lapsed", message: body.message }
      : { state: "resolved", decision: body.decision, by: body.by, message: body.message };
  const answered: TranscriptEntry =
    entry.kind === "permission"
      ? { ...entry, outcome }
      : {
          ...entry,
          permissions: entry.permissions?.map((asked) =>
            asked.request.id === body.request_id ? { ...asked, outcome } : asked,
          ),
        };
  return {
    ...view,
    transcript: withEntry(view.transcript, index, answered),
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
    revision: view.changesRevision + 1,
  };
  const index = view.changedFiles.findIndex((one) => one.key === key);
  return {
    ...view,
    changedFiles:
      index === -1 ? [...view.changedFiles, change] : view.changedFiles.with(index, change),
    changesRevision: view.changesRevision + 1,
  };
}

/**
 * Inserts the pull request or replaces the one of its repo (a session has at
 * most one per repo). The transcript says it once when it is opened and once
 * when it is merged or closed: shipping again announces the same one again.
 */
function putPullRequest(
  view: SessionView,
  event: Event,
  described: PullRequest,
  opened: boolean,
): SessionView {
  // Widened on purpose, as the body is: an event that names no pull request changes nothing.
  const pullRequest = described as PullRequest | null | undefined;
  if (!pullRequest) return view;
  const index = view.pullRequests.findIndex(
    (one) => one.session_repo_id === pullRequest.session_repo_id,
  );
  const pullRequests =
    index === -1 ? [...view.pullRequests, pullRequest] : view.pullRequests.with(index, pullRequest);
  let transcript = view.transcript;
  const news = [
    ...(opened ? (["opened"] as const) : []),
    ...(pullRequest.state === "merged" || pullRequest.state === "closed"
      ? [pullRequest.state]
      : []),
  ];
  for (const what of news) {
    const key = pullRequestKey(pullRequest, what);
    if (transcript.some((entry) => entry.key === key)) continue;
    transcript = [...transcript, { kind: "pull_request", key, ts: event.ts, what, pullRequest }];
  }
  return { ...view, pullRequests, transcript };
}

/** Whether a finished node's result is worth a row of its own (see `WorkflowNote`). */
function finishIsNews(kind: string, port: string, failure: boolean): boolean {
  if (failure) return true;
  if (kind === "agent") return port !== "done";
  return kind === "gate" || (kind === "commit" && port === "nothing");
}

type EventType = EventBody["type"];
type Body<K extends EventType> = Extract<EventBody, { type: K }>;

const eventKey = (event: Event): string => `event:${event.id}`;

const workflowEntry = (event: Event, note: WorkflowNote): TranscriptEntry => ({
  kind: "workflow",
  key: `workflow:${event.id}`,
  ts: event.ts,
  agentId: null,
  note,
});

function startWorkflow(view: SessionView, event: Event, body: Body<"workflow_started">) {
  const workflow = startRun(body);
  const row = workflowEntry(event, { what: "started", name: workflow.name });
  return { ...appendRow(view, row), workflow };
}

function finishWorkflow(view: SessionView, event: Event, body: Body<"workflow_finished">) {
  // An end without a start this log showed has nothing to update.
  if (!view.workflow) return view;
  const reason = body.reason ?? null;
  const workflow = finishRun(view.workflow, { ...body, reason });
  const row = workflowEntry(event, { what: "finished", status: body.status, reason });
  return { ...appendRow(view, row), workflow };
}

/** The node `nodeId` names, unless it is the start or the end: those are not steps of their own. */
function stepOf(run: WorkflowRun, nodeId: string): WorkflowNodeInfo | undefined {
  const info = run.graph.nodes.find((node) => node.id === nodeId);
  return info && info.kind !== "start" && info.kind !== "end" ? info : undefined;
}

/** The row a step's start or end is worth, if any (see `WorkflowNote`). */
function stepNote(
  after: WorkflowRun,
  info: WorkflowNodeInfo,
  body: Body<"workflow_node">,
): WorkflowNote | undefined {
  const common = { nodeId: info.id, nodeKind: info.kind, label: info.label };
  if (body.phase === "started") {
    const runs = after.nodes.find((node) => node.id === info.id)?.runs ?? 1;
    return { what: "node_started", ...common, run: runs };
  }
  const port = body.port;
  if (port === null) return undefined;
  const failure = isFailurePort(info, port);
  if (!finishIsNews(info.kind, port, failure)) return undefined;
  return { what: "node_finished", ...common, port, outcome: body.outcome, failure };
}

function foldWorkflowNode(view: SessionView, event: Event, body: Body<"workflow_node">) {
  const run = view.workflow;
  // A node without a start this log showed has nothing to update.
  if (!run) return view;
  const workflow = foldNode(run, body);
  const step = workflow === run ? undefined : stepOf(run, body.node_id);
  const note = step && stepNote(workflow, step, body);
  const next = { ...view, workflow };
  return note ? appendRow(next, workflowEntry(event, note)) : next;
}

function setStatus(view: SessionView, body: Body<"session_status">): SessionView {
  const next = {
    ...view,
    status: body.status,
    statusReason: body.reason,
    // manager: the session is `running` from its first turn on, after its starting repos are attached.
    turnStarted: view.turnStarted || body.status === "running",
  };
  return turnOver.includes(body.status) ? closeTurn(next) : next;
}

function attachRepo(view: SessionView, event: Event, repo: SessionRepo): SessionView {
  const next = { ...view, repos: upsertRepo(view.repos, repo) };
  // The starting set is not news, and neither is a repo the session already has.
  if (!view.turnStarted || view.repos.some((one) => one.id === repo.id)) return next;
  return appendRow(next, {
    kind: "repo",
    key: eventKey(event),
    ts: event.ts,
    repo,
    // api.rs: an agent's own request carries its id; the reader's carries none.
    by: event.agent_id === null ? "user" : "agent",
    agentId: event.agent_id,
  });
}

/** How one kind of event folds into the view. */
type Fold<K extends EventType> = (view: SessionView, event: Event, body: Body<K>) => SessionView;

/** One fold per event kind; the compiler checks that none is missing. */
const folds: { [K in EventType]: Fold<K> } = {
  session_status: (view, _event, body) => setStatus(view, body),
  user_message: (view, event, body) =>
    appendRow(view, { kind: "user", key: eventKey(event), ts: event.ts, text: body.text }),
  agent_spawned: (view, event, body) => spawnAgent(view, event, body.agent),
  agent_status: (view, event, body) => patchAgent(view, event.agent_id, { status: body.status }),
  agent_session: (view, event, body) =>
    patchAgent(view, event.agent_id, {
      native_session_id: body.native_session_id,
      // A runner that does not name its model leaves the one already known in place.
      ...(body.model === null ? {} : { model: body.model }),
    }),
  item_started: (view, event, body) => putItem(view, event, body.item),
  item_completed: (view, event, body) => putItem(view, event, body.item),
  item_delta: (view, _event, body) => applyDelta(view, body.item_id, body.delta),
  plan_updated: (view, _event, body) => ({ ...view, plan: body.entries }),
  usage: (view, event, body) =>
    applyUsage(view, event.agent_id, {
      cost_usd: body.cost_usd,
      tokens_in: body.tokens_in,
      tokens_out: body.tokens_out,
    }),
  repo_attached: (view, event, body) => attachRepo(view, event, body.repo),
  permission_requested: (view, event, body) => requestPermission(view, event, body.request),
  permission_resolved: (view, _event, body) => resolvePermission(view, body),
  file_changed: changeFile,
  pull_request_opened: (view, event, body) => putPullRequest(view, event, body.pull_request, true),
  pull_request_updated: (view, event, body) =>
    putPullRequest(view, event, body.pull_request, false),
  message_sent: (view, event, body) => putMessage(view, event, body.message, "sent"),
  message_received: (view, event, body) => putMessage(view, event, body.message, "received"),
  question_asked: (view, event, body) => askQuestion(view, event, body.question),
  question_answered: (view, _event, body) => answerQuestion(view, body),
  workflow_started: startWorkflow,
  workflow_node: foldWorkflowNode,
  workflow_finished: finishWorkflow,
  // Not a transcript row: the store hands it to the app (integrations spec, section 1).
  limits: (view) => view,
  error: (view, event, body) =>
    appendRow(view, {
      kind: "error",
      key: eventKey(event),
      ts: event.ts,
      agentId: event.agent_id,
      message: body.message,
    }),
};

function foldAs<K extends EventType>(
  type: K,
  view: SessionView,
  event: Event,
  body: Body<K>,
): SessionView {
  const fold: Fold<K> = folds[type];
  return fold(view, event, body);
}

function applyBody(view: SessionView, event: Event): SessionView {
  // Widened on purpose: a frame without a body must not throw in the fold.
  const body = event.body as EventBody | null | undefined;
  // An event type from a newer server: it still takes its place in the sequence.
  if (!body || !Object.hasOwn(folds, body.type)) return view;
  return foldAs(body.type, view, event, body);
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
