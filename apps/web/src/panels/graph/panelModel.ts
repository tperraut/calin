// What the Graph panel decides apart from its markup. Pure.
import type { Timeline } from "../../gen/Timeline";
import type { TimelineArrow } from "../../gen/TimelineArrow";
import type { TimelineSpan } from "../../gen/TimelineSpan";
import type { WorkflowRun } from "../../gen/WorkflowRun";
import type { TranscriptEntry } from "../../state/sessionReducer";
import type { OrchestrationView } from "./data";
import type { LaneView } from "./timelineModel";
import type { TimelineSelection } from "./timelineNav";
import { ACROSS_MIN_WIDTH, type Orientation } from "./workflowLayout";

export type PanelView = OrchestrationView | "workflow";

const views: readonly { id: PanelView; label: string }[] = [
  { id: "graph", label: "Graph" },
  { id: "timeline", label: "Timeline" },
  { id: "workflow", label: "Workflow" },
];

/** The views offered: the workflow only to a session that runs one. */
export const viewChoices = (hasWorkflow: boolean): readonly { id: PanelView; label: string }[] =>
  views.filter((one) => one.id !== "workflow" || hasWorkflow);

/** The view on screen: what the reader chose; until they do, a session that runs a workflow opens on it. */
export function shownView(chosen: PanelView | undefined, hasWorkflow: boolean): PanelView {
  if (chosen === "workflow" && !hasWorkflow) return "graph";
  return chosen ?? (hasWorkflow ? "workflow" : "graph");
}

/** Top to bottom on a narrow pane; across until the pane's width is known. */
export const orientationFor = (width: number): Orientation =>
  width > 0 && width < ACROSS_MIN_WIDTH ? "down" : "across";

/** What is announced when a node starts running. */
export function nowRunning(run: WorkflowRun | undefined): string {
  const active = run?.nodes.find((node) => node.state === "active")?.id;
  return active === undefined ? "" : `Now running: ${active}`;
}

/** The key of a workflow node's newest row in the transcript: where it last started or finished. */
export const nodeRowKey = (
  transcript: readonly TranscriptEntry[],
  nodeId: string,
): string | undefined =>
  transcript.findLast(
    (entry) =>
      entry.kind === "workflow" &&
      (entry.note.what === "node_started" || entry.note.what === "node_finished") &&
      entry.note.nodeId === nodeId,
  )?.key;

export type PickedSpan = { lane: LaneView; span: TimelineSpan };

/** The span picked in the timeline, while its lane still holds it. */
export function pickedSpan(
  lanes: readonly LaneView[],
  chosen: TimelineSelection | undefined,
): PickedSpan | undefined {
  if (chosen?.kind !== "span") return undefined;
  const lane = lanes.find((one) => one.id === chosen.agentId);
  const span = lane?.lane.spans.find((one) => one.id === chosen.id);
  return lane && span ? { lane, span } : undefined;
}

/** The arrow picked in the timeline, while the timeline still holds it. */
export function pickedArrow(
  timeline: Timeline | undefined,
  chosen: TimelineSelection | undefined,
): TimelineArrow | undefined {
  if (chosen?.kind !== "arrow") return undefined;
  return timeline?.arrows.find((arrow) => arrow.id === chosen.id);
}

/** Who an arrow comes from or goes to: the user, an agent's lane, or an agent the timeline has no lane for. */
export const laneName = (lanes: readonly LaneView[], agentId: string | null): string =>
  agentId === null ? "You" : (lanes.find((lane) => lane.id === agentId)?.label ?? "An agent");

/** The item a picked span can be shown as: a tool call of the open session that the transcript holds. */
export function spanItem(
  picked: PickedSpan | undefined,
  sessionId: string,
  transcript: readonly TranscriptEntry[],
): string | undefined {
  if (!picked || picked.span.kind !== "tool" || picked.lane.lane.session_id !== sessionId) {
    return undefined;
  }
  const itemId = picked.span.id;
  return transcript.some((entry) => entry.kind === "item" && entry.item.id === itemId)
    ? itemId
    : undefined;
}
