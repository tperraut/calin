export type TextChange = { from: number; to: number; insert: string };

/**
 * The smallest single replacement that turns `before` into `after`: what they
 * share at both ends is left alone, so a caret outside the changed middle
 * keeps its place. `undefined` when the texts are equal.
 */
export function minimalChange(before: string, after: string): TextChange | undefined {
  if (before === after) return undefined;
  const limit = Math.min(before.length, after.length);
  let start = 0;
  while (start < limit && before.charCodeAt(start) === after.charCodeAt(start)) start += 1;
  let end = 0;
  while (
    end < limit - start &&
    before.charCodeAt(before.length - 1 - end) === after.charCodeAt(after.length - 1 - end)
  ) {
    end += 1;
  }
  return { from: start, to: before.length - end, insert: after.slice(start, after.length - end) };
}
