import type { Session } from "../gen/Session";

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
