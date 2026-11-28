import { describe, expect, it } from "vite-plus/test";
import type { Session } from "../gen/Session";
import { makeSession as session } from "../test/fakes";
import {
  childrenShownByDefault,
  countSessions,
  firstNeedingYou,
  needsYou,
  needsYouFirst,
  nestSessions,
  upsertSession,
  type SessionNode,
} from "./sessionsList";

const ids = (list: readonly Session[]): string[] => list.map((one) => one.id);

describe("upsertSession", () => {
  it("inserts sessions newest first whatever the arrival order", () => {
    let list: Session[] = [];
    for (const one of [
      session("b", 200),
      session("d", 400),
      session("a", 100),
      session("c", 300),
    ]) {
      list = upsertSession(list, one);
    }
    expect(ids(list)).toEqual(["d", "c", "b", "a"]);
  });

  it("updates a session in place without duplicating it", () => {
    let list = [session("a", 100), session("b", 200)].reduce<Session[]>(upsertSession, []);

    list = upsertSession(
      list,
      session("a", 100, { status: "running", title: "Renamed", head_seq: 12 }),
    );

    expect(ids(list)).toEqual(["b", "a"]);
    expect(list[1]).toMatchObject({ status: "running", title: "Renamed", head_seq: 12 });
  });

  it("orders by creation, not by last update", () => {
    let list = [session("old", 100), session("new", 200)].reduce<Session[]>(upsertSession, []);

    list = upsertSession(list, session("old", 100, { updated_at: 999, status: "done" }));

    expect(ids(list)).toEqual(["new", "old"]);
  });

  it("keeps a stable order for sessions created in the same millisecond", () => {
    const forward = [session("a", 100), session("b", 100)].reduce<Session[]>(upsertSession, []);
    const backward = [session("b", 100), session("a", 100)].reduce<Session[]>(upsertSession, []);
    expect(ids(forward)).toEqual(ids(backward));
    expect(ids(forward)).toEqual(["b", "a"]);
  });

  it("does not mutate the list it is given", () => {
    const list = Object.freeze([session("a", 100)]);
    const next = upsertSession(list, session("b", 200));
    expect(ids(list)).toEqual(["a"]);
    expect(ids(next)).toEqual(["b", "a"]);
  });
});

describe("nestSessions", () => {
  const child = (id: string, at: number, parent: string, patch: Partial<Session> = {}) =>
    session(id, at, { parent_session_id: parent, root_session_id: parent, ...patch });
  const shape = (nodes: readonly SessionNode[]): unknown[] =>
    nodes.map((node) =>
      node.children.length === 0 ? node.session.id : [node.session.id, shape(node.children)],
    );
  const sorted = (...list: Session[]) => list.reduce<Session[]>(upsertSession, []);

  it("puts children under their parent in the order they were dispatched, parents newest first", () => {
    const list = sorted(
      session("old", 100),
      session("new", 500),
      child("c2", 300, "old"),
      child("c1", 200, "old"),
      child("n1", 600, "new"),
    );
    expect(shape(nestSessions(list))).toEqual([
      ["new", ["n1"]],
      ["old", ["c1", "c2"]],
    ]);
  });

  it("shows a child whose parent is not in the list at the top level", () => {
    const list = sorted(session("a", 100), child("orphan", 200, "gone"));
    expect(shape(nestSessions(list))).toEqual(["orphan", "a"]);
  });

  it("nests a child's own children under it", () => {
    const list = sorted(session("a", 100), child("b", 200, "a"), child("c", 300, "b"));
    expect(shape(nestSessions(list))).toEqual([["a", [["b", ["c"]]]]]);
  });

  it("folds a parent's children away once all are settled, unless one of them is open", () => {
    const tree = (status: Session["status"]) =>
      nestSessions(
        sorted(
          session("a", 100),
          child("b", 200, "a", { status: "done" }),
          child("c", 300, "a", { status }),
        ),
      )[0];
    expect(childrenShownByDefault(tree("running"), undefined)).toBe(true);
    expect(childrenShownByDefault(tree("waiting"), undefined)).toBe(true);
    expect(childrenShownByDefault(tree("review"), undefined)).toBe(false);
    expect(childrenShownByDefault(tree("cancelled"), undefined)).toBe(false);
    expect(childrenShownByDefault(tree("review"), "c")).toBe(true);
    expect(childrenShownByDefault(tree("review"), "a")).toBe(false);
  });
});

describe("what needs the user", () => {
  const child = (id: string, at: number, parent: string, patch: Partial<Session> = {}) =>
    session(id, at, { parent_session_id: parent, root_session_id: parent, ...patch });

  it("is a session waiting on an answer or a review, and nothing else", () => {
    expect(needsYou("waiting")).toBe(true);
    expect(needsYou("review")).toBe(true);
    for (const status of ["queued", "preparing", "running", "done", "failed"] as const) {
      expect(needsYou(status)).toBe(false);
    }
  });

  it("puts every tree holding such a session first, each group in its order", () => {
    const nodes = nestSessions(
      [
        session("new", 500),
        session("mid", 400, { status: "review" }),
        session("old", 100),
        child("deep", 300, "old", { status: "waiting" }),
      ].reduce<Session[]>(upsertSession, []),
    );
    expect(needsYouFirst(nodes).map((node) => node.session.id)).toEqual(["mid", "old", "new"]);
    expect(firstNeedingYou(nodes)?.id).toBe("mid");
    expect(firstNeedingYou(nodes.filter((node) => node.session.id !== "mid"))?.id).toBe("deep");
    expect(firstNeedingYou(nestSessions([session("calm", 100)]))).toBeUndefined();
  });

  it("counts the sessions that need the user, run and wait for a slot", () => {
    const statuses: Session["status"][] = [
      "waiting",
      "review",
      "running",
      "preparing",
      "queued",
      "done",
      "failed",
    ];
    const list = statuses.map((status, index) => session(`s${index}`, index, { status }));
    expect(countSessions(list)).toEqual({ needYou: 2, running: 2, queued: 1 });
    expect(countSessions([])).toEqual({ needYou: 0, running: 0, queued: 0 });
  });
});

describe("a session that names itself as its parent", () => {
  it("stays at the top level instead of vanishing", () => {
    const self = session("loop", 100, { parent_session_id: "loop" });
    expect(nestSessions([self]).map((node) => node.session.id)).toEqual(["loop"]);
  });
});
