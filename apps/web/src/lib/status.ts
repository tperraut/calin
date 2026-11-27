import type { AgentStatus } from "../gen/AgentStatus";
import type { DecisionSource } from "../gen/DecisionSource";
import type { ItemStatus } from "../gen/ItemStatus";
import type { PermissionMode } from "../gen/PermissionMode";
import type { PlanStatus } from "../gen/PlanStatus";
import type { RepoMode } from "../gen/RepoMode";
import type { SessionStatus } from "../gen/SessionStatus";

/** Color family of a badge. The label always carries the meaning as well. */
export type Tone = "neutral" | "active" | "attention" | "success" | "danger";

export type Look = { label: string; tone: Tone };

// Keyed by the generated unions, so a status added to the contract fails the type-check here.
const sessionLooks: Record<SessionStatus, Look> = {
  queued: { label: "Queued", tone: "neutral" },
  preparing: { label: "Preparing", tone: "active" },
  running: { label: "Running", tone: "active" },
  waiting: { label: "Waiting", tone: "attention" },
  review: { label: "Review", tone: "attention" },
  shipping: { label: "Shipping", tone: "active" },
  done: { label: "Done", tone: "success" },
  failed: { label: "Failed", tone: "danger" },
  cancelled: { label: "Cancelled", tone: "neutral" },
};

const itemLooks: Record<ItemStatus, Look> = {
  running: { label: "Running", tone: "active" },
  completed: { label: "Done", tone: "success" },
  failed: { label: "Failed", tone: "danger" },
  declined: { label: "Declined", tone: "attention" },
};

const agentLooks: Record<AgentStatus, Look> = {
  starting: { label: "Starting", tone: "active" },
  running: { label: "Running", tone: "active" },
  idle: { label: "Idle", tone: "neutral" },
  done: { label: "Done", tone: "success" },
  failed: { label: "Failed", tone: "danger" },
  stopped: { label: "Stopped", tone: "neutral" },
};

/** Statuses after which an agent does nothing more. */
const agentOver: readonly AgentStatus[] = ["done", "failed", "stopped"];

export const agentFinished = (status: AgentStatus): boolean => agentOver.includes(status);

export type PlanLook = { label: string; mark: string };

const planLooks: Record<PlanStatus, PlanLook> = {
  pending: { label: "Pending", mark: "[ ]" },
  in_progress: { label: "In progress", mark: "[~]" },
  completed: { label: "Completed", mark: "[x]" },
};

// The lookups below tolerate a value a newer server may send: it is shown
// under its wire name instead of breaking the screen.
const lookup = <T>(table: Record<string, T>, key: string): T | undefined => table[key];

export const sessionLook = (status: SessionStatus): Look =>
  lookup(sessionLooks, status) ?? { label: status, tone: "neutral" };

export const agentLook = (status: AgentStatus): Look =>
  lookup(agentLooks, status) ?? { label: status, tone: "neutral" };

export const itemLook = (status: ItemStatus): Look =>
  lookup(itemLooks, status) ?? { label: status, tone: "neutral" };

export const planLook = (status: PlanStatus): PlanLook =>
  lookup(planLooks, status) ?? { label: status, mark: "[?]" };

/** What a running item shows once its turn has ended without it. */
export const interruptedLook: Look = { label: "Interrupted", tone: "neutral" };

export type ModeLook = { label: string; hint: string };

const permissionModeLooks: Record<PermissionMode, ModeLook> = {
  ask: { label: "Ask", hint: "Every tool call waits for you." },
  accept_edits: { label: "Accept edits", hint: "File edits are allowed, commands wait." },
  autonomous: { label: "Autonomous", hint: "Nothing waits, everything is still recorded." },
};

/** In the order the composer offers them; the first is the default (api.rs). */
export const permissionModes: readonly PermissionMode[] = ["ask", "accept_edits", "autonomous"];

export const permissionModeLook = (mode: PermissionMode): ModeLook =>
  lookup(permissionModeLooks, mode) ?? { label: mode, hint: "" };

const repoModeLabels: Record<RepoMode, string> = {
  write: "Worktree",
  read: "Read only",
  live: "Edit in place",
};

export const repoModeLabel = (mode: RepoMode): string => lookup(repoModeLabels, mode) ?? mode;

/** In the order they are offered; a worktree is the default (repo.rs). */
export const repoModes: readonly RepoMode[] = ["write", "read", "live"];

/**
 * How a repo is attached unless the reader says otherwise: the home repo is
 * edited in place, any other one gets a worktree (design spec, section 13).
 */
export const defaultRepoMode = (repo: { home: boolean }): Extract<RepoMode, "write" | "live"> =>
  repo.home ? "live" : "write";

// `expired` is nobody answering; it reads as "No longer needed", never after "by".
const sourceLabels: Record<Exclude<DecisionSource, "expired">, string> = {
  user: "you",
  policy: "policy",
};

/** Who answered a permission request, as it reads after "by". */
export const decisionSourceLabel = (by: Exclude<DecisionSource, "expired">): string =>
  lookup(sourceLabels, by) ?? by;

// The lists below restate the "Semantics" section of calin-proto/src/api.rs.
const interruptible: readonly SessionStatus[] = ["running", "waiting"];
const stoppable: readonly SessionStatus[] = [
  "queued",
  "preparing",
  "running",
  "waiting",
  "review",
  "shipping",
];
// "While `shipping`": a message starts a turn as it does in `review`.
const messageable: readonly SessionStatus[] = [
  "running",
  "review",
  "shipping",
  "waiting",
  "failed",
  "cancelled",
];
const shippable: readonly SessionStatus[] = ["review", "shipping"];
const resumable: readonly SessionStatus[] = ["failed", "cancelled"];
const attachable: readonly SessionStatus[] = ["running", "waiting", "review"];

const among = (list: readonly SessionStatus[], status: SessionStatus | undefined): boolean =>
  status !== undefined && list.includes(status);

export const canInterrupt = (status: SessionStatus | undefined): boolean =>
  among(interruptible, status);

export const canStop = (status: SessionStatus | undefined): boolean => among(stoppable, status);

export const canMessage = (status: SessionStatus | undefined): boolean =>
  among(messageable, status);

/** True while a repo can be attached to the session (api.rs, "Attaching mid-session"). */
export const canAttach = (status: SessionStatus | undefined): boolean => among(attachable, status);

/** True while the session's pull requests can be opened or updated (api.rs, "Shipping"). */
export const canShip = (status: SessionStatus | undefined): boolean => among(shippable, status);

/** True when a message restarts the agent instead of joining a live conversation. */
export const resumesOnMessage = (status: SessionStatus | undefined): boolean =>
  among(resumable, status);
