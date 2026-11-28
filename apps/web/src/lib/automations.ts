import { HttpError } from "../api/http";
import type { Automation } from "../gen/Automation";
import type { AutomationOutcome } from "../gen/AutomationOutcome";
import type { AutomationRequest } from "../gen/AutomationRequest";
import type { AutomationRun } from "../gen/AutomationRun";
import type { AutomationSession } from "../gen/AutomationSession";
import type { AutomationTrigger } from "../gen/AutomationTrigger";
import type { PermissionMode } from "../gen/PermissionMode";
import type { Repo } from "../gen/Repo";
import { errorMessage } from "./errors";
import { blankAsNull } from "./format";
import type { RepoChoice } from "./prefs";

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

/** The number typed for Run now ("12" or "#12"), when it is a whole number above zero. */
export function runNumber(typed: string): number | undefined {
  const wanted = Number(typed.trim().replace(/^#/, ""));
  return Number.isInteger(wanted) && wanted > 0 ? wanted : undefined;
}

/** What Run now asks the server for: no number for a schedule, else the one typed, or why not. */
export function runWanted(
  trigger: AutomationTrigger,
  typed: string,
): { number: number | undefined } | { problem: string } {
  const label = runNumberLabel(trigger);
  if (label === undefined) return { number: undefined };
  const number = runNumber(typed);
  if (number === undefined)
    return { problem: `Give the ${label.toLowerCase()}, a whole number such as 12.` };
  return { number };
}

/** The trigger fields as the form holds them; every kind keeps its value while another is shown. */
export type TriggerDraft = { kind: TriggerKind; cron: string; repo: string; label: string };

/** The session fields as the form holds them. `runner` is empty until one is chosen. */
export type SessionDraft = {
  prompt: string;
  choices: readonly RepoChoice[];
  workflow: string | undefined;
  runner: string;
  model: string;
  mode: PermissionMode;
};

/** The form's first values: `stored` as saved, or a new automation's when it is absent. */
export function automationDraft(stored: Automation | undefined): {
  name: string;
  enabled: boolean;
  trigger: TriggerDraft;
  session: SessionDraft;
} {
  return {
    name: stored?.name ?? "",
    enabled: stored?.enabled ?? false,
    trigger: triggerDraft(stored?.trigger),
    session: sessionDraft(stored?.session),
  };
}

function triggerDraft(trigger: AutomationTrigger | undefined): TriggerDraft {
  const blank: TriggerDraft = { kind: "schedule", cron: "0 8 * * 1-5", repo: "", label: "" };
  if (trigger === undefined) return blank;
  switch (trigger.kind) {
    case "schedule":
      return { ...blank, cron: trigger.cron };
    case "issue":
      return { ...blank, kind: "issue", repo: trigger.repo_id, label: trigger.label ?? "" };
    case "pull_request":
      return { ...blank, kind: "pull_request", repo: trigger.repo_id };
  }
}

function sessionDraft(session: AutomationSession | undefined): SessionDraft {
  if (session === undefined)
    return {
      prompt: "",
      choices: [],
      workflow: undefined,
      runner: "",
      model: "",
      mode: "accept_edits",
    };
  return {
    prompt: session.prompt,
    // Absent means a worktree (repo.rs); every other mode is kept as it was saved.
    choices: session.repos.map((repo) => ({ repoId: repo.repo_id, mode: repo.mode ?? "write" })),
    workflow: session.workflow ?? undefined,
    runner: session.runner ?? "",
    model: session.model ?? "",
    mode: automationModes.includes(session.permission_mode)
      ? session.permission_mode
      : "accept_edits",
  };
}

/** The repo a trigger watches: the chosen one, else the first that can be used, else the first. */
export function watchedRepo(chosen: string, repos: readonly Repo[]): string {
  if (chosen !== "") return chosen;
  return repos.find((repo) => repo.available)?.id ?? repos[0]?.id ?? "";
}

/** The trigger the fields describe, watching `watchedRepo` when its kind watches one. */
export function triggerOf(draft: TriggerDraft, repos: readonly Repo[]): AutomationTrigger {
  switch (draft.kind) {
    case "schedule":
      return { kind: "schedule", cron: draft.cron.trim() };
    case "issue":
      return {
        kind: "issue",
        repo_id: watchedRepo(draft.repo, repos),
        label: blankAsNull(draft.label),
      };
    case "pull_request":
      return { kind: "pull_request", repo_id: watchedRepo(draft.repo, repos) };
  }
}

/**
 * The session the fields describe. `offered` is what the server offers for the
 * draft's choices: the workflow to send (none for Quick) and the runner it falls back to.
 */
export function sessionOf(
  draft: SessionDraft,
  offered: { workflow: string | undefined; runner: string | undefined },
): AutomationSession {
  return {
    prompt: draft.prompt,
    workflow: offered.workflow ?? null,
    repos: draft.choices.map((choice) => ({ repo_id: choice.repoId, mode: choice.mode })),
    // Never chosen: the server's default runner, whichever it is then.
    runner: draft.runner === "" ? null : (offered.runner ?? null),
    model: blankAsNull(draft.model),
    permission_mode: draft.mode,
  };
}

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

/** What the form shows when a save is refused; only a 400 is about a field. */
export type SaveFailure = { message: string; field?: BlamedField };

export function saveFailure(reason: unknown, kind: TriggerKind): SaveFailure {
  const message = errorMessage(reason);
  const invalid = reason instanceof HttpError && reason.status === 400;
  return { message, field: invalid ? blameField(message, kind) : undefined };
}

/** The message shown at `field`, or by Save (`field` absent) when it is about no field. */
export const messageAt = (
  failure: SaveFailure | undefined,
  field: BlamedField | undefined,
): string | undefined => (failure?.field === field ? failure?.message : undefined);
