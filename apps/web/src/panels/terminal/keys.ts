// What terminals have always sent for Ctrl with a key that is not a letter.
const controlSymbols: Record<string, string> = {
  " ": "\x00",
  "2": "\x00",
  "3": "\x1b",
  "4": "\x1c",
  "5": "\x1d",
  "6": "\x1e",
  "7": "\x1f",
  "-": "\x1f",
  "/": "\x1f",
  "8": "\x7f",
  "?": "\x7f",
};

/** The byte a letter or symbol becomes with Ctrl held; `undefined` when Ctrl does nothing to it. */
export function controlCode(char: string): string | undefined {
  if (char.length !== 1) return undefined;
  if (Object.hasOwn(controlSymbols, char)) return controlSymbols[char];
  const code = char.toUpperCase().charCodeAt(0);
  // `@` to `_`, which holds A to Z: Ctrl clears the top bits.
  return code >= 0x40 && code <= 0x5f ? String.fromCharCode(code & 0x1f) : undefined;
}

export type ExtraKey = {
  /** What the button shows. */
  cap: string;
  /** Its accessible name. */
  label: string;
  /** What it sends; arrows depend on the mode the running program asked for. */
  sequence: (applicationCursorKeys: boolean) => string;
};

const fixed = (cap: string, label: string, sequence: string): ExtraKey => ({
  cap,
  label,
  sequence: () => sequence,
});
const arrow = (cap: string, label: string, final: string): ExtraKey => ({
  cap,
  label,
  sequence: (application) => `\x1b${application ? "O" : "["}${final}`,
});

/** The keys a phone's keyboard lacks, in the order of the row. Ctrl sits between Tab and the arrows. */
export const keysBeforeCtrl: readonly ExtraKey[] = [
  fixed("Esc", "Escape", "\x1b"),
  fixed("Tab", "Tab", "\t"),
];

export const keysAfterCtrl: readonly ExtraKey[] = [
  arrow("←", "Left arrow", "D"),
  arrow("↑", "Up arrow", "A"),
  arrow("↓", "Down arrow", "B"),
  arrow("→", "Right arrow", "C"),
  fixed("|", "Pipe", "|"),
  fixed("/", "Slash", "/"),
  fixed("-", "Dash", "-"),
  fixed("~", "Tilde", "~"),
];
