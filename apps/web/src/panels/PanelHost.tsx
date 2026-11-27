import { useBeforeLeave } from "@solidjs/router";
import { createMemo, createSignal, Errored, For, Loading, Show } from "solid-js";
import { errorMessage } from "../lib/errors";
import type { SessionStore } from "../state/sessionStore";
import {
  sessionPath,
  sidePanels,
  type FileRequest,
  type PanelLinks,
  type SidePanel,
  type SidePanelId,
} from "./registry";

function KeptPanel(props: {
  definition: SidePanel;
  store: SessionStore;
  active: boolean;
  links: PanelLinks;
}) {
  return (
    // Hidden, not unmounted: an editor keeps its unsaved text and a terminal its shell.
    <div hidden={!props.active} class="flex min-h-0 min-w-0 flex-1 flex-col">
      {/* One boundary per panel: its code is fetched when it is first shown, and a failed
          fetch (a deploy in between, a dropped connection) takes down that panel only. */}
      <Errored
        fallback={(error) => (
          <div role="alert" class="flex flex-1 flex-col items-center justify-center gap-3 p-6">
            <p class="text-sm">This panel could not be loaded: {errorMessage(error())}</p>
            <button type="button" class="btn" onClick={() => location.reload()}>
              Reload
            </button>
          </div>
        )}
      >
        <Loading fallback={<p class="p-3 text-sm text-muted">Loading…</p>}>
          <props.definition.component
            store={props.store}
            active={props.active}
            links={props.links}
          />
        </Loading>
      </Errored>
    </div>
  );
}

/**
 * The side panels of one session. A panel is mounted the first time it is
 * shown and then kept, hidden while another one shows, until the session
 * screen itself goes away; a panel that was never opened is never loaded.
 *
 * The host is created once per session screen and renders into one element,
 * handed to `ref`. The layout on screen adopts that element through
 * `PanelOutlet`, so switching between the phone and the wide layout moves the
 * live panels instead of mounting them again.
 */
export function PanelHost(props: {
  store: SessionStore;
  /** The panel on screen; none while the transcript has the screen or the side pane is collapsed. */
  panel: SidePanelId | undefined;
  onPanel: (panel: SidePanelId) => void;
  ref: (element: HTMLDivElement) => void;
}) {
  // Grows only: a panel joins when it is first shown.
  const opened = createMemo<ReadonlySet<SidePanelId>>((previous) => {
    const seen = previous ?? new Set<SidePanelId>();
    const panel = props.panel;
    return panel === undefined || seen.has(panel) ? seen : new Set(seen).add(panel);
  });

  const [fileRequest, setFileRequest] = createSignal<FileRequest>();
  let requests = 0;
  let unsaved: string | undefined;
  const links: PanelLinks = {
    openFile(target) {
      requests += 1;
      setFileRequest({ target, seq: requests });
      props.onPanel("files");
    },
    fileRequest,
    setUnsavedWork(description) {
      unsaved = description;
    },
  };

  // A link to another session or to the home page would drop the panels and
  // what they hold. Moving between this session's own panels never does, and
  // the browser's Back cannot be told apart from that, so it is let through.
  const home = sessionPath(props.store.id);
  useBeforeLeave((leave) => {
    if (unsaved === undefined || leave.defaultPrevented || typeof leave.to !== "string") return;
    const to = leave.to.split(/[?#]/)[0];
    if (to === home || to.startsWith(`${home}/`)) return;
    if (!window.confirm(`${unsaved} Leave this session and lose them?`)) leave.preventDefault();
  });

  return (
    <div ref={props.ref} class="flex min-h-0 min-w-0 flex-1 flex-col">
      <For each={sidePanels}>
        {(definition) => (
          <Show when={opened().has(definition.id)}>
            <KeptPanel
              definition={definition}
              store={props.store}
              active={props.panel === definition.id}
              links={links}
            />
          </Show>
        )}
      </For>
    </div>
  );
}

/** Where the kept panels show: whichever layout is on screen adopts their element. */
export function PanelOutlet(props: { panels: HTMLElement | undefined }) {
  return (
    <div
      class="flex min-h-0 min-w-0 flex-1 flex-col"
      ref={(slot) => {
        const panels = props.panels;
        if (panels) slot.append(panels);
      }}
    />
  );
}
