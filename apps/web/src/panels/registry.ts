import { lazy, type Component } from "solid-js";
import type { SessionStore } from "../state/sessionStore";

/**
 * What every workspace panel receives. The store is the session's live state:
 * `store.repos()` is its repo set (primary first), `store.view().changedFiles`
 * the files touched so far, and `store.view().changesRevision` goes up with
 * every file change, so a panel refetches when it moves.
 */
export type PanelProps = { store: SessionStore };

export type SidePanelId = "changes" | "files" | "terminal";

/** The transcript is the session itself; the others are the panels beside it. */
export type PanelId = "transcript" | SidePanelId;

export type SidePanel = {
  /** Also the last segment of the panel's address: `/s/:id/<id>`. */
  id: SidePanelId;
  label: string;
  component: Component<PanelProps>;
};

/**
 * The panels beside the transcript, in tab order. To add one: write its
 * component as the default export of a module in this folder, add its id to
 * `SidePanelId` and an entry here. Each is its own chunk, fetched the first
 * time it is shown, so an editor or a terminal costs nothing until it is opened.
 */
export const sidePanels: readonly SidePanel[] = [
  { id: "changes", label: "Changes", component: lazy(() => import("./ChangesPanel")) },
  { id: "files", label: "Files", component: lazy(() => import("./FilesPanel")) },
  { id: "terminal", label: "Terminal", component: lazy(() => import("./TerminalPanel")) },
];

export const sidePanelIds: readonly string[] = sidePanels.map((panel) => panel.id);

/** The panel an address segment names; the transcript when there is none. */
export function panelFromSegment(segment: string | undefined): PanelId {
  return sidePanels.find((panel) => panel.id === segment)?.id ?? "transcript";
}

/** Address of a session, on the given panel. */
export function sessionPath(id: string, panel: PanelId = "transcript"): string {
  const base = `/s/${encodeURIComponent(id)}`;
  return panel === "transcript" ? base : `${base}/${panel}`;
}
