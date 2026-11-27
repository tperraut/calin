import { describe, expect, it } from "vite-plus/test";
import type { Item } from "../gen/Item";
import type { TranscriptEntry } from "../state/sessionReducer";
import { makeAgent, makeSubagent } from "../test/orchestration";
import { transcriptRows } from "./transcriptRows";

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
