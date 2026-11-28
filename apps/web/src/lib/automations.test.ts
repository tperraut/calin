import { describe, expect, it } from "vite-plus/test";
import type { AutomationTrigger } from "../gen/AutomationTrigger";
import { badRequest, conflict, makeRepo } from "../test/fakes";
import { makeAutomation, makeRun } from "../test/integrations";
import {
  automationDraft,
  blameField,
  causeText,
  messageAt,
  outcomeText,
  placeholders,
  runNumber,
  runWanted,
  saveFailure,
  sessionOf,
  triggerOf,
  watchedRepo,
  type SessionDraft,
} from "./automations";

describe("runNumber", () => {
  it.each([
    ["12", 12],
    ["#12", 12],
    ["1", 1],
    [" 7 ", 7],
  ])("reads %j as %d", (typed, number) => {
    expect(runNumber(typed)).toBe(number);
  });

  it.each(["", "#", "0", "-3", "1.5", "twelve", "12a"])("refuses %j", (typed) => {
    expect(runNumber(typed)).toBeUndefined();
  });
});

describe("placeholders", () => {
  it("offers only the date to a schedule, and the item's fields to a trigger", () => {
    expect(placeholders("schedule")).toEqual(["{{date}}"]);
    expect(placeholders("issue")).toEqual([
      "{{date}}",
      "{{number}}",
      "{{title}}",
      "{{body}}",
      "{{url}}",
      "{{author}}",
    ]);
  });
});

describe("outcomeText", () => {
  it("words a start alone, even with a detail", () => {
    expect(outcomeText(makeRun("r", { detail: "ignored" }))).toBe("Started a session");
  });

  it("adds why a run was skipped or failed, when the server said", () => {
    expect(outcomeText(makeRun("r", { outcome: "skipped", detail: "still active" }))).toBe(
      "Skipped: still active",
    );
    expect(outcomeText(makeRun("r", { outcome: "failed", detail: "no repo" }))).toBe(
      "Failed: no repo",
    );
    expect(outcomeText(makeRun("r", { outcome: "failed", detail: null }))).toBe("Failed");
    expect(outcomeText(makeRun("r", { outcome: "skipped", detail: "" }))).toBe("Skipped");
  });
});

describe("causeText", () => {
  it("capitalizes the server's words, and leaves none as none", () => {
    expect(causeText("schedule")).toBe("Schedule");
    expect(causeText("issue #12")).toBe("Issue #12");
    expect(causeText("")).toBe("");
  });
});

describe("runWanted", () => {
  const issue = { kind: "issue", repo_id: "api", label: null } as const;
  const pull = { kind: "pull_request", repo_id: "api" } as const;

  it("asks no number of a schedule, whatever was typed", () => {
    expect(runWanted({ kind: "schedule", cron: "* * * * *" }, "nonsense")).toEqual({
      number: undefined,
    });
  });

  it("sends the number typed for a trigger", () => {
    expect(runWanted(issue, "#4")).toEqual({ number: 4 });
  });

  it("says what a trigger needs when the number is not one", () => {
    expect(runWanted(issue, "")).toEqual({
      problem: "Give the issue number, a whole number such as 12.",
    });
    expect(runWanted(pull, "x")).toEqual({
      problem: "Give the pull request number, a whole number such as 12.",
    });
  });
});

describe("automationDraft", () => {
  it("starts a new automation off, on weekday mornings, unattended", () => {
    expect(automationDraft(undefined)).toEqual({
      name: "",
      enabled: false,
      trigger: { kind: "schedule", cron: "0 8 * * 1-5", repo: "", label: "" },
      session: {
        prompt: "",
        choices: [],
        workflow: undefined,
        runner: "",
        model: "",
        mode: "accept_edits",
      },
    });
  });

  it("reads each kind of trigger as saved, with the schedule's default behind the others", () => {
    const draft = (trigger: AutomationTrigger) =>
      automationDraft(makeAutomation("a", { trigger })).trigger;
    expect(draft({ kind: "schedule", cron: "5 * * * *" })).toEqual({
      kind: "schedule",
      cron: "5 * * * *",
      repo: "",
      label: "",
    });
    expect(draft({ kind: "issue", repo_id: "api", label: "agent" })).toEqual({
      kind: "issue",
      cron: "0 8 * * 1-5",
      repo: "api",
      label: "agent",
    });
    expect(draft({ kind: "issue", repo_id: "api", label: null }).label).toBe("");
    expect(draft({ kind: "pull_request", repo_id: "web" })).toEqual({
      kind: "pull_request",
      cron: "0 8 * * 1-5",
      repo: "web",
      label: "",
    });
  });

  it("reads the session as saved: a repo without a mode is a worktree, Ask becomes Accept edits", () => {
    const stored = makeAutomation("a", {
      name: "Nightly",
      enabled: true,
      session: {
        prompt: "Do it",
        workflow: "review",
        repos: [{ repo_id: "api" }, { repo_id: "web", mode: "live" }],
        runner: "mock",
        model: "opus",
        permission_mode: "ask",
      },
    });
    expect(automationDraft(stored)).toMatchObject({
      name: "Nightly",
      enabled: true,
      session: {
        prompt: "Do it",
        choices: [
          { repoId: "api", mode: "write" },
          { repoId: "web", mode: "live" },
        ],
        workflow: "review",
        runner: "mock",
        model: "opus",
        mode: "accept_edits",
      },
    });
    const autonomous = makeAutomation("b", {
      session: { ...stored.session, permission_mode: "autonomous" },
    });
    expect(automationDraft(autonomous).session.mode).toBe("autonomous");
  });
});

describe("watchedRepo", () => {
  const repos = [makeRepo("gone", { available: false }), makeRepo("api"), makeRepo("web")];

  it("keeps the repo chosen", () => {
    expect(watchedRepo("web", repos)).toBe("web");
  });

  it("falls back to the first usable repo, then the first, then none", () => {
    expect(watchedRepo("", repos)).toBe("api");
    expect(watchedRepo("", [makeRepo("gone", { available: false })])).toBe("gone");
    expect(watchedRepo("", [])).toBe("");
  });
});

describe("triggerOf", () => {
  const repos = [makeRepo("api"), makeRepo("web")];
  const draft = { kind: "schedule", cron: " 0 9 * * 1 ", repo: "", label: " agent " } as const;

  it("sends a schedule's expression trimmed", () => {
    expect(triggerOf(draft, repos)).toEqual({ kind: "schedule", cron: "0 9 * * 1" });
  });

  it("sends an issue trigger's watched repo and trimmed label, or no label when blank", () => {
    expect(triggerOf({ ...draft, kind: "issue" }, repos)).toEqual({
      kind: "issue",
      repo_id: "api",
      label: "agent",
    });
    expect(triggerOf({ ...draft, kind: "issue", repo: "web", label: "  " }, repos)).toEqual({
      kind: "issue",
      repo_id: "web",
      label: null,
    });
  });

  it("sends a pull request trigger's watched repo only", () => {
    expect(triggerOf({ ...draft, kind: "pull_request" }, repos)).toEqual({
      kind: "pull_request",
      repo_id: "api",
    });
  });
});

describe("sessionOf", () => {
  const draft: SessionDraft = {
    prompt: " Do {{date}} ",
    choices: [{ repoId: "api", mode: "read" }],
    workflow: "review",
    runner: "",
    model: "  ",
    mode: "autonomous",
  };

  it("sends the prompt as typed, the repos, and nothing for what was never chosen", () => {
    expect(sessionOf(draft, { workflow: undefined, runner: "claude" })).toEqual({
      prompt: " Do {{date}} ",
      workflow: null,
      repos: [{ repo_id: "api", mode: "read" }],
      runner: null,
      model: null,
      permission_mode: "autonomous",
    });
  });

  it("sends the workflow and runner offered for a choice, and the model trimmed", () => {
    expect(
      sessionOf(
        { ...draft, runner: "gone", model: " opus " },
        { workflow: "review", runner: "claude" },
      ),
    ).toMatchObject({ workflow: "review", runner: "claude", model: "opus" });
    expect(
      sessionOf({ ...draft, runner: "mock" }, { workflow: undefined, runner: undefined }).runner,
    ).toBeNull();
  });
});

describe("saveFailure and messageAt", () => {
  it("places a 400 at the field its words name", () => {
    expect(saveFailure(badRequest("repo web is gone"), "issue")).toEqual({
      message: "repo web is gone",
      field: "trigger-repo",
    });
  });

  it("places any other refusal by Save", () => {
    expect(saveFailure(conflict("name is taken"), "schedule")).toEqual({
      message: "name is taken",
      field: undefined,
    });
    expect(saveFailure(new Error("Cannot reach the server"), "schedule").field).toBeUndefined();
  });

  it("shows the message at its field only, and by Save only when it is about none", () => {
    const atName = { message: "name is empty", field: "name" } as const;
    const atNone = { message: "busy" };
    expect(messageAt(atName, "name")).toBe("name is empty");
    expect(messageAt(atName, "cron")).toBeUndefined();
    expect(messageAt(atName, undefined)).toBeUndefined();
    expect(messageAt(atNone, undefined)).toBe("busy");
    expect(messageAt(atNone, "name")).toBeUndefined();
    expect(messageAt(undefined, undefined)).toBeUndefined();
  });
});

describe("blameField", () => {
  it.each([
    ["name must not be empty", "name"],
    ["Name: too long", "name"],
    ["invalid cron: expected 5 fields", "cron"],
    ["the schedule never fires", "cron"],
    ["bad expression", "cron"],
    ["unknown placeholder {{foo}}", "prompt"],
    ["the template does not parse", "prompt"],
    ["prompt must not be empty", "prompt"],
    ["{{nope}} is not known", "prompt"],
    ["permission mode is refused", "permission"],
    ["Ask needs someone to answer", "permission"],
    ["it runs unattended", "permission"],
    ["unknown workflow review", "workflow"],
  ] as const)("blames %j on %s", (message, field) => {
    expect(blameField(message, "schedule")).toBe(field);
    expect(blameField(message, "issue")).toBe(field);
  });

  it("blames a repo on the session's repos for a schedule, on the watched repo for a trigger", () => {
    expect(blameField("repo web is not available", "schedule")).toBe("repos");
    expect(blameField("repo web is not available", "issue")).toBe("trigger-repo");
    expect(blameField("repo web is not available", "pull_request")).toBe("trigger-repo");
  });

  it("reads whole words for name and ask, and the first rule that fits wins", () => {
    expect(blameField("rename failed", "schedule")).toBeUndefined();
    expect(blameField("task failed", "schedule")).toBeUndefined();
    expect(blameField("name of the workflow is wrong", "schedule")).toBe("name");
    expect(blameField("workflow repo is gone", "issue")).toBe("workflow");
  });

  it("blames nothing when no word fits", () => {
    expect(blameField("something else entirely", "schedule")).toBeUndefined();
  });
});
