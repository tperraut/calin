import type { Session } from "../gen/Session";
import type { SessionStatus } from "../gen/SessionStatus";
import { compareText } from "../lib/format";

/**
 * The sessions of one tree: a root session and every session dispatched from
 * it (api.rs, "Child sessions"), as the `sessions` topic last described them.
 */
export type SessionTree = {
  /** `root_session_id` of the tree; `undefined` until the session on screen is known. */
  readonly rootId: string | undefined;
  /** Oldest first: the root, when it is listed, then the children as they were dispatched. */
  readonly sessions: readonly Session[];
  /** The session that dispatched the one on screen, when it is listed. */
  readonly parent: Session | undefined;
  /** The sessions the one on screen dispatched, oldest first. */
  readonly children: readonly Session[];
};

export const emptySessionTree: SessionTree = {
  rootId: undefined,
  sessions: [],
  parent: undefined,
  children: [],
};

const oldestFirst = (a: Session, b: Session): number =>
  a.created_at - b.created_at || compareText(a.id, b.id);

/** The tree `session` belongs to, among `sessions`. Pure. */
export function sessionTreeOf(
  sessions: readonly Session[],
  session: Session | undefined,
): SessionTree {
  if (!session) return emptySessionTree;
  const members = sessions.filter((one) => one.root_session_id === session.root_session_id);
  // The session on screen may be known from its own record before the list names it.
  if (!members.some((one) => one.id === session.id)) members.push(session);
  members.sort(oldestFirst);
  return {
    rootId: session.root_session_id,
    sessions: members,
    parent: members.find((one) => one.id === session.parent_session_id),
    children: members.filter((one) => one.parent_session_id === session.id),
  };
}

/**
 * What changes when any session of the tree is upserted: the graph and the
 * timeline are read again when this moves.
 */
export const treeRevision = (tree: SessionTree): string =>
  tree.sessions.map((one) => `${one.id}:${one.head_seq}:${one.status}`).join("|");

// model.rs, `SessionStatus::is_settled`: no agent runs again unless the user continues the session.
const settled: readonly SessionStatus[] = ["review", "shipping", "done", "failed", "cancelled"];

export const isSettled = (status: SessionStatus): boolean => settled.includes(status);
