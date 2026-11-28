import { describe, expect, it } from "vite-plus/test";
import { makeSession } from "../test/fakes";
import { isSettled, sessionTreeOf, treeRevision } from "./sessionTree";

const root = makeSession("s1", 100, { title: "Root" });
const childA = makeSession("s2", 300, { parent_session_id: "s1", root_session_id: "s1" });
const childB = makeSession("s3", 200, { parent_session_id: "s1", root_session_id: "s1" });
const other = makeSession("s9", 50);

describe("sessionTreeOf", () => {
  it("holds the sessions that share the root, oldest first", () => {
    const tree = sessionTreeOf([childA, other, childB, root], root);
    expect(tree.rootId).toBe("s1");
    expect(tree.sessions.map((one) => one.id)).toEqual(["s1", "s3", "s2"]);
    expect(tree.children.map((one) => one.id)).toEqual(["s3", "s2"]);
    expect(tree.parent).toBeUndefined();
  });

  it("knows the parent of a child, and that a child has no children", () => {
    const tree = sessionTreeOf([root, childA, childB], childA);
    expect(tree.parent?.id).toBe("s1");
    expect(tree.children).toEqual([]);
    expect(tree.sessions).toHaveLength(3);
  });

  it("includes the session on screen even before the list names it", () => {
    const tree = sessionTreeOf([root], childA);
    expect(tree.sessions.map((one) => one.id)).toEqual(["s1", "s2"]);
    expect(tree.parent?.id).toBe("s1");
  });

  it("orders sessions made in the same millisecond by id", () => {
    const twinB = makeSession("s5", 300, { parent_session_id: "s1", root_session_id: "s1" });
    const twinA = makeSession("s4", 300, { parent_session_id: "s1", root_session_id: "s1" });
    const tree = sessionTreeOf([twinB, childA, root, twinA], root);
    expect(tree.sessions.map((one) => one.id)).toEqual(["s1", "s2", "s4", "s5"]);
  });

  it("is empty until the session is known", () => {
    expect(sessionTreeOf([root], undefined).sessions).toEqual([]);
  });
});

describe("treeRevision", () => {
  it("moves when any session of the tree records an event or changes status, and only then", () => {
    const before = treeRevision(sessionTreeOf([root, childA], root));
    expect(treeRevision(sessionTreeOf([root, childA, other], root))).toBe(before);
    const moved = { ...childA, head_seq: 4 };
    expect(treeRevision(sessionTreeOf([root, moved], root))).not.toBe(before);
    const done = { ...childA, status: "review" as const };
    expect(treeRevision(sessionTreeOf([root, done], root))).not.toBe(before);
    expect(treeRevision(sessionTreeOf([root, childA, childB], root))).not.toBe(before);
  });
});

describe("isSettled", () => {
  it("follows the contract's list", () => {
    expect((["review", "shipping", "done", "failed", "cancelled"] as const).every(isSettled)).toBe(
      true,
    );
    expect((["queued", "preparing", "running", "waiting"] as const).some(isSettled)).toBe(false);
  });
});
