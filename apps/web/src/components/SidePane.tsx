import { createSignal, onSettled, Show, type ParentProps } from "solid-js";
import { loadSidePaneWidth, saveSidePaneWidth } from "../lib/prefs";
import type { SidePanelId } from "../panels/registry";
import { PanelTabs, tabId, tabPanelId, type Tab } from "./PanelTabs";

const MIN_WIDTH = 280;
const DEFAULT_WIDTH = 440;
/** What the transcript beside the pane always keeps. */
const MIN_REST = 320;
const KEY_STEP = 24;

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
  let dragging = false;

  const limit = (): number =>
    Math.max(MIN_WIDTH, (pane?.parentElement?.clientWidth ?? 0) - MIN_REST);
  const clamp = (value: number): number => Math.min(limit(), Math.max(MIN_WIDTH, value));

  onSettled(() => {
    setWidest(limit());
  });

  function resize(next: number, persist: boolean): void {
    const clamped = clamp(next);
    setWidth(clamped);
    setWidest(limit());
    if (persist) saveSidePaneWidth(clamped);
  }

  function onKeyDown(event: KeyboardEvent): void {
    const current = pane?.offsetWidth ?? MIN_WIDTH;
    // The edge is on the pane's left: moving it left makes the pane wider.
    if (event.key === "ArrowLeft") resize(current + KEY_STEP, true);
    else if (event.key === "ArrowRight") resize(current - KEY_STEP, true);
    else if (event.key === "Home") resize(MIN_WIDTH, true);
    else if (event.key === "End") resize(limit(), true);
    else return;
    event.preventDefault();
  }

  return (
    <>
      <Show when={props.panel !== undefined}>
        <div
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize the panel"
          aria-valuemin={MIN_WIDTH}
          aria-valuemax={widest()}
          aria-valuenow={width()}
          aria-valuetext={`${Math.round(width())} pixels wide`}
          tabindex="0"
          class="w-1.5 shrink-0 cursor-col-resize touch-none border-l border-line bg-bg -outline-offset-2 hover:bg-line"
          onKeyDown={onKeyDown}
          onPointerDown={(event) => {
            dragging = true;
            event.currentTarget.setPointerCapture(event.pointerId);
            event.preventDefault();
          }}
          onPointerMove={(event) => {
            if (dragging && pane) resize(pane.getBoundingClientRect().right - event.clientX, false);
          }}
          onPointerUp={() => {
            if (!dragging) return;
            dragging = false;
            saveSidePaneWidth(width());
          }}
          onPointerCancel={() => {
            dragging = false;
          }}
        />
      </Show>
      <aside
        ref={(element) => (pane = element)}
        aria-label="Workspace panels"
        class={[
          "flex min-h-0 shrink-0 flex-col bg-bg",
          { "border-l border-line": props.panel === undefined },
        ]}
        // The stylesheet has the last word, so a width remembered from a larger window still fits.
        style={
          props.panel === undefined
            ? undefined
            : { width: `${width()}px`, "max-width": `calc(100% - ${MIN_REST}px)` }
        }
      >
        <div
          class={[
            "flex",
            props.panel === undefined ? "flex-col" : "items-center border-b border-line bg-surface",
          ]}
        >
          <PanelTabs
            label="Workspace panels"
            idBase={props.idBase}
            tabs={props.tabs}
            active={props.panel}
            orientation={props.panel === undefined ? "vertical" : "horizontal"}
            onSelect={props.onSelect}
          />
          <Show when={props.panel !== undefined}>
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
        <div
          role={props.panel === undefined ? undefined : "tabpanel"}
          id={tabPanelId(props.idBase, props.panel ?? "none")}
          aria-labelledby={props.panel === undefined ? undefined : tabId(props.idBase, props.panel)}
          hidden={props.panel === undefined}
          class="flex min-h-0 flex-1 flex-col"
        >
          {props.children}
        </div>
      </aside>
    </>
  );
}
