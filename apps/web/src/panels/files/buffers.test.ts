import { createRoot, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { HttpError } from "../../api/http";
import type { FileContent } from "../../gen/FileContent";
import type { WriteFileRequest } from "../../gen/WriteFileRequest";
import { conflict, deferred, settle } from "../../test/fakes";
import type { FileTarget } from "../registry";
import {
  bufferKey,
  createBuffers,
  sessionTabStorage,
  type Buffer,
  type Buffers,
  type StoredTabs,
} from "./buffers";

const main: FileTarget = { sessionRepoId: "api", path: "src/main.rs" };
const lib: FileTarget = { sessionRepoId: "api", path: "src/lib.rs" };
const guide: FileTarget = { sessionRepoId: "docs", path: "guide.md" };

const content = (
  target: FileTarget,
  text: string | null,
  patch: Partial<FileContent> = {},
): FileContent => ({
  path: target.path,
  text,
  binary: false,
  too_large: false,
  size: text?.length ?? 0,
  hash: `hash(${text})`,
  ...patch,
});

const missing = (): HttpError => new HttpError(404, "no such file", true);

let dispose = (): void => {};
afterEach(() => {
  dispose();
  sessionStorage.clear();
});

function setup(options: { stored?: StoredTabs } = {}) {
  /** What is "on disk", by buffer key. */
  const disk = new Map<string, string>([
    [bufferKey(main), "fn main() {}\n"],
    [bufferKey(lib), "pub fn lib() {}\n"],
    [bufferKey(guide), "# Guide\n"],
  ]);
  const read = vi.fn((target: FileTarget): Promise<FileContent> => {
    const text = disk.get(bufferKey(target));
    return text === undefined ? Promise.reject(missing()) : Promise.resolve(content(target, text));
  });
  const write = vi.fn((target: FileTarget, body: WriteFileRequest): Promise<FileContent> => {
    const text = disk.get(bufferKey(target));
    const current = text === undefined ? undefined : `hash(${text})`;
    if (body.expected_hash !== current) {
      return Promise.reject(
        conflict(
          body.expected_hash === undefined
            ? `${target.path} already exists`
            : `${target.path} changed on disk`,
        ),
      );
    }
    disk.set(bufferKey(target), body.text);
    return Promise.resolve(content(target, body.text));
  });
  let saved: StoredTabs | undefined = options.stored;
  let buffers!: Buffers;
  dispose = createRoot((stop) => {
    buffers = createBuffers({
      read,
      write,
      readOnly: (target) => target.sessionRepoId === "docs",
      storage: { load: () => saved, save: (tabs) => (saved = tabs) },
    });
    return stop;
  });
  const opened = async (target: FileTarget): Promise<Buffer> => {
    buffers.open(target);
    await settle();
    flush();
    const buffer = buffers.list().find((one) => one.key === bufferKey(target));
    if (!buffer) throw new Error("the buffer did not open");
    return buffer;
  };
  /** State of a buffer as the screen would show it. */
  const look = (buffer: Buffer) => {
    flush();
    return {
      load: buffer.load().status,
      text: buffer.text(),
      dirty: buffer.dirty(),
      notice: buffer.notice()?.kind,
      revision: buffer.revision(),
    };
  };
  return { buffers, disk, read, write, opened, look, stored: () => saved };
}

/** The agent edits the file behind the editor's back. */
const agentWrites = (disk: Map<string, string>, target: FileTarget, text: string): void => {
  disk.set(bufferKey(target), text);
};

describe("buffers", () => {
  it("opens a file as a tab, loading its text", async () => {
    const { buffers, look, read } = setup();
    buffers.open(main);
    flush();
    const [buffer] = buffers.list();
    expect(buffer.name).toBe("main.rs");
    expect(buffer.load().status).toBe("loading");
    expect(buffers.activeKey()).toBe(bufferKey(main));
    await settle();
    expect(look(buffer)).toEqual({
      load: "text",
      text: "fn main() {}\n",
      dirty: false,
      notice: undefined,
      revision: 1,
    });
    expect(read).toHaveBeenCalledTimes(1);
  });

  it("shows an open file again instead of opening it twice", async () => {
    const { buffers, opened, read } = setup();
    await opened(main);
    await opened(lib);
    await opened(main);
    expect(buffers.list().map((buffer) => buffer.name)).toEqual(["main.rs", "lib.rs"]);
    expect(buffers.activeKey()).toBe(bufferKey(main));
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("tells a binary file and one too large from text, and a file that cannot be read", async () => {
    const { buffers, read, opened, write } = setup();
    read.mockResolvedValueOnce(content(main, null, { binary: true, size: 2048 }));
    read.mockResolvedValueOnce(content(lib, null, { too_large: true, size: 9_000_000 }));
    read.mockRejectedValueOnce(new HttpError(400, "guide.md leads outside the repo", true));
    const binary = await opened(main);
    const large = await opened(lib);
    const refused = await opened(guide);
    expect(binary.load()).toEqual({ status: "binary", size: 2048 });
    expect(large.load()).toEqual({ status: "too_large", size: 9_000_000 });
    expect(refused.load()).toEqual({ status: "error", message: "guide.md leads outside the repo" });

    // Nothing to save where there is no text.
    await buffers.save(binary.key);
    expect(write).not.toHaveBeenCalled();
  });

  it("marks a buffer dirty while its text differs from the last one loaded or saved", async () => {
    const { buffers, opened, look } = setup();
    const buffer = await opened(main);
    buffers.edit(buffer.key, "fn main() { todo!() }\n");
    expect(look(buffer).dirty).toBe(true);
    expect(buffers.anyDirty()).toBe(true);
    // Typing it back is not an unsaved change.
    buffers.edit(buffer.key, "fn main() {}\n");
    expect(look(buffer).dirty).toBe(false);
    expect(buffers.anyDirty()).toBe(false);
  });

  it("saves with the hash the buffer was loaded with, and adopts the new one", async () => {
    const { buffers, opened, look, write, disk } = setup();
    const buffer = await opened(main);
    buffers.edit(buffer.key, "fn main() { one() }\n");
    await buffers.save(buffer.key);
    expect(write).toHaveBeenLastCalledWith(main, {
      text: "fn main() { one() }\n",
      expected_hash: "hash(fn main() {}\n)",
    });
    expect(look(buffer)).toMatchObject({ dirty: false, notice: undefined });
    expect(disk.get(buffer.key)).toBe("fn main() { one() }\n");

    buffers.edit(buffer.key, "fn main() { two() }\n");
    await buffers.save(buffer.key);
    expect(write).toHaveBeenLastCalledWith(main, {
      text: "fn main() { two() }\n",
      expected_hash: "hash(fn main() { one() }\n)",
    });
    // The editor's text was never replaced by a save.
    expect(look(buffer).revision).toBe(1);
  });

  it("stays dirty when the reader typed on while the save was on its way", async () => {
    const { buffers, opened, look, write } = setup();
    const buffer = await opened(main);
    const answer = deferred<FileContent>();
    write.mockReturnValueOnce(answer.promise);
    buffers.edit(buffer.key, "first\n");
    const saving = buffers.save(buffer.key);
    flush();
    expect(buffer.busy()).toBe("saving");
    // A second save while one is in flight sends nothing.
    await buffers.save(buffer.key);
    expect(write).toHaveBeenCalledTimes(1);

    buffers.edit(buffer.key, "first, then more\n");
    answer.resolve(content(main, "first\n"));
    await saving;
    expect(look(buffer)).toMatchObject({ dirty: true, text: "first, then more\n" });
    expect(buffer.busy()).toBe(false);
  });

  it("keeps the reader's text when a save is refused with 409, and says what happened", async () => {
    const { buffers, opened, look, disk } = setup();
    const buffer = await opened(main);
    buffers.edit(buffer.key, "mine\n");
    agentWrites(disk, main, "the agent's\n");
    await buffers.save(buffer.key);
    expect(look(buffer)).toMatchObject({ text: "mine\n", dirty: true, notice: "conflict" });
    expect(buffer.notice()).toEqual({ kind: "conflict", message: "src/main.rs changed on disk" });
    expect(disk.get(buffer.key)).toBe("the agent's\n");
  });

  it("resolves a conflict by reloading from disk: the reader's text is discarded", async () => {
    const { buffers, opened, look, disk } = setup();
    const buffer = await opened(main);
    buffers.edit(buffer.key, "mine\n");
    agentWrites(disk, main, "the agent's\n");
    await buffers.save(buffer.key);

    await buffers.reload(buffer.key);
    expect(look(buffer)).toEqual({
      load: "text",
      text: "the agent's\n",
      dirty: false,
      notice: undefined,
      revision: 2,
    });
    // And from there a save goes through, on the new hash.
    buffers.edit(buffer.key, "ours\n");
    await buffers.save(buffer.key);
    expect(disk.get(buffer.key)).toBe("ours\n");
  });

  it("resolves a conflict by overwriting: reads the current hash, then saves the reader's text over it", async () => {
    const { buffers, opened, look, disk, read, write } = setup();
    const buffer = await opened(main);
    buffers.edit(buffer.key, "mine\n");
    agentWrites(disk, main, "the agent's\n");
    await buffers.save(buffer.key);
    read.mockClear();
    write.mockClear();

    await buffers.overwrite(buffer.key);
    expect(read).toHaveBeenCalledTimes(1);
    expect(write).toHaveBeenCalledWith(main, {
      text: "mine\n",
      expected_hash: "hash(the agent's\n)",
    });
    expect(disk.get(buffer.key)).toBe("mine\n");
    expect(look(buffer)).toMatchObject({
      text: "mine\n",
      dirty: false,
      notice: undefined,
      revision: 1,
    });
  });

  it("stays in conflict when the file changes again between the re-read and the overwrite", async () => {
    const { buffers, opened, look, disk, write } = setup();
    const buffer = await opened(main);
    buffers.edit(buffer.key, "mine\n");
    agentWrites(disk, main, "the agent's\n");
    await buffers.save(buffer.key);
    write.mockRejectedValueOnce(conflict("src/main.rs changed on disk, again"));
    await buffers.overwrite(buffer.key);
    expect(look(buffer)).toMatchObject({ text: "mine\n", dirty: true, notice: "conflict" });
    expect(buffer.notice()).toMatchObject({ message: "src/main.rs changed on disk, again" });
  });

  it("resolves a conflict by comparing: the disk version is fetched, the reader's text is untouched", async () => {
    const { buffers, opened, look, disk } = setup();
    const buffer = await opened(main);
    buffers.edit(buffer.key, "mine\n");
    agentWrites(disk, main, "the agent's\n");
    await buffers.save(buffer.key);

    await buffers.compare(buffer.key);
    flush();
    expect(buffer.comparing()).toBe("the agent's\n");
    expect(look(buffer)).toMatchObject({
      text: "mine\n",
      dirty: true,
      notice: "conflict",
      revision: 1,
    });
    buffers.endCompare(buffer.key);
    flush();
    expect(buffer.comparing()).toBeUndefined();
    // Overwriting from the comparison closes it.
    await buffers.compare(buffer.key);
    await buffers.overwrite(buffer.key);
    flush();
    expect(buffer.comparing()).toBeUndefined();
    expect(disk.get(buffer.key)).toBe("mine\n");
  });

  it("reports a save that fails for another reason with the server's message", async () => {
    const { buffers, opened, look, write } = setup();
    const buffer = await opened(main);
    buffers.edit(buffer.key, "mine\n");
    write.mockRejectedValueOnce(
      new HttpError(400, "src/main.rs is a symlink out of the repo", true),
    );
    await buffers.save(buffer.key);
    expect(buffer.notice()).toEqual({
      kind: "error",
      message: "src/main.rs is a symlink out of the repo",
    });
    expect(look(buffer)).toMatchObject({ text: "mine\n", dirty: true });
    buffers.dismiss(buffer.key);
    expect(look(buffer).notice).toBeUndefined();
  });

  it("sends nothing for a repo attached read-only", async () => {
    const { buffers, opened, write } = setup();
    const buffer = await opened(guide);
    buffers.edit(buffer.key, "# Changed\n");
    await buffers.save(buffer.key);
    await buffers.overwrite(buffer.key);
    expect(write).not.toHaveBeenCalled();
  });

  it("reloads quietly when the file changes on disk and there are no unsaved edits", async () => {
    const { buffers, opened, look, disk } = setup();
    const buffer = await opened(main);
    agentWrites(disk, main, "the agent's\n");
    await buffers.changedOnDisk(main, false);
    expect(look(buffer)).toEqual({
      load: "text",
      text: "the agent's\n",
      dirty: false,
      notice: undefined,
      revision: 2,
    });
  });

  it("only says so when the file changes on disk under unsaved edits", async () => {
    const { buffers, opened, look, disk } = setup();
    const buffer = await opened(main);
    buffers.edit(buffer.key, "mine\n");
    agentWrites(disk, main, "the agent's\n");
    await buffers.changedOnDisk(main, false);
    expect(look(buffer)).toEqual({
      load: "text",
      text: "mine\n",
      dirty: true,
      notice: "changed",
      revision: 1,
    });
    // The hash was not adopted: saving still conflicts instead of overwriting the agent's edit.
    await buffers.save(buffer.key);
    expect(look(buffer).notice).toBe("conflict");
    expect(disk.get(buffer.key)).toBe("the agent's\n");
  });

  it("recognizes the event of its own save and does nothing", async () => {
    const { buffers, opened, look } = setup();
    const buffer = await opened(main);
    buffers.edit(buffer.key, "mine\n");
    await buffers.save(buffer.key);
    buffers.edit(buffer.key, "mine, and more\n");
    await buffers.changedOnDisk(main, false);
    expect(look(buffer)).toMatchObject({
      text: "mine, and more\n",
      dirty: true,
      notice: undefined,
      revision: 1,
    });
  });

  it("ignores a change to a file that is not open", async () => {
    const { buffers, read } = setup();
    await buffers.changedOnDisk(lib, false);
    expect(read).not.toHaveBeenCalled();
  });

  it("says when the file was deleted on disk, and creates it again on save", async () => {
    const { buffers, opened, look, disk, write } = setup();
    const buffer = await opened(main);
    disk.delete(buffer.key);
    await buffers.changedOnDisk(main, true);
    expect(look(buffer)).toMatchObject({ text: "fn main() {}\n", dirty: true, notice: "deleted" });
    await buffers.save(buffer.key);
    // No hash: the file is created, not overwritten.
    expect(write).toHaveBeenLastCalledWith(main, { text: "fn main() {}\n" });
    expect(look(buffer)).toMatchObject({ dirty: false, notice: undefined });
  });

  it("closes a tab and moves to its neighbour", async () => {
    const { buffers, opened } = setup();
    await opened(main);
    await opened(lib);
    await opened(guide);
    buffers.activate(bufferKey(lib));
    buffers.close(bufferKey(lib));
    flush();
    expect(buffers.list().map((buffer) => buffer.name)).toEqual(["main.rs", "guide.md"]);
    expect(buffers.activeKey()).toBe(bufferKey(guide));
    buffers.close(bufferKey(guide));
    flush();
    expect(buffers.activeKey()).toBe(bufferKey(main));
    buffers.close(bufferKey(main));
    flush();
    expect(buffers.activeKey()).toBeUndefined();
    expect(buffers.active()).toBeUndefined();
  });

  it("creates a file without a hash, opens it, and passes on a 409 when it already exists", async () => {
    const { buffers, write, look } = setup();
    const fresh: FileTarget = { sessionRepoId: "api", path: "src/new file.rs" };
    await buffers.create(fresh);
    expect(write).toHaveBeenCalledWith(fresh, { text: "" });
    await settle();
    flush();
    const [buffer] = buffers.list();
    expect(buffer.target).toEqual(fresh);
    expect(look(buffer)).toMatchObject({ load: "text", text: "", dirty: false });

    await expect(buffers.create(main)).rejects.toMatchObject({
      status: 409,
      message: "src/main.rs already exists",
    });
    expect(buffers.list()).toHaveLength(1);
  });

  it("creates nothing in a repo attached read-only: the request that would be refused is never sent", async () => {
    const { buffers, write } = setup();
    const fresh: FileTarget = { sessionRepoId: "docs", path: "new.md" };
    await expect(buffers.create(fresh)).rejects.toThrow("attached read-only");
    expect(write).not.toHaveBeenCalled();
    expect(buffers.list()).toHaveLength(0);
  });

  it("stores the open tabs and the active one as paths, and restores them", async () => {
    const first = setup();
    await first.opened(main);
    await first.opened(lib);
    first.buffers.edit(bufferKey(lib), "unsaved text that must not be stored\n");
    first.buffers.activate(bufferKey(main));
    expect(first.stored()).toEqual({ tabs: [main, lib], active: bufferKey(main) });
    expect(JSON.stringify(first.stored())).not.toContain("unsaved");
    dispose();

    const second = setup({ stored: first.stored() });
    flush();
    expect(second.buffers.list().map((buffer) => buffer.name)).toEqual(["main.rs", "lib.rs"]);
    expect(second.buffers.activeKey()).toBe(bufferKey(main));
    // Nothing is fetched until the panel is on screen.
    expect(second.read).not.toHaveBeenCalled();
    second.buffers.start();
    await settle();
    expect(second.read).toHaveBeenCalledTimes(2);
    expect(second.look(second.buffers.list()[1])).toMatchObject({
      text: "pub fn lib() {}\n",
      dirty: false,
    });
  });

  describe("never loses the reader's unsaved text", () => {
    async function edited() {
      const app = setup();
      const buffer = await app.opened(main);
      app.buffers.edit(buffer.key, "mine\n");
      return { ...app, buffer };
    }

    it("when a reload fails", async () => {
      const { buffers, buffer, look, read } = await edited();
      read.mockRejectedValueOnce(new HttpError(500, "disk on fire", true));
      await buffers.reload(buffer.key);
      expect(look(buffer)).toMatchObject({ text: "mine\n", dirty: true, notice: "error" });
      expect(buffer.notice()).toEqual({ kind: "error", message: "disk on fire" });
    });

    it("when an overwrite cannot re-read the file: nothing is written", async () => {
      const { buffers, buffer, look, read, write } = await edited();
      read.mockRejectedValueOnce(new HttpError(500, "disk on fire", true));
      await buffers.overwrite(buffer.key);
      expect(write).not.toHaveBeenCalled();
      expect(look(buffer)).toMatchObject({ text: "mine\n", dirty: true, notice: "error" });
    });

    it("when an overwrite finds the file gone: it is created again", async () => {
      const { buffers, buffer, look, disk, write } = await edited();
      disk.delete(buffer.key);
      await buffers.overwrite(buffer.key);
      expect(write).toHaveBeenLastCalledWith(main, { text: "mine\n" });
      expect(disk.get(buffer.key)).toBe("mine\n");
      expect(look(buffer)).toMatchObject({ text: "mine\n", dirty: false, notice: undefined });
    });

    it("when the disk version cannot be compared", async () => {
      const { buffers, buffer, look, read } = await edited();
      read.mockResolvedValueOnce(content(main, null, { binary: true, size: 3 }));
      await buffers.compare(buffer.key);
      expect(buffer.notice()).toEqual({
        kind: "error",
        message: "The version on disk is not text that can be compared.",
      });
      read.mockRejectedValueOnce(new HttpError(500, "disk on fire", true));
      await buffers.compare(buffer.key);
      expect(buffer.notice()).toEqual({ kind: "error", message: "disk on fire" });
      expect(buffer.comparing()).toBeUndefined();
      expect(look(buffer)).toMatchObject({ text: "mine\n", dirty: true, revision: 1 });
    });

    it("when a changed file cannot be read: the buffer stays as it is", async () => {
      const { buffers, buffer, look, read } = await edited();
      read.mockRejectedValueOnce(new HttpError(500, "disk on fire", true));
      await buffers.changedOnDisk(main, false);
      expect(look(buffer)).toMatchObject({ text: "mine\n", dirty: true, notice: undefined });
    });

    it("when a changed file is found gone on read: it says it was deleted", async () => {
      const { buffers, buffer, look, disk } = await edited();
      disk.delete(buffer.key);
      await buffers.changedOnDisk(main, false);
      expect(look(buffer)).toMatchObject({ text: "mine\n", dirty: true, notice: "deleted" });
    });

    it("when a save starts while a change on disk is being read: the save's answer wins", async () => {
      const { buffers, buffer, look, read, disk } = await edited();
      const slow = deferred<FileContent>();
      read.mockReturnValueOnce(slow.promise);
      const changed = buffers.changedOnDisk(main, false);
      const saving = buffers.save(buffer.key);
      slow.resolve(content(main, "the agent's\n"));
      await Promise.all([changed, saving]);
      expect(disk.get(buffer.key)).toBe("mine\n");
      expect(look(buffer)).toMatchObject({ text: "mine\n", dirty: false, notice: undefined });
    });

    it("when a change event arrives while a save is in flight: it is left to the save", async () => {
      const { buffers, buffer, read, write } = await edited();
      const answer = deferred<FileContent>();
      write.mockReturnValueOnce(answer.promise);
      read.mockClear();
      const saving = buffers.save(buffer.key);
      await buffers.changedOnDisk(main, false);
      expect(read).not.toHaveBeenCalled();
      answer.resolve(content(main, "mine\n"));
      await saving;
    });
  });

  it("drops a read that answers after its tab was closed and opened again", async () => {
    const { buffers, read, look, opened } = setup();
    const slow = deferred<FileContent>();
    read.mockReturnValueOnce(slow.promise);
    buffers.open(main);
    buffers.close(bufferKey(main));
    const buffer = await opened(main);
    slow.reject(new HttpError(500, "late", true));
    await settle();
    expect(look(buffer)).toMatchObject({ load: "text", text: "fn main() {}\n" });
  });

  it("ignores what is asked of a tab that is not open", async () => {
    const { buffers, opened, read, write } = setup();
    await opened(main);
    read.mockClear();
    const gone = bufferKey(lib);
    buffers.activate(gone);
    buffers.close(gone);
    buffers.edit(gone, "text");
    buffers.endCompare(gone);
    buffers.dismiss(gone);
    await buffers.save(gone);
    await buffers.reload(gone);
    await buffers.overwrite(gone);
    await buffers.compare(gone);
    flush();
    expect(buffers.activeKey()).toBe(bufferKey(main));
    expect(buffers.list()).toHaveLength(1);
    expect(read).not.toHaveBeenCalled();
    expect(write).not.toHaveBeenCalled();
  });

  it("closing a tab in the background keeps the active one", async () => {
    const { buffers, opened } = setup();
    await opened(main);
    await opened(lib);
    buffers.close(bufferKey(main));
    flush();
    expect(buffers.activeKey()).toBe(bufferKey(lib));
  });

  it("restores each tab once, and the first one when the active one is not among them", () => {
    const { buffers } = setup({ stored: { tabs: [main, lib, main], active: "elsewhere" } });
    flush();
    expect(buffers.list().map((buffer) => buffer.name)).toEqual(["main.rs", "lib.rs"]);
    expect(buffers.activeKey()).toBe(bufferKey(main));
  });

  it("keeps tabs per session in sessionStorage, and survives what it finds there", () => {
    const storage = sessionTabStorage("s1");
    expect(storage.load()).toBeUndefined();
    storage.save({ tabs: [main], active: bufferKey(main) });
    expect(sessionTabStorage("s1").load()).toEqual({ tabs: [main], active: bufferKey(main) });
    expect(sessionTabStorage("s2").load()).toBeUndefined();

    sessionStorage.setItem("calin.files.v1:s1", "{not json");
    expect(storage.load()).toBeUndefined();
    sessionStorage.setItem(
      "calin.files.v1:s1",
      JSON.stringify({ tabs: [main, { path: 3 }, "x", { ...lib, text: "leaked" }], active: 7 }),
    );
    expect(storage.load()).toEqual({ tabs: [main, lib], active: null });
    sessionStorage.setItem("calin.files.v1:s1", JSON.stringify({ active: bufferKey(main) }));
    expect(storage.load()).toEqual({ tabs: [], active: bufferKey(main) });
    sessionStorage.setItem("calin.files.v1:s1", "7");
    expect(storage.load()).toBeUndefined();
  });

  it("restores nothing, and stores nothing, when sessionStorage is forbidden", () => {
    const storage = sessionTabStorage("s1");
    const getItem = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new DOMException("denied", "SecurityError");
    });
    const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new DOMException("denied", "SecurityError");
    });
    try {
      expect(storage.load()).toBeUndefined();
      expect(() => storage.save({ tabs: [main], active: null })).not.toThrow();
    } finally {
      getItem.mockRestore();
      setItem.mockRestore();
    }
  });
});
