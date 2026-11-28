// CodeEditor over the real CodeMirror (see codemirror.test.ts for why it runs under happy-dom).
import { EditorView } from "@codemirror/view";
import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { find, mount, unmountAll, until } from "../../test/render";
import { CodeEditor, type EditorControls } from "./CodeEditor";
import { loadEditorModule } from "./load";

// Real by default; a test makes one load fail, as a chunk lost to the network would.
vi.mock("./load", async (importOriginal) => {
  const real = await importOriginal<typeof import("./load")>();
  return { loadEditorModule: vi.fn(real.loadEditorModule) };
});

afterEach(() => unmountAll());

function open() {
  const [text, setText] = createSignal("first\n");
  const [revision, setRevision] = createSignal(0);
  const [readOnly, setReadOnly] = createSignal(false);
  const [wrap, setWrap] = createSignal(false);
  const edits: string[] = [];
  const saves: number[] = [];
  const controls: Array<EditorControls | undefined> = [];
  const host = mount(() => (
    <CodeEditor
      label="notes.md"
      path="notes.md"
      text={text}
      revision={revision()}
      readOnly={readOnly()}
      wrap={wrap()}
      onChange={(next) => edits.push(next)}
      onSave={() => saves.push(1)}
      controls={(next) => controls.push(next)}
    />
  ));
  const content = () => find(host, ".cm-content");
  const view = () => {
    const found = EditorView.findFromDOM(find(host, ".cm-editor"));
    if (!found) throw new Error("no editor view");
    return found;
  };
  return {
    host,
    content,
    view,
    edits,
    saves,
    controls,
    setText,
    setRevision,
    setReadOnly,
    setWrap,
  };
}

describe("CodeEditor", () => {
  it("says it is loading, then shows the file under its name", async () => {
    const { host, content, view } = open();
    expect(host.textContent).toContain("Loading the editor…");
    await until(() => host.querySelector(".cm-content") !== null);
    expect(host.textContent).not.toContain("Loading the editor…");
    expect(content().getAttribute("aria-label")).toBe("notes.md");
    expect(view().state.doc.toString()).toBe("first\n");
  });

  it("takes the text again when it or the revision moves, without calling it an edit", async () => {
    const { host, view, setText, setRevision, edits } = open();
    await until(() => host.querySelector(".cm-content") !== null);
    setText("second\n");
    flush();
    expect(view().state.doc.toString()).toBe("second\n");
    setText("third\n");
    setRevision(1);
    flush();
    expect(view().state.doc.toString()).toBe("third\n");
    expect(edits).toEqual([]);
  });

  it("tells the reader's edits and saves", async () => {
    const { host, view, content, edits, saves } = open();
    await until(() => host.querySelector(".cm-content") !== null);
    view().dispatch({ changes: { from: 0, insert: "> " }, userEvent: "input.type" });
    expect(edits).toEqual(["> first\n"]);
    content().dispatchEvent(
      new KeyboardEvent("keydown", { key: "s", code: "KeyS", ctrlKey: true, bubbles: true }),
    );
    expect(saves).toEqual([1]);
  });

  it("follows the lock and the wrapping", async () => {
    const { host, content, setReadOnly, setWrap } = open();
    await until(() => host.querySelector(".cm-content") !== null);
    setReadOnly(true);
    setWrap(true);
    flush();
    expect(content().getAttribute("aria-readonly")).toBe("true");
    expect(content().classList.contains("cm-lineWrapping")).toBe(true);
  });

  it("hands the toolbar its controls, and takes them back when it goes", async () => {
    const { host, content, controls } = open();
    await until(() => controls.length === 1);
    controls[0]?.openSearch();
    expect(host.querySelector(".cm-search")).not.toBeNull();
    controls[0]?.focus();
    expect(content().contains(document.activeElement)).toBe(true);
    unmountAll();
    expect(controls).toHaveLength(2);
    expect(controls[1]).toBeUndefined();
  });

  it("says so when the editor's code cannot be loaded", async () => {
    vi.mocked(loadEditorModule).mockRejectedValueOnce(new Error("chunk lost"));
    const { host, controls } = open();
    await until(() => host.querySelector('[role="alert"]') !== null);
    expect(find(host, '[role="alert"]').textContent).toBe(
      "The editor could not be loaded: chunk lost",
    );
    expect(host.textContent).not.toContain("Loading the editor…");
    expect(controls).toEqual([]);
  });

  it("makes no editor when it is gone before the code arrives", async () => {
    const loaded = loadEditorModule();
    const { controls } = open();
    unmountAll();
    await loaded;
    await Promise.resolve();
    expect(document.querySelector(".cm-editor")).toBeNull();
    expect(controls).toEqual([undefined]);
  });
});
