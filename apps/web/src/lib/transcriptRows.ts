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
  const plain = (entry: TranscriptEntry): TranscriptRow => ({
    kind: "entry",
    key: entry.key,
    entry,
  });
  if (filter !== null) {
    return entries.filter((entry) => entryAgent(entry) === filter).map(plain);
  }
  const subagents = new Map(
    agents.filter((agent) => agent.kind === "subagent").map((agent) => [agent.id, agent]),
  );
  if (subagents.size === 0) return entries.map(plain);

  const owned = new Map<string, TranscriptEntry[]>();
  const top: TranscriptEntry[] = [];
  for (const entry of entries) {
    const agentId = entryAgent(entry);
    if (agentId !== null && subagents.has(agentId)) {
      owned.set(agentId, [...(owned.get(agentId) ?? []), entry]);
    } else {
      top.push(entry);
    }
  }
  // Subagents by the call that started them, in the order they were announced.
  const topItems = new Set(top.flatMap((entry) => (entry.kind === "item" ? [entry.item.id] : [])));
  const startedBy = new Map<string, string[]>();
  const loose = new Set<string>();
  for (const id of subagents.keys()) {
    const call = calls[id];
    if (call !== undefined && topItems.has(call)) {
      startedBy.set(call, [...(startedBy.get(call) ?? []), id]);
    } else {
      loose.add(id);
    }
  }
  const group = (agentId: string): TranscriptRow | undefined => {
    const agent = subagents.get(agentId);
    if (!agent) return undefined;
    const mine = owned.get(agentId) ?? [];
    return {
      kind: "subagent",
      key: `subagent:${agentId}`,
      agentId,
      agent,
      entries: mine,
      toolCalls: mine.filter(isToolCall).length,
    };
  };

  const rows: TranscriptRow[] = [];
  const placed = new Set<string>();
  const place = (agentId: string): void => {
    if (placed.has(agentId)) return;
    placed.add(agentId);
    const row = group(agentId);
    if (row) rows.push(row);
  };
  for (const entry of entries) {
    const agentId = entryAgent(entry);
    if (agentId !== null && subagents.has(agentId)) {
      // A subagent with no known call shows where it first did something.
      if (loose.has(agentId)) place(agentId);
      continue;
    }
    rows.push(plain(entry));
    if (entry.kind === "item") {
      for (const started of startedBy.get(entry.item.id) ?? []) place(started);
    }
  }
  return rows;
}
