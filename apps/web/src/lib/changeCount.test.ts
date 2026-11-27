import { describe, expect, it } from "vite-plus/test";
import type { RepoChanges } from "../gen/RepoChanges";
import type { FileChange } from "../state/sessionReducer";
import { changedFileCount } from "./changeCount";

const event = (revision: number, path: string, patch: Partial<FileChange> = {}): FileChange => ({
  key: `api\n${path}`,
  sessionRepoId: "api",
  path,
  op: "edit",
  by: "agent",
  agentId: "a1",
  ts: revision,
  revision,
  ...patch,
});

const listed = (repo: string, ...paths: string[]): RepoChanges => ({
  session_repo_id: repo,
  name: repo,
  branch: null,
  base_ref: null,
  files: paths.map((path) => ({
    path,
    old_path: null,
    status: "modified",
    added: 1,
    removed: 0,
    binary: false,
  })),
  added: paths.length,
  removed: 0,
});

describe("changedFileCount", () => {
  it("counts the files the events named until the server's list is read", () => {
    expect(changedFileCount(undefined, 0, [])).toBe(0);
    expect(changedFileCount(undefined, 0, [event(1, "a.rs"), event(2, "b.rs")])).toBe(2);
  });

  it("counts the server's list once read, whatever the events said", () => {
    // A shell edited three files: no event named them.
    expect(changedFileCount([listed("api", "a.rs", "b.rs", "c.rs")], 0, [])).toBe(3);
    // An edit was reverted: the event stays, the list no longer has the file.
    expect(changedFileCount([listed("api", "a.rs")], 2, [event(1, "a.rs"), event(2, "b.rs")])).toBe(
      1,
    );
    expect(changedFileCount([listed("api"), listed("web")], 5, [event(1, "a.rs")])).toBe(0);
  });

  it("adds what events named since the list was read and the list does not hold", () => {
    const list = [listed("api", "a.rs"), listed("web", "index.ts")];
    const since = [
      event(1, "old.rs"),
      // Already in the list: not counted twice.
      event(3, "a.rs"),
      event(4, "new.rs"),
      // The same path in another repo is another file.
      event(5, "a.rs", { key: "web\na.rs", sessionRepoId: "web" }),
      // A deletion adds nothing, and neither does a file outside every repo.
      event(6, "gone.rs", { op: "delete" }),
      event(7, "/tmp/x", { key: "\n/tmp/x", sessionRepoId: null }),
    ];
    expect(changedFileCount(list, 2, since)).toBe(4);
    // The next read accounts for all of them.
    expect(changedFileCount(list, 7, since)).toBe(2);
  });
});
