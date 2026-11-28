import { describe, expect, it } from "vite-plus/test";
import type { Item } from "../gen/Item";
import type { TranscriptEntry } from "../state/sessionReducer";
import { makePullRequest } from "../test/fakes";
import { makeAgent, makeSubagent } from "../test/orchestration";
import { entryAgent, transcriptRows } from "./transcriptRows";

const item = (
  id: string,
  agentId: string,
  kind: "text" | "task" | "tool" = "text",
): TranscriptEntry => {
  const content: Item["content"] =
    kind === "text"
      ? { kind: "text", text: id }
      : {
          kind: "tool",
          name: "T",
          tool_kind: kind === "task" ? "task" : "read",
          title: id,
          input: {},
          output: null,
          locations: [],
        };
  return {
    kind: "item",
    key: `item:${id}`,
    ts: 0,
    agentId,
    item: { id, status: "completed", content },
    interrupted: false,
  };
};
const user: TranscriptEntry = { kind: "user", key: "event:u", ts: 0, text: "go" };
const agents = [
  makeAgent("a1"),
  makeSubagent("sub1", "explorer"),
  makeSubagent("sub2", "reviewer"),
];

const shape = (rows: ReturnType<typeof transcriptRows>): string[] =>
  rows.map((row) =>
    row.kind === "entry"
      ? row.key
      : `${row.key}[${row.entries.map((entry) => entry.key).join(",")}]#${row.toolCalls}`,
  );

describe("transcriptRows", () => {
  const entries = [
    user,
    item("m1", "a1"),
    item("t1", "a1", "task"),
    item("s1a", "sub1", "tool"),
    item("t2", "a1", "task"),
    item("s2a", "sub2", "tool"),
    item("s1b", "sub1", "tool"),
    item("s1c", "sub1"),
    item("m2", "a1"),
  ];

  it("gathers each subagent's rows under the tool call that started it", () => {
    expect(shape(transcriptRows(entries, agents, { sub1: "t1", sub2: "t2" }, null))).toEqual([
      "event:u",
      "item:m1",
      "item:t1",
      "subagent:sub1[item:s1a,item:s1b,item:s1c]#2",
      "item:t2",
      "subagent:sub2[item:s2a]#1",
      "item:m2",
    ]);
  });

  it("places a subagent whose call is not known where it first did something", () => {
    expect(shape(transcriptRows(entries, agents, { sub1: "t1" }, null))).toEqual([
      "event:u",
      "item:m1",
      "item:t1",
      "subagent:sub1[item:s1a,item:s1b,item:s1c]#2",
      "item:t2",
      "subagent:sub2[item:s2a]#1",
      "item:m2",
    ]);
    expect(shape(transcriptRows(entries, agents, {}, null))[3]).toBe(
      "subagent:sub1[item:s1a,item:s1b,item:s1c]#2",
    );
  });

  it("shows a subagent that has done nothing yet under its call", () => {
    const rows = transcriptRows([item("t1", "a1", "task")], agents, { sub1: "t1" }, null);
    expect(shape(rows)).toEqual(["item:t1", "subagent:sub1[]#0"]);
  });

  it("puts two subagents started by one call under it, in the order they were announced", () => {
    const rows = transcriptRows(entries, agents, { sub1: "t2", sub2: "t2" }, null);
    expect(shape(rows)).toEqual([
      "event:u",
      "item:m1",
      "item:t1",
      "item:t2",
      "subagent:sub1[item:s1a,item:s1b,item:s1c]#2",
      "subagent:sub2[item:s2a]#1",
      "item:m2",
    ]);
  });

  it("takes only a top-level call as a subagent's start, and leaves out one with no call and no rows", () => {
    // sub2's call is one of sub1's own items: it is placed where it first did something.
    const rows = transcriptRows(entries, agents, { sub1: "t1", sub2: "s1a" }, null);
    expect(shape(rows)).toEqual([
      "event:u",
      "item:m1",
      "item:t1",
      "subagent:sub1[item:s1a,item:s1b,item:s1c]#2",
      "item:t2",
      "subagent:sub2[item:s2a]#1",
      "item:m2",
    ]);
    const quiet = transcriptRows([user, item("m1", "a1")], agents, {}, null);
    expect(shape(quiet)).toEqual(["event:u", "item:m1"]);
  });

  it("gathers the rows of a session's only subagent too", () => {
    const one = [makeAgent("a1"), makeSubagent("sub1", "explorer")];
    const rows = transcriptRows(entries.slice(0, 4), one, { sub1: "t1" }, null);
    expect(shape(rows)).toEqual(["event:u", "item:m1", "item:t1", "subagent:sub1[item:s1a]#1"]);
  });

  it("says the user's rows and the pull request rows are nobody's", () => {
    const opened: TranscriptEntry = {
      kind: "pull_request",
      key: "pull_request:r1:7:opened",
      ts: 0,
      what: "opened",
      pullRequest: makePullRequest("r1", 7),
    };
    expect(entryAgent(user)).toBeNull();
    expect(entryAgent(opened)).toBeNull();
    expect(entryAgent(item("m1", "a1"))).toBe("a1");
  });

  it("leaves the transcript flat when the session has no subagent", () => {
    const rows = transcriptRows(entries, [makeAgent("a1")], {}, null);
    expect(rows.every((row) => row.kind === "entry")).toBe(true);
    expect(rows).toHaveLength(entries.length);
  });

  it("shows only one agent's rows, flat, when filtered", () => {
    expect(shape(transcriptRows(entries, agents, { sub1: "t1" }, "sub1"))).toEqual([
      "item:s1a",
      "item:s1b",
      "item:s1c",
    ]);
    // The lead's own rows: the user's are nobody's.
    expect(shape(transcriptRows(entries, agents, { sub1: "t1" }, "a1"))).toEqual([
      "item:m1",
      "item:t1",
      "item:t2",
      "item:m2",
    ]);
  });
});
