import { describe, expect, it } from "vite-plus/test";
import { blameField } from "./automations";

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
