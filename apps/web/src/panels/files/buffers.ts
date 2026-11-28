import { createMemo, createSignal, type Accessor, type Setter } from "solid-js";
import { HttpError } from "../../api/http";
import type { FileContent } from "../../gen/FileContent";
import type { WriteFileRequest } from "../../gen/WriteFileRequest";
import { errorMessage } from "../../lib/errors";
import type { FileTarget } from "../registry";

export type BufferLoad =
  | { status: "loading" }
  | { status: "text" }
  | { status: "binary"; size: number }
  | { status: "too_large"; size: number }
  | { status: "error"; message: string };

/**
 * What stands between a buffer and the file on disk.
 * `conflict`: a save was refused with 409. The server's message says why; of
 * its causes (api.rs) only two can happen here, the file changed since it was
 * loaded or it exists again after being deleted, because nothing is ever sent
 * for a repo attached `read`. `message` is the server's, shown as it is.
 * `changed`: the file changed on disk while the buffer has unsaved edits.
 * `deleted`: the file is gone from disk; saving creates it again.
 * `error`: a save or a reload failed for another reason.
 */
export type BufferNotice =
  | { kind: "conflict"; message: string }
  | { kind: "changed" }
  | { kind: "deleted" }
  | { kind: "error"; message: string };

export type BufferWork = "saving" | "reading";

/** One open file. The editor holds its text; this holds what the server last said about it. */
export type Buffer = {
  readonly key: string;
  readonly target: FileTarget;
  /** The last segment of the path. */
  readonly name: string;
  load: Accessor<BufferLoad>;
  /** True while the text differs from what was last loaded or saved. */
  dirty: Accessor<boolean>;
  /** What is in flight, if anything: a save (an overwrite included), or a read from disk. */
  busy: Accessor<BufferWork | false>;
  notice: Accessor<BufferNotice | undefined>;
  /** The version on disk, while the reader compares it with their own. */
  comparing: Accessor<string | undefined>;
  /** Moves when the text was replaced from outside the editor: the editor then takes `text()`. */
  revision: Accessor<number>;
  text(): string;
};

export type StoredTabs = { tabs: FileTarget[]; active: string | null };

export type TabStorage = {
  load(): StoredTabs | undefined;
  save(tabs: StoredTabs): void;
};

export type BuffersDeps = {
  read(target: FileTarget): Promise<FileContent>;
  write(target: FileTarget, body: WriteFileRequest): Promise<FileContent>;
  /** A repo attached `read` refuses every write (api.rs): nothing is sent for it. */
  readOnly(target: FileTarget): boolean;
  storage?: TabStorage;
};

export type Buffers = {
  list: Accessor<readonly Buffer[]>;
  activeKey: Accessor<string | undefined>;
  active: Accessor<Buffer | undefined>;
  anyDirty: Accessor<boolean>;
  /** Loads the tabs restored from storage. Call once, after the first render. */
  start(): void;
  /** Shows the file in a tab, opening one if it has none. */
  open(target: FileTarget): void;
  activate(key: string): void;
  /** Closes the tab, unsaved text included: asking first is the caller's business. */
  close(key: string): void;
  /** The reader typed. */
  edit(key: string, text: string): void;
  /** Sends the text with the hash it was loaded with. A 409 becomes a `conflict` notice; the text stays. */
  save(key: string): Promise<void>;
  /** Discards the reader's text for what is on disk. */
  reload(key: string): Promise<void>;
  /** Re-reads the current hash, then saves the reader's text over it. */
  overwrite(key: string): Promise<void>;
  /** Fetches the version on disk and shows it against the reader's text. */
  compare(key: string): Promise<void>;
  endCompare(key: string): void;
  dismiss(key: string): void;
  /** A `file_changed` event named this file. Reloads it quietly, or says so when there are unsaved edits. */
  changedOnDisk(target: FileTarget, deleted: boolean): Promise<void>;
  /**
   * Creates an empty file, then opens it. Rejects with the server's error
   * (409: it already exists). Nothing is sent for a repo attached `read`.
   */
  create(target: FileTarget): Promise<void>;
};

export const bufferKey = (target: FileTarget): string => `${target.sessionRepoId}\n${target.path}`;

const isStatus = (error: unknown, status: number): boolean =>
  error instanceof HttpError && error.status === status;

const errorNotice = (error: unknown): BufferNotice => ({
  kind: "error",
  message: errorMessage(error),
});

/** A refused save: a 409 is a conflict the reader resolves, anything else an error. */
const saveFailure = (error: unknown): BufferNotice =>
  isStatus(error, 409) ? { kind: "conflict", message: errorMessage(error) } : errorNotice(error);

/** What the editor can do with what the server sent. */
function loadOf(content: FileContent): BufferLoad {
  if (content.text !== null) return { status: "text" };
  return content.binary
    ? { status: "binary", size: content.size }
    : { status: "too_large", size: content.size };
}

/** Each file once, in the order first seen. */
function uniqueTargets(targets: readonly FileTarget[]): FileTarget[] {
  const seen = new Set<string>();
  return targets.filter((target) => {
    const key = bufferKey(target);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/** One open file: what the server last said about it, and the reader's text. */
class OpenFile {
  readonly buffer: Buffer;
  /** The server's `hash` of the content `saved` came from; absent when the file is not on disk. */
  hash: string | undefined = undefined;
  saved = "";
  text = "";
  editable = false;
  busy = false;
  /** Bumped by every read, so an older answer never lands on a newer one. */
  reads = 0;
  readonly setLoad: Setter<BufferLoad>;
  readonly setBusy: Setter<BufferWork | false>;
  readonly setNotice: Setter<BufferNotice | undefined>;
  readonly setComparing: Setter<string | undefined>;
  private readonly setDirty: Setter<boolean>;
  private readonly setRevision: Setter<number>;
  private revisions = 0;

  constructor(target: FileTarget) {
    const [load, setLoad] = createSignal<BufferLoad>({ status: "loading" });
    const [dirty, setDirty] = createSignal(false);
    const [busy, setBusy] = createSignal<BufferWork | false>(false);
    const [notice, setNotice] = createSignal<BufferNotice>();
    const [comparing, setComparing] = createSignal<string>();
    const [revision, setRevision] = createSignal(0);
    this.buffer = {
      key: bufferKey(target),
      target,
      name: target.path.slice(target.path.lastIndexOf("/") + 1),
      load,
      dirty,
      busy,
      notice,
      comparing,
      revision,
      text: () => this.text,
    };
    this.setLoad = setLoad;
    this.setDirty = setDirty;
    this.setBusy = setBusy;
    this.setNotice = setNotice;
    this.setComparing = setComparing;
    this.setRevision = setRevision;
  }

  /** Text that is not on disk: edited since it was loaded, or of a file that is gone from there. */
  unsaved(): boolean {
    return this.editable && (this.hash === undefined || this.text !== this.saved);
  }

  /** Starts a read; its answer counts only while `reads` still equals what this returns. */
  startRead(): number {
    this.reads += 1;
    return this.reads;
  }

  /** Makes the buffer what the server just sent; the reader's text, if any, is replaced. */
  adopt(content: FileContent): void {
    this.hash = content.hash;
    this.editable = content.text !== null;
    this.saved = content.text ?? "";
    this.text = this.saved;
    this.setLoad(loadOf(content));
    this.setDirty(false);
    this.setNotice(undefined);
    this.setComparing(undefined);
    this.revisions += 1;
    this.setRevision(this.revisions);
  }

  edit(text: string): void {
    this.text = text;
    this.setDirty(this.unsaved());
  }

  /** `text` is on disk now, as `hash`. */
  written(text: string, hash: string): void {
    this.hash = hash;
    this.saved = text;
    // The reader may have typed on while the save was on its way.
    this.setDirty(this.unsaved());
    this.setNotice(undefined);
    this.setComparing(undefined);
  }

  deleted(): void {
    this.hash = undefined;
    this.setNotice({ kind: "deleted" });
    this.setDirty(this.unsaved());
  }

  /** Someone else wrote `content`: taken quietly, unless the reader has text it would replace. */
  changed(content: FileContent): void {
    // Same content as the buffer came from: the event was this buffer's own save.
    if (content.hash === this.hash) return;
    if (this.unsaved()) this.setNotice({ kind: "changed" });
    else this.adopt(content);
  }

  compareWith(content: FileContent): void {
    if (content.text === null) {
      this.setNotice({
        kind: "error",
        message: "The version on disk is not text that can be compared.",
      });
    } else {
      this.setComparing(content.text);
    }
  }
}

/** The files open in the editor, as tabs. Plain state behind signals, so it can be tested without a screen. */
export function createBuffers(deps: BuffersDeps): Buffers {
  return new OpenFiles(deps);
}

// A class rather than a closure of methods: each method then stands, and is measured, on its own.
// `private`, not `#`: the complexity report does not see `#` methods.
class OpenFiles implements Buffers {
  readonly list: Accessor<readonly Buffer[]>;
  readonly activeKey: Accessor<string | undefined>;
  readonly active: Accessor<Buffer | undefined>;
  readonly anyDirty: Accessor<boolean>;
  private readonly setList: Setter<readonly Buffer[]>;
  private readonly setActiveKey: Setter<string | undefined>;
  private readonly deps: BuffersDeps;
  // Signals publish; decisions are taken on these plain values (a setter shows a tick later).
  private files: OpenFile[];
  private current: string | undefined;

  constructor(deps: BuffersDeps) {
    this.deps = deps;
    const restored = deps.storage?.load();
    this.files = uniqueTargets(restored?.tabs ?? []).map((target) => new OpenFile(target));
    this.current = (this.find(restored?.active ?? "") ?? this.files.at(0))?.buffer.key;
    [this.list, this.setList] = createSignal<readonly Buffer[]>(this.buffers());
    [this.activeKey, this.setActiveKey] = createSignal<string | undefined>(this.current);
    this.active = createMemo(() => this.list().find((buffer) => buffer.key === this.activeKey()));
    this.anyDirty = createMemo(() => this.list().some((buffer) => buffer.dirty()));
  }

  start(): void {
    for (const file of this.files) void this.load(file);
  }

  open(target: FileTarget): void {
    const key = bufferKey(target);
    if (!this.find(key)) {
      const file = new OpenFile(target);
      this.files = [...this.files, file];
      void this.load(file);
    }
    this.current = key;
    this.publish();
  }

  activate(key: string): void {
    if (!this.find(key)) return;
    this.current = key;
    this.publish();
  }

  close(key: string): void {
    const index = this.files.findIndex((file) => file.buffer.key === key);
    if (index === -1) return;
    this.files[index].startRead();
    this.files = this.files.toSpliced(index, 1);
    // The neighbour takes its place: the one after, else the one before.
    if (this.current === key)
      this.current = (this.files.at(index) ?? this.files.at(-1))?.buffer.key;
    this.publish();
  }

  edit(key: string, text: string): void {
    this.find(key)?.edit(text);
  }

  async save(key: string): Promise<void> {
    const file = this.writable(key);
    if (file) await this.busily(file, "saving", () => this.write(file));
  }

  async reload(key: string): Promise<void> {
    const file = this.find(key);
    if (!file) return;
    await this.busily(file, "reading", async () => {
      file.startRead();
      try {
        file.adopt(await this.deps.read(file.buffer.target));
      } catch (error) {
        file.setNotice(errorNotice(error));
      }
    });
  }

  async overwrite(key: string): Promise<void> {
    const file = this.writable(key);
    if (!file) return;
    await this.busily(file, "saving", async () => {
      file.startRead();
      if (await this.readHash(file)) await this.write(file);
    });
  }

  async compare(key: string): Promise<void> {
    const file = this.find(key);
    if (!file) return;
    await this.busily(file, "reading", async () => {
      try {
        file.compareWith(await this.deps.read(file.buffer.target));
      } catch (error) {
        file.setNotice(errorNotice(error));
      }
    });
  }

  endCompare(key: string): void {
    this.find(key)?.setComparing(undefined);
  }

  dismiss(key: string): void {
    this.find(key)?.setNotice(undefined);
  }

  async changedOnDisk(target: FileTarget, deleted: boolean): Promise<void> {
    const file = this.find(bufferKey(target));
    // A save or reload in flight will bring the current state of the file itself.
    if (!file || file.busy) return;
    const read = file.startRead();
    const disk = deleted ? "gone" : await this.readOnDisk(target);
    // Unreadable for now: the buffer stays as it is, and a save will tell.
    if (disk === "unreadable" || read !== file.reads || file.busy) return;
    if (disk === "gone") file.deleted();
    else file.changed(disk);
  }

  async create(target: FileTarget): Promise<void> {
    // Of the three refusals a write can meet (api.rs), this one is known beforehand: it is never sent.
    if (this.deps.readOnly(target)) {
      throw new Error("This repo is attached read-only: nothing can be created in it.");
    }
    await this.deps.write(target, { text: "" });
    this.open(target);
  }

  private buffers(): Buffer[] {
    return this.files.map((file) => file.buffer);
  }

  private publish(): void {
    this.setList(this.buffers());
    this.setActiveKey(this.current);
    // Paths only: content is read again from disk on the next visit.
    this.deps.storage?.save({
      tabs: this.files.map((file) => file.buffer.target),
      active: this.current ?? null,
    });
  }

  private find(key: string): OpenFile | undefined {
    return this.files.find((file) => file.buffer.key === key);
  }

  /** The open file `key` if its text can be sent: it is text, and its repo takes writes. */
  private writable(key: string): OpenFile | undefined {
    const file = this.find(key);
    return file?.editable && !this.deps.readOnly(file.buffer.target) ? file : undefined;
  }

  /** One request at a time per buffer: a second one asked meanwhile is dropped, not queued. */
  private async busily(file: OpenFile, kind: BufferWork, work: () => Promise<void>): Promise<void> {
    if (file.busy) return;
    file.busy = true;
    file.setBusy(kind);
    try {
      await work();
    } finally {
      file.busy = false;
      file.setBusy(false);
    }
  }

  private async load(file: OpenFile): Promise<void> {
    const read = file.startRead();
    try {
      const content = await this.deps.read(file.buffer.target);
      if (read === file.reads) file.adopt(content);
    } catch (error) {
      if (read === file.reads) file.setLoad({ status: "error", message: errorMessage(error) });
    }
  }

  private async write(file: OpenFile): Promise<void> {
    // A read asked before the save may answer after it, with the version the save replaces.
    file.startRead();
    const text = file.text;
    const body: WriteFileRequest = { text };
    if (file.hash !== undefined) body.expected_hash = file.hash;
    try {
      file.written(text, (await this.deps.write(file.buffer.target, body)).hash);
    } catch (error) {
      file.setNotice(saveFailure(error));
    }
  }

  /** Takes the hash on disk before an overwrite; false when it cannot be known. */
  private async readHash(file: OpenFile): Promise<boolean> {
    try {
      file.hash = (await this.deps.read(file.buffer.target)).hash;
    } catch (error) {
      if (!isStatus(error, 404)) {
        file.setNotice(errorNotice(error));
        return false;
      }
      // Gone from disk: saving creates it again.
      file.hash = undefined;
    }
    return true;
  }

  /** The file as it is on disk now, or why it cannot be had. */
  private async readOnDisk(target: FileTarget): Promise<FileContent | "gone" | "unreadable"> {
    try {
      return await this.deps.read(target);
    } catch (error) {
      return isStatus(error, 404) ? "gone" : "unreadable";
    }
  }
}

const isTarget = (value: unknown): value is FileTarget =>
  typeof value === "object" &&
  value !== null &&
  "sessionRepoId" in value &&
  typeof value.sessionRepoId === "string" &&
  "path" in value &&
  typeof value.path === "string";

/** The tabs as stored, if that is what `stored` is; only the paths of well-formed targets are kept. */
function parseStoredTabs(stored: unknown): StoredTabs | undefined {
  if (typeof stored !== "object" || stored === null) return undefined;
  const tabs: unknown[] = "tabs" in stored && Array.isArray(stored.tabs) ? stored.tabs : [];
  const active = "active" in stored && typeof stored.active === "string" ? stored.active : null;
  return {
    tabs: tabs.filter(isTarget).map(({ sessionRepoId, path }) => ({ sessionRepoId, path })),
    active,
  };
}

/** The open tabs of one session, kept for the life of the browser tab. Storage may be missing or forbidden. */
export function sessionTabStorage(sessionId: string): TabStorage {
  const key = `calin.files.v1:${sessionId}`;
  return {
    load() {
      try {
        return parseStoredTabs(JSON.parse(sessionStorage.getItem(key) ?? "null"));
      } catch {
        return undefined;
      }
    },
    save(tabs) {
      try {
        sessionStorage.setItem(key, JSON.stringify(tabs));
      } catch {
        // Nothing to do: the tabs are simply not restored next time.
      }
    },
  };
}
