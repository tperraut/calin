// The real CodeMirror, under happy-dom: nothing is laid out, but the document,
// the content's attributes and the state can be read back. A reader's typing is
// a transaction sent to the view, as CodeMirror's own input handling would send.
import { language } from "@codemirror/language";
import { EditorView } from "@codemirror/view";
import { afterEach, describe, expect, it } from "vite-plus/test";
import { until } from "../../test/render";
import editorModule from "./codemirror";
import type { DiffHandle, DiffOptions, EditorHandle, EditorOptions } from "./types";

let parents: HTMLElement[] = [];
let handles: Array<EditorHandle | DiffHandle> = [];

function host(): HTMLElement {
  const parent = document.createElement("div");
  document.body.append(parent);
  parents.push(parent);
  return parent;
}

/** The view under `element`, as CodeMirror finds it from the page. */
function viewIn(element: Element | null | undefined): EditorView {
  const view = element instanceof HTMLElement ? EditorView.findFromDOM(element) : null;
  if (!view) throw new Error("no editor view here");
  return view;
}

function openEditor(options: Partial<EditorOptions> = {}) {
  const parent = host();
  const changes: string[] = [];
  let saves = 0;
  const editor = editorModule.createEditor(parent, {
    label: "src/main.rs",
    path: "src/main.rs",
    text: "fn main() {}\n",
    readOnly: false,
    wrap: false,
    onChange: (text) => changes.push(text),
    onSave: () => {
      saves += 1;
    },
    ...options,
  });
  handles.push(editor);
  const content = (): HTMLElement => {
    const found = parent.querySelector<HTMLElement>(".cm-content");
    if (!found) throw new Error("no editor content");
    return found;
  };
  const view = () => viewIn(parent.querySelector(".cm-editor"));
  return { parent, editor, changes, saves: () => saves, content, view };
}

function openDiff(options: Partial<DiffOptions> = {}) {
  const parent = host();
  const diff = editorModule.createDiff(parent, {
    label: "src/lib.rs",
    path: "src/lib.rs",
    original: "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n",
    modified: "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nK\nl\n",
    layout: "unified",
    ...options,
  });
  handles.push(diff);
  const contents = () => [...parent.querySelectorAll<HTMLElement>(".cm-content")];
  return { parent, diff, contents };
}

const root = document.documentElement;

afterEach(() => {
  for (const handle of handles) handle.destroy();
  for (const parent of parents) parent.remove();
  handles = [];
  parents = [];
  delete root.dataset.theme;
});

describe("CodeMirror editor", () => {
  it("shows the text under its name, and says how to leave it", () => {
    const { editor, content } = openEditor();
    expect(editor.text()).toBe("fn main() {}\n");
    expect(content().getAttribute("aria-label")).toBe("src/main.rs");
    expect(content().getAttribute("aria-description")).toBe(
      "Press Escape, then Tab, to leave the editor.",
    );
    expect(content().getAttribute("aria-readonly")).toBe("false");
  });

  it("tells each edit of the reader, and not a text replaced from outside", () => {
    const { editor, changes, view } = openEditor();
    view().dispatch({ changes: { from: 0, insert: "// hi\n" }, userEvent: "input.type" });
    expect(changes).toEqual(["// hi\nfn main() {}\n"]);

    editor.setText("fn main() { run() }\n");
    expect(editor.text()).toBe("fn main() { run() }\n");
    // The same text again changes nothing at all.
    editor.setText("fn main() { run() }\n");
    view().dispatch({ selection: { anchor: 1 } });
    expect(changes).toEqual(["// hi\nfn main() {}\n"]);
  });

  it("saves on Ctrl+S", () => {
    const { content, saves } = openEditor();
    content().dispatchEvent(
      new KeyboardEvent("keydown", { key: "s", code: "KeyS", ctrlKey: true, bubbles: true }),
    );
    expect(saves()).toBe(1);
  });

  it("locks and unlocks, staying readable either way", () => {
    const { editor, content, view } = openEditor({ readOnly: true });
    expect(content().getAttribute("aria-readonly")).toBe("true");
    expect(view().state.readOnly).toBe(true);
    editor.setReadOnly(false);
    expect(content().getAttribute("aria-readonly")).toBe("false");
    expect(view().state.readOnly).toBe(false);
  });

  it("wraps long lines on request", () => {
    const { editor, content } = openEditor({ wrap: true });
    expect(content().classList.contains("cm-lineWrapping")).toBe(true);
    editor.setWrap(false);
    expect(content().classList.contains("cm-lineWrapping")).toBe(false);
    editor.setWrap(true);
    expect(content().classList.contains("cm-lineWrapping")).toBe(true);
  });

  it("opens its search above the text, and takes the focus", () => {
    const { editor, parent, content } = openEditor();
    editor.openSearch();
    expect(parent.querySelector(".cm-panels-top .cm-search")).not.toBeNull();
    editor.focus();
    expect(content().contains(document.activeElement)).toBe(true);
  });

  it("loads the file's grammar, and follows the page's theme", async () => {
    root.dataset.theme = "dark";
    const { view } = openEditor();
    expect(view().state.facet(EditorView.darkTheme)).toBe(true);
    await until(() => view().state.facet(language)?.name === "rust");

    root.dataset.theme = "light";
    await until(() => !view().state.facet(EditorView.darkTheme));
  });

  it("keeps a file with no known grammar as plain text", async () => {
    const { view, editor } = openEditor({ path: "notes.unknown" });
    await Promise.resolve();
    expect(view().state.facet(language)).toBeNull();
    expect(editor.text()).toBe("fn main() {}\n");
  });

  it("is gone from the page once destroyed, grammar or not", async () => {
    const { editor, parent } = openEditor();
    editor.destroy();
    expect(parent.querySelector(".cm-editor")).toBeNull();
    // The grammar arriving after the end changes nothing and throws nothing.
    await new Promise((resolve) => setTimeout(resolve, 20));
    handles = handles.filter((one) => one !== editor);
  });
});

describe("CodeMirror diff", () => {
  it("shows both sides in one view, read-only, reachable by the keyboard", async () => {
    const { diff, contents } = openDiff();
    const [content] = contents();
    expect(contents()).toHaveLength(1);
    expect(content?.getAttribute("aria-label")).toBe("src/lib.rs");
    expect(content?.getAttribute("tabindex")).toBe("0");
    expect(content?.getAttribute("contenteditable")).toBe("false");
    expect(diff.changes()).toBe(2);
    expect(viewIn(content).state.readOnly).toBe(true);
    await until(() => viewIn(content).state.facet(language)?.name === "rust");
  });

  it("shows the two sides apart, named before and after, when split", () => {
    const { diff, contents } = openDiff({ layout: "split" });
    expect(contents().map((one) => one.getAttribute("aria-label"))).toEqual([
      "src/lib.rs, before",
      "src/lib.rs, after",
    ]);
    expect(diff.changes()).toBe(2);
  });

  it("moves between the changes", () => {
    const { diff, contents } = openDiff({ layout: "split" });
    const after = viewIn(contents()[1]);
    const line = () => after.state.doc.lineAt(after.state.selection.main.head).number;
    diff.nextChange();
    expect(line()).toBe(2);
    diff.nextChange();
    expect(line()).toBe(11);
    diff.previousChange();
    expect(line()).toBe(2);
  });

  it("follows the page's theme on every side, and is gone once destroyed", async () => {
    const { diff, contents, parent } = openDiff({ layout: "split" });
    const views = contents().map(viewIn);
    root.dataset.theme = "dark";
    await until(() => views.every((view) => view.state.facet(EditorView.darkTheme)));
    diff.destroy();
    handles = handles.filter((one) => one !== diff);
    expect(parent.querySelector(".cm-editor")).toBeNull();
  });
});
