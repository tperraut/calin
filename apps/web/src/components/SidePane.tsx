import { createSignal, onSettled, Show, type ParentProps } from "solid-js";
import { loadSidePaneWidth, saveSidePaneWidth } from "../lib/prefs";
import type { SidePanelId } from "../panels/registry";
import { PanelTabs, tabId, tabPanelId, type Tab } from "./PanelTabs";

const MIN_WIDTH = 280;
const DEFAULT_WIDTH = 440;
/** What the transcript beside the pane always keeps. */
const MIN_REST = 320;
const KEY_STEP = 24;

/** The widest the pane may be: what its row leaves once the transcript has its share. */
function widestIn(pane: HTMLElement | undefined): number {
  return Math.max(MIN_WIDTH, (pane?.parentElement?.clientWidth ?? 0) - MIN_REST);
}

/** The width a key on the edge asks for; undefined leaves the key to the browser. */
function keyedWidth(key: string, current: number, widest: number): number | undefined {
  // The edge is on the pane's left: moving it left makes the pane wider.
  return new Map([
    ["ArrowLeft", current + KEY_STEP],
    ["ArrowRight", current - KEY_STEP],
    ["Home", MIN_WIDTH],
    ["End", widest],
  ]).get(key);
}

/**
 * The right-hand pane of the wide layout: the side panels behind their own
 * tabs. Collapsed (no `panel`) it is only the tabs, as a rail; open, it is
 * resizable by dragging its edge or with the arrow keys on it. Its children
 * are the panels themselves, kept in place (hidden) while it is collapsed.
 */
export function SidePane(
  props: ParentProps<{
    idBase: string;
    tabs: readonly Tab<SidePanelId>[];
    panel: SidePanelId | undefined;
    onSelect: (panel: SidePanelId) => void;
    onCollapse: () => void;
  }>,
) {
  const [width, setWidth] = createSignal(loadSidePaneWidth() ?? DEFAULT_WIDTH);
  // Depends on the window, so it is measured: once shown, then with every resize of the pane.
  const [widest, setWidest] = createSignal<number>();
  let pane: HTMLElement | undefined;
  const collapsed = (): boolean => props.panel === undefined;

  onSettled(() => {
    setWidest(widestIn(pane));
  });

  function resize(next: number, persist: boolean): void {
    const clamped = Math.min(widestIn(pane), Math.max(MIN_WIDTH, next));
    setWidth(clamped);
    setWidest(widestIn(pane));
    if (persist) saveSidePaneWidth(clamped);
  }

  return (
    <>
      <Show when={!collapsed()}>
        <ResizeEdge
          width={width()}
          widest={widest()}
          pane={() => pane}
          onResize={resize}
          onRelease={() => saveSidePaneWidth(width())}
        />
      </Show>
      <aside
        ref={(element) => (pane = element)}
        aria-label="Workspace panels"
        class={["flex min-h-0 shrink-0 flex-col bg-bg", { "border-l border-line": collapsed() }]}
        // The stylesheet has the last word, so a width remembered from a larger window still fits.
        style={
          collapsed()
            ? undefined
            : { width: `${width()}px`, "max-width": `calc(100% - ${MIN_REST}px)` }
        }
      >
        <div
          class={[
            "flex",
            collapsed() ? "flex-col" : "items-center border-b border-line bg-surface",
          ]}
        >
          <PanelTabs
            label="Workspace panels"
            idBase={props.idBase}
            tabs={props.tabs}
            active={props.panel}
            orientation={collapsed() ? "vertical" : "horizontal"}
            onSelect={props.onSelect}
          />
          <Show when={!collapsed()}>
            <button
              type="button"
              class="btn btn-ghost px-0"
              aria-label="Collapse the panel"
              title="Collapse the panel"
              onClick={() => props.onCollapse()}
            >
              <span aria-hidden="true">⇥</span>
            </button>
          </Show>
        </div>
        <PaneBody idBase={props.idBase} panel={props.panel}>
          {props.children}
        </PaneBody>
      </aside>
    </>
  );
}

/** The pane's left edge: dragged, or moved with the keys, it resizes the pane. */
function ResizeEdge(props: {
  width: number;
  widest: number | undefined;
  pane: () => HTMLElement | undefined;
  onResize: (next: number, persist: boolean) => void;
  /** A drag let go: the width it left is the one to remember. */
  onRelease: () => void;
}) {
  let dragging = false;

  function onKeyDown(event: KeyboardEvent): void {
    const pane = props.pane();
    const next = keyedWidth(event.key, pane?.offsetWidth ?? MIN_WIDTH, widestIn(pane));
    if (next === undefined) return;
    props.onResize(next, true);
    event.preventDefault();
  }

  function onPointerMove(event: PointerEvent): void {
    const pane = props.pane();
    if (dragging && pane) props.onResize(pane.getBoundingClientRect().right - event.clientX, false);
  }

  function onPointerUp(): void {
    if (!dragging) return;
    dragging = false;
    props.onRelease();
  }

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the panel"
      aria-valuemin={MIN_WIDTH}
      aria-valuemax={props.widest}
      aria-valuenow={props.width}
      aria-valuetext={`${Math.round(props.width)} pixels wide`}
      tabindex="0"
      class="w-1.5 shrink-0 cursor-col-resize touch-none border-l border-line bg-bg -outline-offset-2 hover:bg-line"
      onKeyDown={onKeyDown}
      onPointerDown={(event) => {
        dragging = true;
        event.currentTarget.setPointerCapture(event.pointerId);
        event.preventDefault();
      }}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={() => {
        dragging = false;
      }}
    />
  );
}

/** Where the panels live: the open one's tabpanel, or hidden while the pane is collapsed. */
function PaneBody(props: ParentProps<{ idBase: string; panel: SidePanelId | undefined }>) {
  return (
    <div
      role={props.panel === undefined ? undefined : "tabpanel"}
      id={tabPanelId(props.idBase, props.panel ?? "none")}
      aria-labelledby={props.panel === undefined ? undefined : tabId(props.idBase, props.panel)}
      hidden={props.panel === undefined}
      class="flex min-h-0 flex-1 flex-col"
    >
      {props.children}
    </div>
  );
}
