import { createMemo, createSignal, type Accessor } from "solid-js";
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
 * `conflict`: a save was refused because the file changed since it was loaded.
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
  /** Creates an empty file, then opens it. Rejects with the server's error (409: it already exists). */
  create(target: FileTarget): Promise<void>;
};

export const bufferKey = (target: FileTarget): string => `${target.sessionRepoId}\n${target.path}`;

type Entry = {
  buffer: Buffer;
  /** The server's `hash` of the content `saved` came from; absent when the file is not on disk. */
  hash: string | undefined;
  saved: string;
  text: string;
  editable: boolean;
  busy: boolean;
  /** Bumped by every read, so an older answer never lands on a newer one. */
  reads: number;
  setLoad(load: BufferLoad): void;
  setDirty(dirty: boolean): void;
  setBusy(busy: BufferWork | false): void;
  setNotice(notice: BufferNotice | undefined): void;
  setComparing(disk: string | undefined): void;
  bump(): void;
};

const isStatus = (error: unknown, status: number): boolean =>
  error instanceof HttpError && error.status === status;

/** Text that is not on disk: edited since it was loaded, or of a file that is gone from there. */
const unsaved = (entry: Entry): boolean =>
  entry.editable && (entry.hash === undefined || entry.text !== entry.saved);

/** The files open in the editor, as tabs. Plain state behind signals, so it can be tested without a screen. */
export function createBuffers(deps: BuffersDeps): Buffers {
  // Signals publish; decisions are taken on these plain values (a setter shows a tick later).
  let entries: Entry[] = [];
  let current: string | undefined;

  function makeEntry(target: FileTarget): Entry {
    const [load, setLoad] = createSignal<BufferLoad>({ status: "loading" });
    const [dirty, setDirty] = createSignal(false);
    const [busy, setBusy] = createSignal<BufferWork | false>(false);
    const [notice, setNotice] = createSignal<BufferNotice>();
    const [comparing, setComparing] = createSignal<string>();
    const [revision, setRevision] = createSignal(0);
    let revisions = 0;
    const entry: Entry = {
      buffer: {
        key: bufferKey(target),
        target,
        name: target.path.slice(target.path.lastIndexOf("/") + 1),
        load,
        dirty,
        busy,
        notice,
        comparing,
        revision,
        text: () => entry.text,
      },
      hash: undefined,
      saved: "",
      text: "",
      editable: false,
      busy: false,
      reads: 0,
      setLoad,
      setDirty,
      setBusy,
      setNotice,
      setComparing,
      bump() {
        revisions += 1;
        setRevision(revisions);
      },
    };
    return entry;
  }

  const restored = deps.storage?.load();
  if (restored) {
    const seen = new Set<string>();
    for (const target of restored.tabs) {
      const key = bufferKey(target);
      if (seen.has(key)) continue;
      seen.add(key);
      entries.push(makeEntry(target));
    }
    current = entries.find((entry) => entry.buffer.key === restored.active)?.buffer.key;
    current ??= entries.at(0)?.buffer.key;
  }

  const [list, setList] = createSignal<readonly Buffer[]>(entries.map((entry) => entry.buffer));
  const [activeKey, setActiveKey] = createSignal<string | undefined>(current);

  function publish(): void {
    setList(entries.map((entry) => entry.buffer));
    setActiveKey(current);
    // Paths only: content is read again from disk on the next visit.
    deps.storage?.save({
      tabs: entries.map((entry) => entry.buffer.target),
      active: current ?? null,
    });
  }

  const find = (key: string): Entry | undefined =>
    entries.find((entry) => entry.buffer.key === key);

  /** Makes the buffer what the server just sent; the reader's text, if any, is replaced. */
  function adopt(entry: Entry, content: FileContent): void {
    entry.hash = content.hash;
    entry.editable = content.text !== null;
    entry.saved = content.text ?? "";
    entry.text = entry.saved;
    entry.setLoad(
      content.text !== null
        ? { status: "text" }
        : content.binary
          ? { status: "binary", size: content.size }
          : { status: "too_large", size: content.size },
    );
    entry.setDirty(false);
    entry.setNotice(undefined);
    entry.setComparing(undefined);
    entry.bump();
  }

  /** One request at a time per buffer: a second one asked meanwhile is dropped, not queued. */
  async function busily(entry: Entry, kind: BufferWork, work: () => Promise<void>): Promise<void> {
    if (entry.busy) return;
    entry.busy = true;
    entry.setBusy(kind);
    try {
      await work();
    } finally {
      entry.busy = false;
      entry.setBusy(false);
    }
  }

  async function load(entry: Entry): Promise<void> {
    entry.reads += 1;
    const read = entry.reads;
    try {
      const content = await deps.read(entry.buffer.target);
      if (read === entry.reads) adopt(entry, content);
    } catch (error) {
      if (read === entry.reads) entry.setLoad({ status: "error", message: errorMessage(error) });
    }
  }

  async function write(entry: Entry): Promise<void> {
    const text = entry.text;
    const body: WriteFileRequest = { text };
    if (entry.hash !== undefined) body.expected_hash = entry.hash;
    try {
      const content = await deps.write(entry.buffer.target, body);
      entry.hash = content.hash;
      entry.saved = text;
      // The reader may have typed on while the save was on its way.
      entry.setDirty(unsaved(entry));
      entry.setNotice(undefined);
      entry.setComparing(undefined);
    } catch (error) {
      entry.setNotice(
        isStatus(error, 409)
          ? { kind: "conflict", message: errorMessage(error) }
          : { kind: "error", message: errorMessage(error) },
      );
    }
  }

  function open(target: FileTarget): void {
    const key = bufferKey(target);
    if (!find(key)) {
      const entry = makeEntry(target);
      entries = [...entries, entry];
      void load(entry);
    }
    current = key;
    publish();
  }

  return {
    list,
    activeKey,
    active: createMemo(() => list().find((buffer) => buffer.key === activeKey())),
    anyDirty: createMemo(() => list().some((buffer) => buffer.dirty())),
    start() {
      for (const entry of entries) void load(entry);
    },
    open,
    activate(key) {
      if (!find(key)) return;
      current = key;
      publish();
    },
    close(key) {
      const index = entries.findIndex((entry) => entry.buffer.key === key);
      if (index === -1) return;
      entries[index].reads += 1;
      entries = entries.toSpliced(index, 1);
      // The neighbour takes its place: the one after, else the one before.
      if (current === key) current = (entries.at(index) ?? entries.at(-1))?.buffer.key;
      publish();
    },
    edit(key, text) {
      const entry = find(key);
      if (!entry) return;
      entry.text = text;
      entry.setDirty(unsaved(entry));
    },
    async save(key) {
      const entry = find(key);
      if (!entry || !entry.editable || deps.readOnly(entry.buffer.target)) return;
      await busily(entry, "saving", () => write(entry));
    },
    async reload(key) {
      const entry = find(key);
      if (!entry) return;
      await busily(entry, "reading", async () => {
        entry.reads += 1;
        try {
          adopt(entry, await deps.read(entry.buffer.target));
        } catch (error) {
          entry.setNotice({ kind: "error", message: errorMessage(error) });
        }
      });
    },
    async overwrite(key) {
      const entry = find(key);
      if (!entry || !entry.editable || deps.readOnly(entry.buffer.target)) return;
      await busily(entry, "saving", async () => {
        entry.reads += 1;
        try {
          entry.hash = (await deps.read(entry.buffer.target)).hash;
        } catch (error) {
          if (!isStatus(error, 404)) {
            entry.setNotice({ kind: "error", message: errorMessage(error) });
            return;
          }
          // Gone from disk: saving creates it again.
          entry.hash = undefined;
        }
        await write(entry);
      });
    },
    async compare(key) {
      const entry = find(key);
      if (!entry) return;
      await busily(entry, "reading", async () => {
        try {
          const content = await deps.read(entry.buffer.target);
          if (content.text === null) {
            entry.setNotice({
              kind: "error",
              message: "The version on disk is not text that can be compared.",
            });
          } else {
            entry.setComparing(content.text);
          }
        } catch (error) {
          entry.setNotice({ kind: "error", message: errorMessage(error) });
        }
      });
    },
    endCompare(key) {
      find(key)?.setComparing(undefined);
    },
    dismiss(key) {
      find(key)?.setNotice(undefined);
    },
    async changedOnDisk(target, deleted) {
      const entry = find(bufferKey(target));
      // A save or reload in flight will bring the current state of the file itself.
      if (!entry || entry.busy) return;
      entry.reads += 1;
      const read = entry.reads;
      let content: FileContent | undefined;
      if (!deleted) {
        try {
          content = await deps.read(target);
        } catch (error) {
          // Unreadable for now: the buffer stays as it is, and a save will tell.
          if (!isStatus(error, 404)) return;
        }
      }
      if (read !== entry.reads || entry.busy) return;
      if (content === undefined) {
        entry.hash = undefined;
        entry.setNotice({ kind: "deleted" });
        entry.setDirty(unsaved(entry));
        return;
      }
      // Same content as the buffer came from: the event was this buffer's own save.
      if (content.hash === entry.hash) return;
      if (unsaved(entry)) entry.setNotice({ kind: "changed" });
      else adopt(entry, content);
    },
    async create(target) {
      await deps.write(target, { text: "" });
      open(target);
    },
  };
}

const isTarget = (value: unknown): value is FileTarget =>
  typeof value === "object" &&
  value !== null &&
  "sessionRepoId" in value &&
  typeof value.sessionRepoId === "string" &&
  "path" in value &&
  typeof value.path === "string";

/** The open tabs of one session, kept for the life of the browser tab. Storage may be missing or forbidden. */
export function sessionTabStorage(sessionId: string): TabStorage {
  const key = `calin.files.v1:${sessionId}`;
  return {
    load() {
      try {
        const stored: unknown = JSON.parse(sessionStorage.getItem(key) ?? "null");
        if (typeof stored !== "object" || stored === null) return undefined;
        const tabs = "tabs" in stored && Array.isArray(stored.tabs) ? stored.tabs : [];
        const active =
          "active" in stored && typeof stored.active === "string" ? stored.active : null;
        return {
          tabs: tabs.filter(isTarget).map(({ sessionRepoId, path }) => ({ sessionRepoId, path })),
          active,
        };
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
