import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vite-plus/test";
import type { Item } from "../gen/Item";
import type { PermissionOutcome, TranscriptEntry } from "../state/sessionReducer";
import { find, findByText, mount, unmountAll } from "../test/render";
import { Transcript } from "./Transcript";

afterEach(unmountAll);

const itemEntry = (item: Item): TranscriptEntry => ({
  kind: "item",
  key: `item:${item.id}`,
  ts: 0,
  agentId: "a1",
  item,
  interrupted: false,
});

const text = (id: string, value: string, status: Item["status"] = "completed"): TranscriptEntry =>
  itemEntry({ id, status, content: { kind: "text", text: value } });

const tool = (output: string | null, status: Item["status"]): TranscriptEntry =>
  itemEntry({
    id: "t1",
    status,
    content: {
      kind: "tool",
      name: "Bash",
      tool_kind: "execute",
      title: "cargo test",
      input: { command: "cargo test", timeout: 60 },
      output,
      locations: [],
    },
  });

const permission = (id: string, outcome: PermissionOutcome): TranscriptEntry => ({
  kind: "permission",
  key: `permission:${id}`,
  ts: 0,
  agentId: "a1",
  request: {
    id,
    tool_name: "Bash",
    tool_kind: "execute",
    title: `git push ${id}`,
    input: { command: `git push ${id}` },
    item_id: null,
  },
  outcome,
});

const interrupted = (entry: TranscriptEntry): TranscriptEntry =>
  entry.kind === "item" ? { ...entry, interrupted: true } : entry;

const user: TranscriptEntry = { kind: "user", key: "event:e1", ts: 0, text: "Run the tests" };
const failure: TranscriptEntry = {
  kind: "error",
  key: "event:e9",
  ts: 0,
  agentId: null,
  message: "The runner exited with code 1",
};

function open(details: HTMLDetailsElement): void {
  details.open = true;
  details.dispatchEvent(new Event("toggle"));
  flush();
}

/** happy-dom does no layout, so the test supplies the numbers a browser would. */
function layout(scroller: HTMLElement, metrics: { scrollHeight: number; clientHeight: number }) {
  Object.defineProperty(scroller, "scrollHeight", {
    configurable: true,
    value: metrics.scrollHeight,
  });
  Object.defineProperty(scroller, "clientHeight", {
    configurable: true,
    value: metrics.clientHeight,
  });
}

function scrollTo(scroller: HTMLElement, top: number): void {
  scroller.scrollTop = top;
  scroller.dispatchEvent(new Event("scroll"));
  flush();
}

describe("Transcript", () => {
  it("renders every kind of entry", () => {
    const reasoning = itemEntry({
      id: "r1",
      status: "completed",
      content: { kind: "reasoning", text: "Tests first." },
    });
    const host = mount(() => (
      <Transcript
        entries={[
          user,
          reasoning,
          tool("ok\n", "completed"),
          text("m1", "All green.\nDone."),
          failure,
        ]}
        synced={true}
      />
    ));

    const rows = host.querySelectorAll("ol > li");
    expect(rows).toHaveLength(5);
    expect(rows[0].textContent).toContain("You");
    expect(rows[0].textContent).toContain("Run the tests");
    expect(rows[3].textContent).toBe("All green.\nDone.");
    expect(rows[4].textContent).toContain("Error");
    expect(rows[4].textContent).toContain("The runner exited with code 1");

    // Reasoning is there but folded away.
    const thinking = find<HTMLDetailsElement>(rows[1], "details");
    expect(thinking.open).toBe(false);
    expect(find(thinking, "summary").textContent).toContain("Reasoning");
    expect(thinking.textContent).toContain("Tests first.");
  });

  it("shows a tool call as a collapsed card with its kind, title and status", () => {
    const host = mount(() => <Transcript entries={[tool(null, "running")]} synced={true} />);

    const card = find<HTMLDetailsElement>(host, "details");
    const summary = find(card, "summary").textContent;
    expect(card.open).toBe(false);
    expect(summary).toContain("execute");
    expect(summary).toContain("cargo test");
    expect(summary).toContain("Running");
    // Nothing of the body is built until the card is opened.
    expect(card.querySelector("pre")).toBeNull();
  });

  it("expands a tool card to pretty-printed input and its output", () => {
    const host = mount(() => (
      <Transcript entries={[tool("running 3 tests\nok\n", "completed")]} synced={true} />
    ));
    const card = find<HTMLDetailsElement>(host, "details");

    open(card);

    const [input, output] = card.querySelectorAll("pre");
    expect(input.textContent).toBe('{\n  "command": "cargo test",\n  "timeout": 60\n}');
    expect(output.textContent).toBe("running 3 tests\nok\n");
    expect(card.textContent).toContain("Bash");
    expect(find(card, "summary").textContent).toContain("Done");
  });

  it("labels failed and declined tool calls in words", () => {
    const [entries, setEntries] = createSignal<TranscriptEntry[]>([tool(null, "failed")]);
    const host = mount(() => <Transcript entries={entries()} synced={true} />);
    expect(find(host, "summary").textContent).toContain("Failed");

    setEntries([tool(null, "declined")]);
    flush();
    expect(find(host, "summary").textContent).toContain("Declined");
  });

  it("applies streamed updates to the existing rows without rebuilding them", () => {
    const [entries, setEntries] = createSignal<TranscriptEntry[]>([
      tool(null, "running"),
      text("m1", "Hel", "running"),
    ]);
    const host = mount(() => <Transcript entries={entries()} synced={true} />);
    const card = find<HTMLDetailsElement>(host, "details");
    const [, messageRow] = host.querySelectorAll("ol > li");
    open(card);
    expect(card.querySelectorAll("pre")).toHaveLength(1);

    // A delta arrives: new objects with the same keys, as the reducer produces.
    setEntries([tool("line 1\n", "running"), text("m1", "Hello", "running")]);
    flush();
    setEntries([
      tool("line 1\nline 2\n", "completed"),
      text("m1", "Hello there", "completed"),
      failure,
    ]);
    flush();

    expect(find(host, "details")).toBe(card);
    // The card the reader opened is still open, now with the output.
    expect(card.open).toBe(true);
    expect(card.querySelectorAll("pre")[1].textContent).toBe("line 1\nline 2\n");
    expect(host.querySelectorAll("ol > li")[1]).toBe(messageRow);
    expect(messageRow.textContent).toBe("Hello there");
    expect(host.querySelectorAll("ol > li")).toHaveLength(3);
  });

  it("is a log that does not announce every change", () => {
    const host = mount(() => <Transcript entries={[user]} synced={true} />);
    const log = find(host, '[role="log"]');
    expect(log.getAttribute("aria-live")).toBe("off");
    expect(log.getAttribute("aria-label")).toBe("Transcript");
    // Focusable, so the keyboard can scroll it.
    expect(log.getAttribute("tabindex")).toBe("0");
  });

  it("says whether an empty transcript is still loading", () => {
    const [synced, setSynced] = createSignal(false);
    const host = mount(() => <Transcript entries={[]} synced={synced()} />);
    expect(host.textContent).toContain("Loading transcript…");

    setSynced(true);
    flush();
    expect(host.textContent).toContain("Nothing has happened in this session yet.");
  });

  it("follows new content while the reader is at the bottom", () => {
    const [entries, setEntries] = createSignal<TranscriptEntry[]>([user]);
    const host = mount(() => <Transcript entries={entries()} synced={true} />);
    const scroller = find(host, '[role="log"]');

    layout(scroller, { scrollHeight: 900, clientHeight: 400 });
    setEntries([user, text("m1", "more")]);
    flush();

    expect(scroller.scrollTop).toBe(900);
    expect(host.querySelector("button")).toBeNull();
  });

  it("stays put once the reader scrolls up, and offers a way back", () => {
    const [entries, setEntries] = createSignal<TranscriptEntry[]>([user]);
    const host = mount(() => <Transcript entries={entries()} synced={true} />);
    const scroller = find(host, '[role="log"]');
    layout(scroller, { scrollHeight: 900, clientHeight: 400 });

    scrollTo(scroller, 120);
    const jump = findByText<HTMLButtonElement>(host, "button", "Jump to latest");

    layout(scroller, { scrollHeight: 1400, clientHeight: 400 });
    setEntries([user, text("m1", "more")]);
    flush();
    expect(scroller.scrollTop).toBe(120);

    jump.click();
    flush();
    expect(scroller.scrollTop).toBe(1400);
    expect(host.querySelector("button")).toBeNull();

    // Back at the bottom, it follows again.
    layout(scroller, { scrollHeight: 1800, clientHeight: 400 });
    setEntries([user, text("m1", "more"), text("m2", "and more")]);
    flush();
    expect(scroller.scrollTop).toBe(1800);
  });

  it("treats a position near the end as the bottom", () => {
    const host = mount(() => <Transcript entries={[user]} synced={true} />);
    const scroller = find(host, '[role="log"]');
    layout(scroller, { scrollHeight: 900, clientHeight: 400 });

    scrollTo(scroller, 470);
    expect(host.querySelector("button")).toBeNull();

    scrollTo(scroller, 440);
    expect(host.querySelector("button")).not.toBeNull();
  });

  it("shows a permission request where it was asked, with what became of it", () => {
    const host = mount(() => (
      <Transcript
        entries={[
          user,
          permission("p1", { state: "pending" }),
          permission("p2", { state: "resolved", decision: "allow", by: "user", message: null }),
          permission("p3", { state: "resolved", decision: "allow", by: "policy", message: null }),
          permission("p4", {
            state: "resolved",
            decision: "deny",
            by: "user",
            message: "Not on main.",
          }),
          permission("p5", { state: "resolved", decision: "deny", by: "policy", message: null }),
          permission("p6", { state: "lapsed", message: "The turn ended." }),
          text("m1", "Understood."),
        ]}
        synced={true}
      />
    ));
    const rows = [...host.querySelectorAll("ol > li")].map((row) => row.textContent);
    expect(rows).toHaveLength(8);
    expect(rows[1]).toContain("Permission");
    expect(rows[1]).toContain("execute");
    expect(rows[1]).toContain("git push p1");
    expect(rows[1]).toContain("Waiting for you");
    expect(rows[2]).toContain("Allowed by you");
    expect(rows[3]).toContain("Allowed by policy");
    expect(rows[4]).toContain("Denied by you");
    expect(rows[4]).toContain("Not on main.");
    expect(rows[5]).toContain("Denied by policy");
    // Closed by the end of the turn: not a refusal, and not by the reader.
    expect(rows[6]).toContain("No longer needed");
    expect(rows[6]).toContain("The turn ended.");
    expect(rows[6]).not.toContain("Denied");
    // The answer is given in the pinned bar; the row only records it.
    expect(host.querySelector("ol button")).toBeNull();
  });

  it("updates a permission row in place when the answer arrives", () => {
    const [entries, setEntries] = createSignal<TranscriptEntry[]>([
      permission("p1", { state: "pending" }),
    ]);
    const host = mount(() => <Transcript entries={entries()} synced={true} />);
    const row = find(host, "ol > li");

    setEntries([
      permission("p1", { state: "resolved", decision: "deny", by: "user", message: "No." }),
    ]);
    flush();
    expect(find(host, "ol > li")).toBe(row);
    expect(row.textContent).toContain("Denied by you");
    expect(row.textContent).not.toContain("Waiting for you");
  });

  it("shows an item its turn left behind as interrupted instead of running", () => {
    const [entries, setEntries] = createSignal<TranscriptEntry[]>([
      tool(null, "running"),
      text("m1", "Half a sen", "running"),
    ]);
    const host = mount(() => <Transcript entries={entries()} synced={true} />);
    const [card, message] = host.querySelectorAll("ol > li");
    expect(find(card, "summary").textContent).toContain("Running");
    expect(message.querySelector(".pulse")).not.toBeNull();

    setEntries(entries().map(interrupted));
    flush();
    expect(find(card, "summary").textContent).toContain("Interrupted");
    expect(find(card, "summary").textContent).not.toContain("Running");
    // Nothing keeps pulsing as if it were still going.
    expect(host.querySelector(".pulse")).toBeNull();
    expect(message.textContent).toContain("(interrupted)");
  });
});
