// Stands in for `panels/editor/codemirror.ts` in the panel tests: the same contract
// over a plain <textarea> they can type into. `codemirror.test.ts` drives the real one.
import type {
  DiffHandle,
  DiffOptions,
  EditorHandle,
  EditorModule,
  EditorOptions,
} from "../panels/editor/types";

export type FakeEditor = {
  options: EditorOptions;
  field: HTMLTextAreaElement;
  /** Every text set from outside (a reload), in order. */
  replaced: string[];
  searches: number;
  destroyed: boolean;
};

export type FakeDiff = { options: DiffOptions; element: HTMLElement; destroyed: boolean };

/** Every editor and diff created since the last `resetFakeEditors()`, oldest first. */
export const editors: FakeEditor[] = [];
export const diffs: FakeDiff[] = [];

export function resetFakeEditors(): void {
  editors.length = 0;
  diffs.length = 0;
}

/** The live editor showing `label` (a file's path). */
export function editorFor(label: string): FakeEditor {
  const editor = editors.findLast((one) => !one.destroyed && one.options.label === label);
  if (!editor) throw new Error(`no live editor for ${label}`);
  return editor;
}

function createEditor(parent: HTMLElement, options: EditorOptions): EditorHandle {
  const field = document.createElement("textarea");
  field.setAttribute("aria-label", options.label);
  field.value = options.text;
  field.readOnly = options.readOnly;
  field.dataset.wrap = String(options.wrap);
  field.addEventListener("input", () => options.onChange?.(field.value));
  field.addEventListener("keydown", (event) => {
    if ((event.ctrlKey || event.metaKey) && event.key === "s") options.onSave?.();
  });
  parent.append(field);
  const editor: FakeEditor = { options, field, replaced: [], searches: 0, destroyed: false };
  editors.push(editor);
  return {
    text: () => field.value,
    setText(text) {
      field.value = text;
      editor.replaced.push(text);
    },
    setReadOnly(readOnly) {
      field.readOnly = readOnly;
    },
    setWrap(wrap) {
      field.dataset.wrap = String(wrap);
    },
    openSearch() {
      editor.searches += 1;
    },
    focus() {
      field.focus();
    },
    destroy() {
      editor.destroyed = true;
      field.remove();
    },
  };
}

function createDiff(parent: HTMLElement, options: DiffOptions): DiffHandle {
  const element = document.createElement("div");
  element.dataset.diff = options.layout;
  element.textContent = `${options.original}⇄${options.modified}`;
  parent.append(element);
  const diff: FakeDiff = { options, element, destroyed: false };
  diffs.push(diff);
  return {
    changes: () => (options.original === options.modified ? 0 : 1),
    nextChange() {},
    previousChange() {},
    destroy() {
      diff.destroyed = true;
      element.remove();
    },
  };
}

export const fakeEditorModule: EditorModule = { createEditor, createDiff };
