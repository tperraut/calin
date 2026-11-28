/** What a live region says, and the `seq` of the item it last read out. */
export type Announcement = { seq: number; text: string };

/**
 * The live region's next text for a list of waiting items, oldest first: the
 * newest is read out once, when it arrives. The text only changes for a newer
 * item, so answering one never repeats another; an empty list clears it.
 */
export function announceNewest<T extends { seq: number }>(
  previous: Announcement | undefined,
  items: readonly T[],
  say: (newest: T, others: number) => string,
): Announcement {
  const last = previous ?? { seq: 0, text: "" };
  const newest = items.at(-1);
  if (!newest) return last.text === "" ? last : { seq: last.seq, text: "" };
  if (newest.seq <= last.seq) return last;
  return { seq: newest.seq, text: say(newest, items.length - 1) };
}
