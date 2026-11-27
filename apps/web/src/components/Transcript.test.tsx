import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { Item } from "../gen/Item";
import type { PermissionDecision } from "../gen/PermissionDecision";
import type { FileTarget } from "../panels/registry";
import { createPermissionAnswers } from "../state/permissionAnswers";
import type { PermissionOutcome, ToolPermission, TranscriptEntry } from "../state/sessionReducer";
import { conflict, deferred, makeSessionRepo } from "../test/fakes";
import { click, find, findByText, mount, unmountAll } from "../test/render";
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
    // The cursor of a message being written is the stylesheet's, on this class.
    expect(message.querySelector(".markdown-streaming")).not.toBeNull();

    setEntries(entries().map(interrupted));
    flush();
    expect(find(card, "summary").textContent).toContain("Interrupted");
    expect(find(card, "summary").textContent).not.toContain("Running");
    // Nothing keeps pulsing as if it were still going.
    expect(host.querySelector(".pulse")).toBeNull();
    expect(host.querySelector(".markdown-streaming")).toBeNull();
    expect(message.textContent).toContain("(interrupted)");
  });
});

describe("Transcript: a permission request inside its tool call's card", () => {
  const asked = (outcome: PermissionOutcome, id = "p1"): ToolPermission => ({
    request: {
      id,
      tool_name: "Bash",
      tool_kind: "execute",
      title: "cargo test",
      input: { command: "cargo test" },
      item_id: "t1",
    },
    outcome,
    ts: 0,
  });
  const call = (
    status: Item["status"],
    permissions: ToolPermission[] | undefined,
  ): TranscriptEntry => {
    const entry = tool(null, status);
    return entry.kind === "item" && permissions ? { ...entry, permissions } : entry;
  };

  function setup(
    permissions: ToolPermission[],
    options: {
      synced?: boolean;
      resolve?: (id: string, decision: PermissionDecision, message?: string) => Promise<void>;
    } = {},
  ) {
    const resolve = vi.fn(options.resolve ?? (() => Promise.resolve()));
    const [entries, setEntries] = createSignal<TranscriptEntry[]>([call("running", permissions)]);
    // Made once, as the session screen does: a prop is read again every time it is used.
    const answers = createPermissionAnswers(resolve);
    const host = mount(() => (
      <Transcript entries={entries()} synced={options.synced ?? true} answers={answers} />
    ));
    const card = () => find(host, "ol > li > div");
    return { host, card, resolve, setEntries };
  }

  it("asks in the card while it waits, with both answers, and without opening the card", async () => {
    const { host, card, resolve } = setup([asked({ state: "pending" })]);
    // One row: the request has no card of its own.
    expect(host.querySelectorAll("ol > li")).toHaveLength(1);
    expect(find<HTMLDetailsElement>(card(), "details").open).toBe(false);
    const question = find(card(), '[data-permission="pending"]');
    expect(question.textContent).toContain("Allow this?");
    expect(question.getAttribute("aria-label")).toBe("Permission for cargo test");
    // Not inside the fold: it shows on the closed card.
    expect(question.closest("details")).toBeNull();

    const [deny, allow] = question.querySelectorAll("button");
    expect([deny.textContent, allow.textContent]).toEqual(["Deny", "Allow"]);
    // Named after the call, so they are not mistaken for the bar's own two.
    expect([deny.getAttribute("aria-label"), allow.getAttribute("aria-label")]).toEqual([
      "Deny cargo test",
      "Allow cargo test",
    ]);

    await click(allow);
    expect(resolve).toHaveBeenCalledWith("p1", "allow", undefined);
    // Sent: both buttons hold until the resolution arrives.
    expect([deny.disabled, allow.disabled]).toEqual([true, true]);
    expect(question.textContent).toContain("Sent, waiting for the agent…");
    await click(allow);
    expect(resolve).toHaveBeenCalledTimes(1);
  });

  it("denies from the card, and shows the server's reason when an answer is refused", async () => {
    const { card, resolve } = setup([asked({ state: "pending" })], {
      resolve: () => Promise.reject(conflict("the request was already answered")),
    });
    const question = find(card(), '[data-permission="pending"]');
    await click(findByText(question, "button", "Deny"));
    expect(resolve).toHaveBeenCalledWith("p1", "deny", undefined);
    expect(find(question, '[role="alert"]').textContent).toBe("the request was already answered");
    expect(findByText<HTMLButtonElement>(question, "button", "Deny").disabled).toBe(false);
  });

  it("says Sending while the answer is on its way", async () => {
    const pending = deferred();
    const { card } = setup([asked({ state: "pending" })], { resolve: () => pending.promise });
    const question = find(card(), '[data-permission="pending"]');
    await click(findByText(question, "button", "Allow"));
    expect(question.textContent).toContain("Sending…");
    pending.resolve();
    await click(question);
    expect(question.textContent).toContain("Sent, waiting for the agent…");
  });

  it("only shows the question, without answers, while the transcript replays or nothing can answer", () => {
    const replaying = setup([asked({ state: "pending" })], { synced: false });
    expect(replaying.card().textContent).toContain("Allow this?");
    expect(replaying.card().querySelector("button")).toBeNull();
    unmountAll();

    const host = mount(() => (
      <Transcript entries={[call("running", [asked({ state: "pending" })])]} synced={true} />
    ));
    expect(host.textContent).toContain("Allow this?");
    expect(host.querySelector("ol button")).toBeNull();
  });

  it.each<[string, PermissionOutcome, string]>([
    [
      "allowed by the reader",
      { state: "resolved", decision: "allow", by: "user", message: null },
      "Allowed by you",
    ],
    [
      "denied by the reader with a reason",
      { state: "resolved", decision: "deny", by: "user", message: "Not on main." },
      "Denied by you: Not on main.",
    ],
    [
      "denied by the reader without one",
      { state: "resolved", decision: "deny", by: "user", message: null },
      "Denied by you",
    ],
    [
      "allowed by policy",
      { state: "resolved", decision: "allow", by: "policy", message: null },
      "Allowed by policy",
    ],
    [
      "denied by policy",
      { state: "resolved", decision: "deny", by: "policy", message: "edits only" },
      "Denied by policy: edits only",
    ],
    ["lapsed", { state: "lapsed", message: null }, "No longer needed"],
    [
      "lapsed with the server's note",
      { state: "lapsed", message: "The turn ended." },
      "No longer needed: The turn ended.",
    ],
  ])("is one line once it is over: %s", (_name, outcome, line) => {
    const { host, card } = setup([asked(outcome)]);
    expect(host.querySelectorAll("ol > li")).toHaveLength(1);
    const outcomeLine = find(card(), "[data-permission]");
    expect(outcomeLine.tagName).toBe("P");
    expect(outcomeLine.textContent).toBe(line);
    // On the closed card, with nothing left to press.
    expect(outcomeLine.closest("details")).toBeNull();
    expect(card().textContent).not.toContain("Allow this?");
    expect(card().querySelector("button")).toBeNull();
  });

  it("turns the question into the outcome in place when the answer arrives", () => {
    const { card, setEntries } = setup([asked({ state: "pending" })]);
    const before = card();
    setEntries([
      call("declined", [
        asked({ state: "resolved", decision: "deny", by: "user", message: "Use a branch." }),
      ]),
    ]);
    flush();
    expect(card()).toBe(before);
    expect(find(card(), "[data-permission]").textContent).toBe("Denied by you: Use a branch.");
    expect(find(card(), "summary").textContent).toContain("Declined");
    expect(card().querySelector("button")).toBeNull();
  });

  it("shows each request of a call that asked more than once", () => {
    const { card } = setup([
      asked({ state: "resolved", decision: "deny", by: "user", message: "Try again." }, "p1"),
      asked({ state: "pending" }, "p2"),
    ]);
    expect(
      [...card().querySelectorAll("[data-permission]")].map((one) =>
        one.getAttribute("data-permission"),
      ),
    ).toEqual(["resolved", "pending"]);
  });

  it("keeps a card of its own for a request that names no tool call", () => {
    const answers = createPermissionAnswers(() => Promise.resolve());
    const host = mount(() => (
      <Transcript
        entries={[call("running", undefined), permission("p9", { state: "pending" })]}
        synced={true}
        answers={answers}
      />
    ));
    const [card, own] = host.querySelectorAll("ol > li");
    expect(card.querySelector("[data-permission]")).toBeNull();
    expect(own.textContent).toContain("Permission");
    expect(own.textContent).toContain("git push p9");
    expect(own.textContent).toContain("Waiting for you");
  });
});

describe("Transcript: the files of a tool call", () => {
  const repos = [
    makeSessionRepo("api", { path: "/sessions/s1/api", primary: true }),
    makeSessionRepo("web", { path: "/sessions/s1/web" }),
  ];
  const edit = (locations: string[]): TranscriptEntry =>
    itemEntry({
      id: "t1",
      status: "completed",
      content: {
        kind: "tool",
        name: "Edit",
        tool_kind: "edit",
        title: "src/main.rs",
        input: { file_path: locations[0] ?? "" },
        output: null,
        locations,
      },
    });

  it("lists them relative to the repo they are in, each opening in Files", async () => {
    const opened: FileTarget[] = [];
    const host = mount(() => (
      <Transcript
        entries={[
          edit([
            "/sessions/s1/api/src/main.rs",
            "/sessions/s1/web/src/app.ts",
            "/sessions/s1/api/src/main.rs",
            "/etc/hosts",
            "/sessions/s1/web",
          ]),
        ]}
        synced={true}
        repos={repos}
        onOpenFile={(target) => opened.push(target)}
      />
    ));
    const card = find<HTMLDetailsElement>(host, "details");
    // Nothing of it is built until the card is opened.
    expect(card.querySelector('ul[aria-label="Files of this tool call"]')).toBeNull();
    open(card);

    const rows = [...find(card, 'ul[aria-label="Files of this tool call"]').children];
    // Named once each; the repo name leads the path.
    expect(rows.map((row) => row.textContent)).toEqual([
      "api/src/main.rs",
      "web/src/app.ts",
      "/etc/hosts",
      "web/",
    ]);
    const links = rows.map((row) => row.querySelector("button"));
    // A file outside every repo, and a repo's own folder, open nothing.
    expect(links.map((link) => link !== null)).toEqual([true, true, false, false]);
    expect(links[0]?.title).toBe("Open api/src/main.rs in Files");

    if (links[1]) await click(links[1]);
    if (links[0]) await click(links[0]);
    expect(opened).toEqual([
      { sessionRepoId: "web", path: "src/app.ts" },
      { sessionRepoId: "api", path: "src/main.rs" },
    ]);
  });

  it("shows them as plain paths where there is no Files panel to open them in", () => {
    const host = mount(() => (
      <Transcript entries={[edit(["/sessions/s1/api/src/main.rs"])]} synced={true} repos={repos} />
    ));
    const card = find<HTMLDetailsElement>(host, "details");
    open(card);
    const list = find(card, 'ul[aria-label="Files of this tool call"]');
    expect(list.textContent).toBe("api/src/main.rs");
    expect(list.querySelector("button")).toBeNull();
  });

  it("has no Files section for a call that names none, and follows a repo attached later", () => {
    const [known, setKnown] = createSignal(repos.slice(0, 1));
    const host = mount(() => (
      <Transcript
        entries={[tool(null, "completed"), edit(["/sessions/s1/web/index.ts"])]}
        synced={true}
        repos={known()}
        onOpenFile={() => {}}
      />
    ));
    const [plain, withFiles] = host.querySelectorAll<HTMLDetailsElement>("details");
    open(plain);
    open(withFiles);
    expect(plain.querySelector('ul[aria-label="Files of this tool call"]')).toBeNull();
    // Not in a repo of the session yet: the path as the tool gave it.
    expect(find(withFiles, "ul").textContent).toBe("/sessions/s1/web/index.ts");

    setKnown(repos);
    flush();
    expect(find(withFiles, "ul").textContent).toBe("web/index.ts");
    expect(withFiles.querySelector("ul button")).not.toBeNull();
  });
});

describe("Transcript: reasoning, Markdown and attached repos", () => {
  const reasoning = (id: string, value: string, status: Item["status"]): TranscriptEntry =>
    itemEntry({ id, status, content: { kind: "reasoning", text: value } });

  it("has no row for a reasoning item with nothing in it, whatever its status", () => {
    const [entries, setEntries] = createSignal<TranscriptEntry[]>([
      reasoning("r1", "", "completed"),
      reasoning("r2", "  \n ", "running"),
      reasoning("r3", "", "failed"),
    ]);
    const host = mount(() => <Transcript entries={entries()} synced={true} />);
    expect(host.querySelectorAll("ol > li")).toHaveLength(0);
    expect(host.textContent).not.toContain("Reasoning");
    // With nothing else to show, the transcript says it is empty.
    expect(host.textContent).toContain("Nothing has happened in this session yet.");

    // It appears with its first words, and stays where it belongs among the rows.
    setEntries([
      reasoning("r1", "", "completed"),
      user,
      reasoning("r2", "Let me look.", "running"),
    ]);
    flush();
    const rows = host.querySelectorAll("ol > li");
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain("Run the tests");
    expect(rows[1].textContent).toContain("Reasoning…");
    expect(rows[1].textContent).toContain("Let me look.");
  });

  it("reads an agent's message as Markdown, and nothing else", () => {
    const host = mount(() => (
      <Transcript
        entries={[
          { kind: "user", key: "event:e1", ts: 0, text: "Use **stars** and `ticks`" },
          reasoning("r1", "I think *hard*.", "completed"),
          text("m1", "Done: **two** files, see `main.rs`.\n\n- [log](https://example.com/log)"),
          failure,
        ]}
        synced={true}
      />
    ));
    const [mine, thought, message] = host.querySelectorAll("ol > li");
    expect(find(message, "strong").textContent).toBe("two");
    expect(find(message, "code").textContent).toBe("main.rs");
    expect(find(message, "ul a").getAttribute("href")).toBe("https://example.com/log");
    // The reader's own words and the agent's reasoning are shown as written.
    expect(mine.textContent).toContain("Use **stars** and `ticks`");
    expect(mine.querySelector("strong, code")).toBeNull();
    expect(thought.textContent).toContain("I think *hard*.");
    expect(thought.querySelector("em")).toBeNull();
  });

  it("never turns an agent's message into markup of its own", () => {
    const host = mount(() => (
      <Transcript
        entries={[text("m1", "<script>window.pwned = 1</script>\n\n[x](javascript:alert(1))")]}
        synced={true}
      />
    ));
    expect(host.querySelector("script")).toBeNull();
    expect(host.querySelector("ol a")).toBeNull();
    expect(host.textContent).toContain("<script>window.pwned = 1</script>");
    expect("pwned" in window).toBe(false);
  });

  it("says who attached a repo while the session was under way, and how", () => {
    const row = (
      name: string,
      by: "user" | "agent",
      mode: "write" | "read" | "live",
    ): TranscriptEntry => ({
      kind: "repo",
      key: `event:${name}`,
      ts: 0,
      repo: makeSessionRepo(name, { mode }),
      by,
      agentId: by === "agent" ? "a1" : null,
    });
    const host = mount(() => (
      <Transcript
        entries={[
          row("web", "user", "write"),
          row("docs", "agent", "read"),
          row("ops", "user", "live"),
        ]}
        synced={true}
      />
    ));
    const rows = [...host.querySelectorAll("ol > li")].map((one) => one.textContent);
    expect(rows).toEqual([
      "Repo web attached by you · Worktree",
      "Repo docs attached by the agent · Read only",
      "Repo ops attached by you · Edit in place",
    ]);
  });
});
