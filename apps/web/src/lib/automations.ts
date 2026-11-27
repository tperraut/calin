import type { Automation } from "../gen/Automation";
import type { AutomationOutcome } from "../gen/AutomationOutcome";
import type { AutomationRequest } from "../gen/AutomationRequest";
import type { AutomationRun } from "../gen/AutomationRun";
import type { AutomationTrigger } from "../gen/AutomationTrigger";
import type { PermissionMode } from "../gen/PermissionMode";

export type TriggerKind = AutomationTrigger["kind"];

export const triggerKinds: readonly { kind: TriggerKind; label: string }[] = [
  { kind: "schedule", label: "Schedule" },
  { kind: "issue", label: "New issue" },
  { kind: "pull_request", label: "New pull request" },
];

/** Shortcuts for the cron field, in the server's local time. */
export const cronPresets: readonly { label: string; expr: string }[] = [
  { label: "Every day at 08:00", expr: "0 8 * * *" },
  { label: "Every weekday at 08:00", expr: "0 8 * * 1-5" },
  { label: "Every hour", expr: "0 * * * *" },
  { label: "Every Monday at 09:00", expr: "0 9 * * 1" },
];

/** The prompt's placeholders for each kind of trigger (integrations spec, section 3, "Definition"). */
export function placeholders(kind: TriggerKind): readonly string[] {
  return kind === "schedule"
    ? ["{{date}}"]
    : ["{{date}}", "{{number}}", "{{title}}", "{{body}}", "{{url}}", "{{author}}"];
}

/** An automated session runs unattended: `ask` is refused (spec, section 3). */
export const automationModes: readonly PermissionMode[] = ["accept_edits", "autonomous"];

/** What `PUT` takes to change only `enabled`: the whole automation as it is. */
export function requestOf(
  automation: Automation,
  patch: Partial<AutomationRequest> = {},
): AutomationRequest {
  return {
    name: automation.name,
    enabled: automation.enabled,
    trigger: automation.trigger,
    session: automation.session,
    ...patch,
  };
}

const outcomeWords: Record<AutomationOutcome, string> = {
  started: "Started a session",
  skipped: "Skipped",
  failed: "Failed",
};

/** "Started a session", "Skipped: the previous session is still active", "Failed: <why>". */
export function outcomeText(run: AutomationRun): string {
  const words = outcomeWords[run.outcome] ?? run.outcome;
  if (run.outcome === "started" || run.detail === null || run.detail === "") return words;
  return `${words}: ${run.detail}`;
}

/** The cause as the server words it, capitalized: "Schedule", "Issue #12", "Manual". */
export const causeText = (cause: string): string =>
  cause === "" ? cause : cause[0]?.toUpperCase() + cause.slice(1);

/** What the number the "Run now" form asks for is called, for a trigger that needs one. */
export function runNumberLabel(trigger: AutomationTrigger): string | undefined {
  if (trigger.kind === "issue") return "Issue number";
  if (trigger.kind === "pull_request") return "Pull request number";
  return undefined;
}

/** The repo a trigger watches, if it watches one. */
export const triggerRepo = (trigger: AutomationTrigger): string | undefined =>
  trigger.kind === "schedule" ? undefined : trigger.repo_id;

export type BlamedField =
  | "name"
  | "cron"
  | "trigger-repo"
  | "prompt"
  | "workflow"
  | "permission"
  | "repos";

/**
 * The field a 400 from saving is most likely about, from the words of the
 * server's message (api.rs, "Usage, issues and automations"). Nothing when no
 * word fits: the message then stands by the Save button.
 */
export function blameField(message: string, kind: TriggerKind): BlamedField | undefined {
  const text = message.toLowerCase();
  if (/\bname\b/.test(text)) return "name";
  if (/cron|schedule|expression/.test(text)) return "cron";
  if (/placeholder|template|prompt|\{\{/.test(text)) return "prompt";
  if (/permission|\bask\b|unattended/.test(text)) return "permission";
  if (/workflow/.test(text)) return "workflow";
  if (/repo/.test(text)) return kind === "schedule" ? "repos" : "trigger-repo";
  return undefined;
}
