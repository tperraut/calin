import type { DecisionSource } from "../gen/DecisionSource";
import type { FileOp } from "../gen/FileOp";
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

const sourceLabels: Record<DecisionSource, string> = { user: "you", policy: "policy" };

/** Who answered a permission request, as it reads after "by". */
export const decisionSourceLabel = (by: DecisionSource): string => lookup(sourceLabels, by) ?? by;

const fileOpLabels: Record<FileOp, string> = {
  create: "Created",
  edit: "Edited",
  delete: "Deleted",
  rename: "Renamed",
};

export const fileOpLabel = (op: FileOp): string => lookup(fileOpLabels, op) ?? op;

// The lists below restate the "Semantics" section of calin-proto/src/api.rs.
const interruptible: readonly SessionStatus[] = ["running", "waiting"];
const stoppable: readonly SessionStatus[] = ["queued", "preparing", "running", "waiting", "review"];
const messageable: readonly SessionStatus[] = [
  "running",
  "review",
  "waiting",
  "failed",
  "cancelled",
];
const resumable: readonly SessionStatus[] = ["failed", "cancelled"];

const among = (list: readonly SessionStatus[], status: SessionStatus | undefined): boolean =>
  status !== undefined && list.includes(status);

export const canInterrupt = (status: SessionStatus | undefined): boolean =>
  among(interruptible, status);

export const canStop = (status: SessionStatus | undefined): boolean => among(stoppable, status);

export const canMessage = (status: SessionStatus | undefined): boolean =>
  among(messageable, status);

/** True when a message restarts the agent instead of joining a live conversation. */
export const resumesOnMessage = (status: SessionStatus | undefined): boolean =>
  among(resumable, status);
