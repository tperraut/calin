import { describe, expect, it } from "vite-plus/test";
import { makeSession } from "../test/fakes";
import { makeAgent, makeMessage, makeSubagent } from "../test/orchestration";
import { isLongBody, messageHeading, speakerFor } from "./messages";

const parent = makeSession("s1", 1000, { title: "Ship the health check" });
const child = makeSession("s2", 2000, {
  title: "API: add /health",
  parent_session_id: "s1",
  root_session_id: "s1",
});
const agents = [makeAgent("a1"), makeSubagent("sub1", "explorer")];

describe("messageHeading", () => {
  const inParent = speakerFor(parent, agents, [parent, child]);
  const inChild = speakerFor(child, [makeAgent("a2", { session_id: "s2" })], [parent, child]);

  it("words each kind from the side of the session that sent it", () => {
    const heading = (kind: "dispatch" | "note" | "report") =>
      messageHeading(makeMessage("m", { kind }), "sent", inParent);
    expect(heading("dispatch")).toBe("Dispatched child session API: add /health");
    expect(heading("note")).toBe("Note to API: add /health");
    expect(heading("report")).toBe("Report to API: add /health");
  });

  it("words each kind from the side of the session that received it", () => {
    const fromChild = (kind: "note" | "report") =>
      messageHeading(
        makeMessage("m", {
          kind,
          from_session_id: "s2",
          from_agent_id: "a2",
          to_session_id: "s1",
          to_agent_id: "a1",
        }),
        "received",
        inParent,
      );
    expect(fromChild("report")).toBe("Report from API: add /health");
    expect(fromChild("note")).toBe("Note from API: add /health");
    expect(messageHeading(makeMessage("m", { kind: "dispatch" }), "received", inChild)).toBe(
      "Brief from Ship the health check",
    );
  });

  it("calls the user you, and an agent of this session by its role", () => {
    const toUser = makeMessage("m", { to_session_id: "s1", to_agent_id: null });
    expect(messageHeading(toUser, "sent", inParent)).toBe("Note to you");
    const fromSubagent = makeMessage("m", {
      from_agent_id: "sub1",
      to_session_id: "s1",
      to_agent_id: "a1",
    });
    expect(messageHeading(fromSubagent, "received", inParent)).toBe("Note from explorer");
  });

  it("falls back to the session's place in the tree when its title is not known", () => {
    const alone = speakerFor(child, [], []);
    expect(messageHeading(makeMessage("m", { kind: "dispatch" }), "received", alone)).toBe(
      "Brief from the parent session",
    );
    const lonelyParent = speakerFor(parent, agents, []);
    expect(messageHeading(makeMessage("m", { kind: "dispatch" }), "sent", lonelyParent)).toBe(
      "Dispatched child session a child session",
    );
  });
});

describe("isLongBody", () => {
  it("is true past ten lines or six hundred characters", () => {
    expect(isLongBody("short")).toBe(false);
    expect(isLongBody("x".repeat(601))).toBe(true);
    expect(isLongBody(Array.from({ length: 11 }, () => "line").join("\n"))).toBe(true);
  });
});
