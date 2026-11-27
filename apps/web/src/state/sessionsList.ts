import type { Session } from "../gen/Session";
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
