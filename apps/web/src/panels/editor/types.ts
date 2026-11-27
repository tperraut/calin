// What the panels need from CodeMirror, and nothing else. `codemirror.ts` is
// the one module that implements it; tests replace that module with a plain
// fake, since CodeMirror needs a layout engine happy-dom does not have.

export type EditorOptions = {
  /** The editor's accessible name. */
  label: string;
  /** Decides the grammar, by file name. */
  path: string;
  text: string;
  readOnly: boolean;
  wrap: boolean;
  /** The reader edited the text. Not called for `setText`. */
  onChange?: (text: string) => void;
  /** Ctrl/Cmd+S. */
  onSave?: () => void;
};

export type EditorHandle = {
  text(): string;
  /** Replaces the text from outside (a reload from disk); the caret stays where it can. */
  setText(text: string): void;
  setReadOnly(readOnly: boolean): void;
  setWrap(wrap: boolean): void;
  openSearch(): void;
  focus(): void;
  destroy(): void;
};

export type DiffLayout = "split" | "unified";

export type DiffOptions = {
  label: string;
  path: string;
  original: string;
  modified: string;
  layout: DiffLayout;
};

export type DiffHandle = {
  /** Number of changed regions. */
  changes(): number;
  nextChange(): void;
  previousChange(): void;
  destroy(): void;
};

export type EditorModule = {
  createEditor(parent: HTMLElement, options: EditorOptions): EditorHandle;
  /** Read-only, with unchanged regions collapsed. */
  createDiff(parent: HTMLElement, options: DiffOptions): DiffHandle;
};
