import type { Session } from "../gen/Session";
import type { SessionStatus } from "../gen/SessionStatus";
import { isSettled } from "./sessionTree";

/** Newest first by `created_at`; the id breaks ties so the order is stable. */
export function compareSessions(a: Session, b: Session): number {
  if (a.created_at !== b.created_at) return b.created_at - a.created_at;
  return a.id < b.id ? 1 : a.id > b.id ? -1 : 0;
}

/** Inserts `session` or replaces the one with its id. Pure. */
export function upsertSession(list: readonly Session[], session: Session): Session[] {
  const next = list.filter((one) => one.id !== session.id);
  next.push(session);
  return next.sort(compareSessions);
}

/** A session of the list with the sessions it dispatched under it. */
export type SessionNode = {
  readonly session: Session;
  /** In the order they were dispatched: oldest first. */
  readonly children: readonly SessionNode[];
};

/**
 * The list as a forest: a child session goes under its parent, a child whose
 * parent is not listed stays at the top level. Top-level order is the list's
 * (newest first); children read in the order they were dispatched. Pure.
 */
export function nestSessions(sessions: readonly Session[]): SessionNode[] {
  const listed = new Set(sessions.map((session) => session.id));
  const under = new Map<string, Session[]>();
  const top: Session[] = [];
  for (const session of sessions) {
    const parent = session.parent_session_id;
    // A session that names itself as its parent would otherwise vanish from the list.
    if (parent === null || parent === session.id || !listed.has(parent)) {
      top.push(session);
    } else {
      under.set(parent, [...(under.get(parent) ?? []), session]);
    }
  }
  const node = (session: Session): SessionNode => ({
    session,
    children: (under.get(session.id) ?? []).toReversed().map(node),
  });
  return top.map(node);
}

/** Every session under `node`, at any depth. */
function descendants(node: SessionNode): Session[] {
  return node.children.flatMap((child) => [child.session, ...descendants(child)]);
}

/**
 * Whether a parent's children show before the reader says otherwise: hidden
 * once every one of them is settled, unless one of them is the open session.
 */
export function childrenShownByDefault(node: SessionNode, openId: string | undefined): boolean {
  const all = descendants(node);
  return all.some((child) => child.id === openId || !isSettled(child.status));
}

/** model.rs: `waiting` asks the user something, `review` waits for them to look (integrations spec, section 4). */
export const needsYou = (status: SessionStatus): boolean =>
  status === "waiting" || status === "review";

const treeNeedsYou = (node: SessionNode): boolean =>
  needsYou(node.session.status) || node.children.some(treeNeedsYou);

/**
 * The top-level rows with every tree that holds a session needing the user
 * first, each group in its existing order. Children stay under their parent. Pure.
 */
export function needsYouFirst(nodes: readonly SessionNode[]): SessionNode[] {
  return [...nodes.filter(treeNeedsYou), ...nodes.filter((node) => !treeNeedsYou(node))];
}

/** The first session needing the user, reading the rows top to bottom, children under their parent. */
export function firstNeedingYou(nodes: readonly SessionNode[]): Session | undefined {
  for (const node of nodes) {
    if (needsYou(node.session.status)) return node.session;
    const below = firstNeedingYou(node.children);
    if (below) return below;
  }
  return undefined;
}

/** What the home summary counts (integrations spec, section 4). */
export type SessionCounts = { needYou: number; running: number; queued: number };

export function countSessions(sessions: readonly Session[]): SessionCounts {
  const counts: SessionCounts = { needYou: 0, running: 0, queued: 0 };
  for (const { status } of sessions) {
    if (needsYou(status)) counts.needYou += 1;
    // `preparing` is a session that took its slot and is setting up its worktrees.
    else if (status === "running" || status === "preparing") counts.running += 1;
    else if (status === "queued") counts.queued += 1;
  }
  return counts;
}
