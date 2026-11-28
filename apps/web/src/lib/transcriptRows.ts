import type { Agent } from "../gen/Agent";
import type { TranscriptEntry } from "../state/sessionReducer";

/** What the transcript lists: a row of its own, or a subagent's rows gathered under its call. */
export type TranscriptRow =
  | { kind: "entry"; key: string; entry: TranscriptEntry }
  | {
      kind: "subagent";
      key: string;
      agentId: string;
      agent: Agent;
      entries: readonly TranscriptEntry[];
      /** How many tool calls the subagent made so far. */
      toolCalls: number;
    };

/** The agent a row belongs to; the user's own rows belong to none. */
export const entryAgent = (entry: TranscriptEntry): string | null =>
  entry.kind === "user" || entry.kind === "pull_request" ? null : entry.agentId;

const isToolCall = (entry: TranscriptEntry): boolean =>
  entry.kind === "item" && entry.item.content.kind === "tool";

const plain = (entry: TranscriptEntry): TranscriptRow => ({ kind: "entry", key: entry.key, entry });

/** The subagent an entry is from; undefined for the lead's, another agent's or the user's. */
function subagentOf(
  entry: TranscriptEntry,
  subagents: ReadonlyMap<string, Agent>,
): Agent | undefined {
  const agentId = entryAgent(entry);
  return agentId === null ? undefined : subagents.get(agentId);
}

/** A subagent's group, with its entries in order. */
function subagentRow(agent: Agent, entries: readonly TranscriptEntry[]): TranscriptRow {
  const mine = entries.filter((entry) => entryAgent(entry) === agent.id);
  return {
    kind: "subagent",
    key: `subagent:${agent.id}`,
    agentId: agent.id,
    agent,
    entries: mine,
    toolCalls: mine.filter(isToolCall).length,
  };
}

/**
 * The subagents each top-level item started, in the order they were
 * announced (`calls`: subagent id to item id). A call that is not a top-level
 * item does not place its subagent: that one shows where it first did something.
 */
function startedByItem(
  entries: readonly TranscriptEntry[],
  subagents: ReadonlyMap<string, Agent>,
  calls: Readonly<Record<string, string>>,
): ReadonlyMap<string, readonly Agent[]> {
  const topItems = new Set(
    entries.flatMap((entry) =>
      entry.kind === "item" && subagentOf(entry, subagents) === undefined ? [entry.item.id] : [],
    ),
  );
  const startedBy = new Map<string, Agent[]>();
  for (const [id, agent] of subagents) {
    const call = calls[id];
    if (call !== undefined && topItems.has(call)) {
      startedBy.set(call, [...(startedBy.get(call) ?? []), agent]);
    }
  }
  return startedBy;
}

/**
 * The transcript as rows. With `filter` set, only that agent's entries, flat.
 * Otherwise each subagent's entries are gathered into one group, placed right
 * after the tool call that started it (`calls`: subagent id to item id), or
 * where its first entry stood when that call is not known. Pure.
 */
export function transcriptRows(
  entries: readonly TranscriptEntry[],
  agents: readonly Agent[],
  calls: Readonly<Record<string, string>>,
  filter: string | null,
): TranscriptRow[] {
  if (filter !== null) {
    return entries.filter((entry) => entryAgent(entry) === filter).map(plain);
  }
  const subagents = new Map(
    agents.filter((agent) => agent.kind === "subagent").map((agent) => [agent.id, agent]),
  );
  if (subagents.size === 0) return entries.map(plain);

  const startedBy = startedByItem(entries, subagents, calls);
  const underCalls = new Set([...startedBy.values()].flat());
  const rows: TranscriptRow[] = [];
  const placed = new Set<string>();
  const place = (agent: Agent): void => {
    if (placed.has(agent.id)) return;
    placed.add(agent.id);
    rows.push(subagentRow(agent, entries));
  };
  for (const entry of entries) {
    const subagent = subagentOf(entry, subagents);
    // A subagent started by a known call is placed under it; any other, where it first did something.
    if (subagent !== undefined) {
      if (!underCalls.has(subagent)) place(subagent);
      continue;
    }
    rows.push(plain(entry));
    for (const agent of startedAfter(entry, startedBy)) place(agent);
  }
  return rows;
}

/** The subagents `entry` started, when it is an item that started any. */
const startedAfter = (
  entry: TranscriptEntry,
  startedBy: ReadonlyMap<string, readonly Agent[]>,
): readonly Agent[] => (entry.kind === "item" ? (startedBy.get(entry.item.id) ?? []) : []);
