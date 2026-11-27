import { flush } from "solid-js";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { HttpError, type Api } from "../api/http";
import type { FileContent } from "../gen/FileContent";
import type { SessionRepo } from "../gen/SessionRepo";
import type { TreeEntry } from "../gen/TreeEntry";
import { conflict, makeSessionRepo } from "../test/fakes";
import { diffs, editorFor, editors, resetFakeEditors } from "../test/fakeEditor";
import { click, find, findByText, mount, type, unmountAll } from "../test/render";
import { act, openWorkspace, type Workspace } from "../test/workspace";
import { loadEditorModule } from "./editor/load";
import FilesPanel from "./FilesPanel";

vi.mock("./editor/codemirror", async () => ({
  default: (await import("../test/fakeEditor")).fakeEditorModule,
}));

const entry = (path: string, kind: TreeEntry["kind"] = "file", ignored = false): TreeEntry => ({
  name: path.slice(path.lastIndexOf("/") + 1),
  path,
  kind,
  size: kind === "file" ? 10 : null,
  ignored,
});

const hashOf = (text: string): string => `hash(${text})`;

/** A little server: a disk per repo, listed, read and written as api.rs says. */
function fakeDisk() {
  const disk: Record<string, Map<string, string>> = {
    api: new Map([
      ["README.md", "# api\n"],
      ["src/main.rs", "fn main() {}\n"],
      ["src/lib.rs", "pub fn lib() {}\n"],
      ["src/api/mod.rs", "// mod\n"],
      ["target/out.log", "built\n"],
    ]),
    docs: new Map([["guide.md", "# Guide\n"]]),
  };
  const content = (path: string, text: string): FileContent => ({
    path,
    text,
    binary: false,
    too_large: false,
    size: text.length,
    hash: hashOf(text),
  });
  const listTree = vi.fn<Api["listTree"]>((_id, repo, path) => {
    const prefix = path === "" ? "" : `${path}/`;
    const seen = new Map<string, TreeEntry>();
    for (const file of disk[repo].keys()) {
      if (!file.startsWith(prefix)) continue;
      const [name, ...deeper] = file.slice(prefix.length).split("/");
      seen.set(
        name,
        deeper.length > 0 ? entry(prefix + name, "dir", name === "target") : entry(prefix + name),
      );
    }
    return Promise.resolve([...seen.values()]);
  });
  const readFile = vi.fn<Api["readFile"]>((_id, repo, path) => {
    const text = disk[repo].get(path);
    return text === undefined
      ? Promise.reject(new HttpError(404, `${path} does not exist`, true))
      : Promise.resolve(content(path, text));
  });
  const writeFile = vi.fn<Api["writeFile"]>((_id, repo, path, body) => {
    if (repo === "docs") return Promise.reject(conflict("docs is attached read-only"));
    const text = disk[repo].get(path);
    if (body.expected_hash === undefined && text !== undefined) {
      return Promise.reject(conflict(`${path} already exists`));
    }
    if (body.expected_hash !== undefined && body.expected_hash !== hashOf(text ?? "")) {
      return Promise.reject(conflict(`${path} changed on disk since it was read`));
    }
    disk[repo].set(path, body.text);
    return Promise.resolve(content(path, body.text));
  });
  return { disk, listTree, readFile, writeFile };
}

const repos: SessionRepo[] = [
  makeSessionRepo("api", { primary: true }),
  makeSessionRepo("docs", { mode: "read" }),
];

type Opened = Workspace & ReturnType<typeof fakeDisk> & { host: HTMLElement };

async function open(patch: Partial<Api> = {}): Promise<Opened> {
  const server = fakeDisk();
  const workspace = await openWorkspace({
    routes: {
      listTree: server.listTree,
      readFile: server.readFile,
      writeFile: server.writeFile,
      ...patch,
    },
    repos,
  });
  workspace.caughtUp();
  const host = mount(() => (
    <FilesPanel store={workspace.store} active={workspace.active()} links={workspace.links} />
  ));
  await act();
  return { ...workspace, ...server, host };
}

const items = (host: HTMLElement): HTMLElement[] => [
  ...host.querySelectorAll<HTMLElement>('[role="treeitem"]'),
];
const item = (host: HTMLElement, name: string): HTMLElement => {
  // Past the arrow that says whether a folder is open.
  const found = items(host).find((one) => one.textContent.replace(/^[▸▾]/, "").startsWith(name));
  if (!found) throw new Error(`no tree item named ${name}`);
  return found;
};
const labels = (host: HTMLElement): string[] =>
  items(host).map(
    (one) => `${"  ".repeat(Number(one.getAttribute("aria-level")) - 1)}${one.textContent}`,
  );

async function press(key: string): Promise<void> {
  document.activeElement?.dispatchEvent(
    new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
  );
  await act();
}

/** Opens `src/main.rs` from the tree and returns its editor. */
async function openMain(app: Opened) {
  await click(item(app.host, "src/"));
  await act();
  await click(item(app.host, "main.rs"));
  await act();
  return editorFor("src/main.rs");
}

const tabs = (host: HTMLElement): string[] =>
  [...host.querySelectorAll('ul[aria-label="Open files"] > li > button:first-child')].map(
    (tab) => tab.textContent,
  );
const editorPane = (host: HTMLElement): HTMLElement => find(host, 'section[aria-label="Editor"]');
const saveButton = (host: HTMLElement): HTMLButtonElement => {
  for (const button of editorPane(host).querySelectorAll<HTMLButtonElement>("button")) {
    if (button.textContent.startsWith("Sav") && button.closest("[hidden]") === null) return button;
  }
  throw new Error("no Save button on screen");
};

/** The agent edits a file behind the editor's back, and the event says so. */
async function agentEdits(app: Opened, path: string, text: string): Promise<void> {
  app.disk.api.set(path, text);
  app.emit({ type: "file_changed", session_repo_id: "api", path, op: "edit" });
  await act();
}

beforeAll(() => loadEditorModule());
beforeEach(resetFakeEditors);
afterEach(() => {
  unmountAll();
  sessionStorage.clear();
});

describe("FilesPanel", () => {
  describe("tree", () => {
    it("has one root per repo, primary first and open, labelled with its mode", async () => {
      const { host, listTree } = await open();
      const tree = find(host, '[role="tree"]');
      expect(tree.getAttribute("aria-label")).toBe("Files of the session's repos");
      expect(labels(host)).toEqual([
        "▾apiWorktree",
        "  ▸src/",
        "  ▸target/ignored",
        "  README.md",
        "▸docsRead only",
      ]);
      expect(item(host, "api").getAttribute("aria-expanded")).toBe("true");
      expect(item(host, "docs").getAttribute("aria-expanded")).toBe("false");
      expect(item(host, "README.md").hasAttribute("aria-expanded")).toBe(false);
      // Only the open root was listed, and only its first level.
      expect(listTree.mock.calls).toEqual([["s1", "api", ""]]);
    });

    it("lists a folder when it is opened, once, until its root is refreshed", async () => {
      const { host, listTree, disk } = await open();
      await click(item(host, "src/"));
      await act();
      expect(labels(host)).toEqual([
        "▾apiWorktree",
        "  ▾src/",
        "    ▸api/",
        "    lib.rs",
        "    main.rs",
        "  ▸target/ignored",
        "  README.md",
        "▸docsRead only",
      ]);
      await click(item(host, "src/"));
      await click(item(host, "src/"));
      await act();
      expect(listTree).toHaveBeenCalledTimes(2);

      disk.api.set("src/health.rs", "// new\n");
      await click(find(host, 'button[aria-label="Refresh api"]'));
      await act();
      expect(labels(host)).toContain("    health.rs");
      // The root and the one open folder under it.
      expect(listTree).toHaveBeenCalledTimes(4);
    });

    it("dims what git ignores, and marks a read-only repo", async () => {
      const { host } = await open();
      expect(item(host, "target/").className).toContain("text-muted");
      expect(item(host, "src/").className).not.toContain("text-muted");
      expect(item(host, "docs").textContent).toContain("Read only");
      // Nothing can be created in a repo attached read-only.
      expect(host.querySelector('button[aria-label="New file in docs"]')).toBeNull();
      expect(host.querySelector('button[aria-label="New file in api"]')).not.toBeNull();
    });

    it("moves with the arrow keys, opens and closes folders, and opens a file with Enter", async () => {
      const app = await open();
      const { host } = app;
      // One tab stop for the whole tree.
      expect(items(host).map((one) => one.tabIndex)).toEqual([0, -1, -1, -1, -1]);

      item(host, "api").focus();
      await press("ArrowDown");
      expect(document.activeElement).toBe(item(host, "src/"));
      expect(item(host, "src/").tabIndex).toBe(0);
      expect(item(host, "api").tabIndex).toBe(-1);

      await press("ArrowRight");
      expect(item(host, "src/").getAttribute("aria-expanded")).toBe("true");
      // Right again steps into the folder.
      await press("ArrowRight");
      expect(document.activeElement).toBe(item(host, "api/"));
      await press("ArrowDown");
      await press("ArrowDown");
      expect(document.activeElement).toBe(item(host, "main.rs"));
      // Left on a file goes to its folder; Left there closes it.
      await press("ArrowLeft");
      expect(document.activeElement).toBe(item(host, "src/"));
      await press("ArrowLeft");
      expect(item(host, "src/").getAttribute("aria-expanded")).toBe("false");
      await press("ArrowLeft");
      expect(document.activeElement).toBe(item(host, "api"));

      await press("End");
      expect(document.activeElement).toBe(item(host, "docs"));
      await press("Home");
      expect(document.activeElement).toBe(item(host, "api"));
      await press("ArrowUp");
      expect(document.activeElement).toBe(item(host, "api"));

      // Type-ahead: the next item starting with the letter, wrapping around.
      await press("r");
      expect(document.activeElement).toBe(item(host, "README.md"));
      await press("Enter");
      expect(tabs(host)).toEqual(["README.md"]);
      expect(editorFor("README.md").field.value).toBe("# api\n");
      expect(item(host, "README.md").getAttribute("aria-selected")).toBe("true");
    });

    it("says so when the session has no repo", async () => {
      const workspace = await openWorkspace({ repos: [] });
      const host = mount(() => (
        <FilesPanel store={workspace.store} active={true} links={workspace.links} />
      ));
      await act();
      expect(host.textContent).toContain("No repo is attached to this session");
      expect(host.querySelector('[role="tree"]')).toBeNull();
    });
  });

  describe("editing", () => {
    it("opens a file in an editor and saves it with the hash it was loaded with", async () => {
      const app = await open();
      const editor = await openMain(app);
      expect(app.readFile).toHaveBeenCalledWith("s1", "api", "src/main.rs");
      expect(editor.options).toMatchObject({
        path: "src/main.rs",
        text: "fn main() {}\n",
        readOnly: false,
      });
      expect(saveButton(app.host).disabled).toBe(true);

      type(editor.field, "fn main() { run() }\n");
      expect(tabs(app.host)).toEqual(["●Unsaved changes:main.rs"]);
      expect(saveButton(app.host).disabled).toBe(false);

      await click(saveButton(app.host));
      await act();
      expect(app.writeFile).toHaveBeenCalledWith("s1", "api", "src/main.rs", {
        text: "fn main() { run() }\n",
        expected_hash: hashOf("fn main() {}\n"),
      });
      expect(tabs(app.host)).toEqual(["main.rs"]);
      expect(saveButton(app.host).disabled).toBe(true);

      // The next save carries the hash the server answered with.
      type(editor.field, "fn main() { run(); stop() }\n");
      editor.field.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true }));
      await act();
      expect(app.writeFile).toHaveBeenLastCalledWith("s1", "api", "src/main.rs", {
        text: "fn main() { run(); stop() }\n",
        expected_hash: hashOf("fn main() { run() }\n"),
      });
      expect(editor.replaced).toEqual([]);
    });

    it("keeps several files open as tabs, each with its own text and dirty mark", async () => {
      const app = await open();
      const main = await openMain(app);
      type(main.field, "edited main\n");
      await click(item(app.host, "lib.rs"));
      await act();
      expect(tabs(app.host)).toEqual(["●Unsaved changes:main.rs", "lib.rs"]);
      // The first editor is still there, hidden, text intact.
      expect(main.destroyed).toBe(false);
      expect(main.field.closest("[hidden]")).not.toBeNull();
      expect(editorFor("src/lib.rs").field.closest("[hidden]")).toBeNull();

      await click(findByText(app.host, 'ul[aria-label="Open files"] button', "main.rs"));
      expect(main.field.closest("[hidden]")).toBeNull();
      expect(main.field.value).toBe("edited main\n");
    });

    it("asks before closing a tab with unsaved changes, and not otherwise", async () => {
      const app = await open();
      const main = await openMain(app);
      await click(item(app.host, "lib.rs"));
      await act();
      await click(find(app.host, 'button[aria-label="Close lib.rs"]'));
      expect(tabs(app.host)).toEqual(["main.rs"]);

      type(main.field, "edited\n");
      await click(find(app.host, 'button[aria-label="Close main.rs"]'));
      expect(editorPane(app.host).textContent).toContain(
        "main.rs has unsaved changes. Close it and lose them?",
      );
      await click(findByText(app.host, "button", "Keep editing"));
      expect(tabs(app.host)).toEqual(["●Unsaved changes:main.rs"]);
      expect(main.field.value).toBe("edited\n");

      await click(find(app.host, 'button[aria-label="Close main.rs"]'));
      await click(findByText(app.host, "button", "Discard changes"));
      expect(tabs(app.host)).toEqual([]);
      expect(main.destroyed).toBe(true);
      expect(editorPane(app.host).textContent).toContain("No file is open.");
    });

    it("warns before the page unloads while a file has unsaved changes, and only then", async () => {
      const app = await open();
      const editor = await openMain(app);
      const unload = (): boolean => {
        const event = new Event("beforeunload", { cancelable: true });
        window.dispatchEvent(event);
        return event.defaultPrevented;
      };
      expect(unload()).toBe(false);
      type(editor.field, "edited\n");
      expect(unload()).toBe(true);
      expect(app.unsaved.at(-1)).toBe("A file has unsaved changes.");
      await click(saveButton(app.host));
      await act();
      expect(unload()).toBe(false);
      expect(app.unsaved.at(-1)).toBeUndefined();
    });

    it("shows a notice instead of an editor for a binary file and one too large", async () => {
      const app = await open({
        readFile: (_id, _repo, path) =>
          Promise.resolve({
            path,
            text: null,
            binary: path === "README.md",
            too_large: path !== "README.md",
            size: path === "README.md" ? 2048 : 5_400_000,
            hash: "h",
          }),
      });
      await click(item(app.host, "README.md"));
      await act();
      expect(editorPane(app.host).textContent).toContain(
        "Binary file (2kB): there is no text to show.",
      );
      await click(item(app.host, "src/"));
      await act();
      await click(item(app.host, "main.rs"));
      await act();
      expect(editorPane(app.host).textContent).toContain(
        "This file is too large to open here (5.4MB).",
      );
      expect(editors).toHaveLength(0);
    });

    it("toggles soft wrap for every open editor and opens the search panel", async () => {
      const app = await open();
      const editor = await openMain(app);
      const wrap = findByText<HTMLButtonElement>(editorPane(app.host), "button", "Wrap");
      const before = editor.field.dataset.wrap;
      await click(wrap);
      expect(editor.field.dataset.wrap).toBe(before === "true" ? "false" : "true");
      expect(wrap.getAttribute("aria-pressed")).toBe(before === "true" ? "false" : "true");

      await click(findByText(editorPane(app.host), "button", "Find"));
      expect(editor.searches).toBe(1);
    });
  });

  describe("a save refused because the file changed on disk", () => {
    async function conflicted() {
      const app = await open();
      const editor = await openMain(app);
      type(editor.field, "mine\n");
      // Not an event yet: the save is what finds out.
      app.disk.api.set("src/main.rs", "the agent's\n");
      await click(saveButton(app.host));
      await act();
      return { app, editor };
    }

    it("keeps the reader's text and says what happened, in the server's words", async () => {
      const { app, editor } = await conflicted();
      const alert = find(editorPane(app.host), '[role="alert"]');
      expect(alert.textContent).toContain("Not saved, your text is kept");
      expect(alert.textContent).toContain("src/main.rs changed on disk since it was read");
      expect([...alert.querySelectorAll("button")].map((button) => button.textContent)).toEqual([
        "Reload from disk",
        "Overwrite",
        "Compare",
      ]);
      expect(editor.field.value).toBe("mine\n");
      expect(tabs(app.host)).toEqual(["●Unsaved changes:main.rs"]);
      expect(app.disk.api.get("src/main.rs")).toBe("the agent's\n");
    });

    it("reloads from disk, discarding the reader's text", async () => {
      const { app, editor } = await conflicted();
      await click(findByText(app.host, "button", "Reload from disk"));
      await act();
      expect(editor.field.value).toBe("the agent's\n");
      expect(editor.replaced).toEqual(["the agent's\n"]);
      expect(tabs(app.host)).toEqual(["main.rs"]);
      expect(editorPane(app.host).querySelector('[role="alert"]')).toBeNull();
    });

    it("overwrites: re-reads the current hash, then saves the reader's text over it", async () => {
      const { app, editor } = await conflicted();
      app.readFile.mockClear();
      await click(findByText(app.host, "button", "Overwrite"));
      await act();
      expect(app.readFile).toHaveBeenCalledTimes(1);
      expect(app.writeFile).toHaveBeenLastCalledWith("s1", "api", "src/main.rs", {
        text: "mine\n",
        expected_hash: hashOf("the agent's\n"),
      });
      expect(app.disk.api.get("src/main.rs")).toBe("mine\n");
      expect(editor.field.value).toBe("mine\n");
      expect(tabs(app.host)).toEqual(["main.rs"]);
      expect(editorPane(app.host).querySelector('[role="alert"]')).toBeNull();
    });

    it("compares the reader's text against the disk version, then goes back to editing", async () => {
      const { app, editor } = await conflicted();
      await click(findByText(app.host, "button", "Compare"));
      await act();
      const [diff] = diffs;
      expect(diff.options).toMatchObject({ original: "the agent's\n", modified: "mine\n" });
      expect(editorPane(app.host).textContent).toContain("your");
      // The editor is out of sight, not gone: the text lives in it.
      expect(editor.destroyed).toBe(false);
      expect(editor.field.closest("[hidden]")).not.toBeNull();

      await click(findByText(app.host, "button", "Back to editing"));
      expect(diff.destroyed).toBe(true);
      expect(editor.field.closest("[hidden]")).toBeNull();
      expect(editor.field.value).toBe("mine\n");
      // Still unresolved: the choice is still offered.
      expect(find(editorPane(app.host), '[role="alert"]').textContent).toContain("Overwrite");
    });
  });

  describe("a file changing on disk while it is open", () => {
    it("reloads it quietly when there are no unsaved edits", async () => {
      const app = await open();
      const editor = await openMain(app);
      await agentEdits(app, "src/main.rs", "fn main() { agent() }\n");
      expect(editor.field.value).toBe("fn main() { agent() }\n");
      // Through the editor's own replace, which is what keeps the caret in place.
      expect(editor.replaced).toEqual(["fn main() { agent() }\n"]);
      expect(editor.destroyed).toBe(false);
      expect(tabs(app.host)).toEqual(["main.rs"]);
      expect(editorPane(app.host).querySelector('[role="status"]')).toBeNull();
      expect(editorPane(app.host).querySelector('[role="alert"]')).toBeNull();
    });

    it("leaves unsaved edits alone and says the file changed, without blocking", async () => {
      const app = await open();
      const editor = await openMain(app);
      type(editor.field, "mine\n");
      await agentEdits(app, "src/main.rs", "the agent's\n");

      expect(editor.field.value).toBe("mine\n");
      expect(editor.replaced).toEqual([]);
      const notice = find(editorPane(app.host), '[role="status"]');
      expect(notice.textContent).toContain(
        "This file changed on disk. Your unsaved edits are kept.",
      );
      expect([...notice.querySelectorAll("button")].map((button) => button.textContent)).toEqual([
        "Reload from disk",
        "Compare",
        "Dismiss",
      ]);
      // Not a dialog: the editor is still there to type in.
      expect(editor.field.closest("[hidden]")).toBeNull();
      type(editor.field, "mine, still typing\n");
      await click(findByText(notice, "button", "Dismiss"));
      expect(editorPane(app.host).querySelector('[role="status"]')).toBeNull();
      expect(editor.field.value).toBe("mine, still typing\n");
    });

    it("does not mistake the event of its own save for someone else's edit", async () => {
      const app = await open();
      const editor = await openMain(app);
      type(editor.field, "mine\n");
      await click(saveButton(app.host));
      await act();
      type(editor.field, "mine, and more\n");
      // event.rs: the reader's own save comes back with no agent.
      app.emit(
        { type: "file_changed", session_repo_id: "api", path: "src/main.rs", op: "edit" },
        null,
      );
      await act();
      expect(editor.field.value).toBe("mine, and more\n");
      expect(editorPane(app.host).querySelector('[role="status"]')).toBeNull();
    });

    it("ignores changes to files that are not open, and events from before it caught up", async () => {
      const server = fakeDisk();
      const workspace = await openWorkspace({
        routes: {
          listTree: server.listTree,
          readFile: server.readFile,
          writeFile: server.writeFile,
        },
        repos,
      });
      // History, replayed while the panel is already on screen.
      const host = mount(() => (
        <FilesPanel store={workspace.store} active={true} links={workspace.links} />
      ));
      await act();
      await click(item(host, "README.md"));
      await act();
      server.readFile.mockClear();
      workspace.emit({
        type: "file_changed",
        session_repo_id: "api",
        path: "README.md",
        op: "edit",
      });
      workspace.caughtUp();
      await act();
      expect(server.readFile).not.toHaveBeenCalled();

      workspace.emit({
        type: "file_changed",
        session_repo_id: "api",
        path: "src/lib.rs",
        op: "edit",
      });
      workspace.emit({ type: "file_changed", session_repo_id: null, path: "/tmp/x", op: "edit" });
      await act();
      expect(server.readFile).not.toHaveBeenCalled();
    });
  });

  describe("a repo attached read-only", () => {
    it("opens its files for reading, with saving disabled and the reason given", async () => {
      const app = await open();
      await click(item(app.host, "docs"));
      await act();
      await click(item(app.host, "guide.md"));
      await act();
      const editor = editorFor("guide.md");
      expect(editor.field.readOnly).toBe(true);
      expect(editorPane(app.host).textContent).toContain(
        "docs is attached read-only: this file can be read here, not saved.",
      );
      const save = saveButton(app.host);
      expect(save.disabled).toBe(true);
      expect(save.title).toBe("This repo is attached read-only");

      // Even if text got in, nothing is sent.
      type(editor.field, "# Changed\n");
      expect(saveButton(app.host).disabled).toBe(true);
      editor.field.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true }));
      await act();
      expect(app.writeFile).not.toHaveBeenCalled();
    });

    it("offers no way to write even once the file is gone from disk, so the server never has to refuse", async () => {
      const app = await open();
      await click(item(app.host, "docs"));
      await act();
      await click(item(app.host, "guide.md"));
      await act();

      app.disk.docs.delete("guide.md");
      app.emit({ type: "file_changed", session_repo_id: "docs", path: "guide.md", op: "delete" });
      await act();
      const pane = editorPane(app.host);
      // Said as it is, without the promise that saving would bring the file back.
      expect(pane.textContent).toContain("This file was deleted on disk.");
      expect(pane.textContent).not.toContain("Saving creates it again");
      expect(saveButton(app.host).disabled).toBe(true);
      expect(
        [...pane.querySelectorAll("button")].map((button) => button.textContent),
      ).not.toContain("Overwrite");
      editorFor("guide.md").field.dispatchEvent(
        new KeyboardEvent("keydown", { key: "s", ctrlKey: true }),
      );
      await act();
      expect(app.writeFile).not.toHaveBeenCalled();
    });
  });

  describe("new file", () => {
    it("asks for a name, creates the file with no expected hash, lists it and opens it", async () => {
      const app = await open();
      await click(item(app.host, "src/"));
      await act();
      await click(find(app.host, 'button[aria-label="New file in src"]'));
      const form = find<HTMLFormElement>(app.host, 'form[aria-label="New file"]');
      expect(form.textContent).toContain("api/src/");
      const create = findByText<HTMLButtonElement>(form, "button", "Create");
      expect(create.disabled).toBe(true);

      type(find<HTMLInputElement>(form, "input"), "health check.rs");
      await click(create);
      await act();
      expect(app.writeFile).toHaveBeenCalledWith("s1", "api", "src/health check.rs", { text: "" });
      expect(app.host.querySelector('form[aria-label="New file"]')).toBeNull();
      expect(tabs(app.host)).toEqual(["health check.rs"]);
      expect(labels(app.host)).toContain("    health check.rs");
      expect(editorFor("src/health check.rs").field.value).toBe("");
    });

    it("creates at the root of a repo, and shows the server's refusal when the name is taken", async () => {
      const app = await open();
      await click(find(app.host, 'button[aria-label="New file in api"]'));
      const form = find<HTMLFormElement>(app.host, 'form[aria-label="New file"]');
      type(find<HTMLInputElement>(form, "input"), "README.md");
      form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
      await act();
      expect(app.writeFile).toHaveBeenCalledWith("s1", "api", "README.md", { text: "" });
      expect(find(form, '[role="alert"]').textContent).toBe("README.md already exists");
      expect(tabs(app.host)).toEqual([]);
      // The form stays for another name, or goes away on Cancel.
      await click(findByText(form, "button", "Cancel"));
      expect(app.host.querySelector('form[aria-label="New file"]')).toBeNull();
    });
  });

  describe("tabs across a reload", () => {
    it("restores the open tabs and the active one from sessionStorage, reading content again", async () => {
      const first = await open();
      const main = await openMain(first);
      await click(item(first.host, "lib.rs"));
      await act();
      type(main.field, "unsaved, and not to be stored\n");
      await click(findByText(first.host, 'ul[aria-label="Open files"] button', "main.rs"));
      const stored = sessionStorage.getItem("calin.files.v1:s1") ?? "";
      expect(JSON.parse(stored)).toEqual({
        tabs: [
          { sessionRepoId: "api", path: "src/main.rs" },
          { sessionRepoId: "api", path: "src/lib.rs" },
        ],
        active: "api\nsrc/main.rs",
      });
      expect(stored).not.toContain("unsaved");
      unmountAll();
      resetFakeEditors();

      const second = await open();
      expect(tabs(second.host)).toEqual(["main.rs", "lib.rs"]);
      expect(second.readFile.mock.calls.map(([, , path]) => path).toSorted()).toEqual([
        "src/lib.rs",
        "src/main.rs",
      ]);
      const restored = editorFor("src/main.rs");
      expect(restored.field.value).toBe("fn main() {}\n");
      expect(restored.field.closest("[hidden]")).toBeNull();
      expect(editorFor("src/lib.rs").field.closest("[hidden]")).not.toBeNull();
    });
  });

  it("opens the file another panel asked for", async () => {
    const app = await open();
    app.requestFile({ sessionRepoId: "api", path: "src/lib.rs" });
    await act();
    expect(tabs(app.host)).toEqual(["lib.rs"]);
    expect(editorFor("src/lib.rs").field.value).toBe("pub fn lib() {}\n");
    flush();
  });
});
