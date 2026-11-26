import { describe, expect, it } from "vite-plus/test";
import type { Session } from "../gen/Session";
import { makeSession as session } from "../test/fakes";
import { upsertSession } from "./sessionsList";

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
  });

  it("does not mutate the list it is given", () => {
    const list = Object.freeze([session("a", 100)]);
    const next = upsertSession(list, session("b", 200));
    expect(ids(list)).toEqual(["a"]);
    expect(ids(next)).toEqual(["b", "a"]);
  });
});
