// What the Terminal panel needs from xterm.js. `xterm.ts` implements it; tests
// replace that module with a fake, since xterm measures glyphs to lay out.

export type TerminalSize = { cols: number; rows: number };

export type TerminalViewOptions = {
  /** Keys typed and text pasted, as the text to send to the shell. */
  onInput: (data: string) => void;
  /** Reports that are raw bytes rather than text (some mouse modes). */
  onBytes: (bytes: Uint8Array<ArrayBuffer>) => void;
};

export type TerminalView = {
  /** Output of the shell, or a line of our own. */
  write(data: Uint8Array | string): void;
  /** Sizes the grid to its element. `undefined` while the element is not laid out. */
  fit(): TerminalSize | undefined;
  focus(): void;
  /** As if pasted from the clipboard, honoring bracketed paste. */
  paste(text: string): void;
  /** True while the program running wants arrow keys in application mode. */
  applicationCursorKeys(): boolean;
  dispose(): void;
};

export type TerminalModule = {
  createTerminalView(parent: HTMLElement, options: TerminalViewOptions): TerminalView;
};

/** Sent to a shell that starts before its element could be measured. */
export const DEFAULT_SIZE: TerminalSize = { cols: 80, rows: 24 };

/** Shared with the stylesheet-free sizing of the terminal's element (see TerminalPanel). */
export const TERMINAL_FONT = 'ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace';
export const TERMINAL_FONT_SIZE = 13;
