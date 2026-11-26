import type { ItemStatus } from "../gen/ItemStatus";
import type { PlanStatus } from "../gen/PlanStatus";
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

const stoppable: readonly SessionStatus[] = ["queued", "preparing", "running", "waiting"];
const messageable: readonly SessionStatus[] = ["running", "review", "waiting"];

export const canStop = (status: SessionStatus | undefined): boolean =>
  status !== undefined && stoppable.includes(status);

export const canMessage = (status: SessionStatus | undefined): boolean =>
  status !== undefined && messageable.includes(status);
