import { Errored, For, Loading, Show } from "solid-js";
import { errorMessage } from "../lib/errors";
import { sidePanels, type PanelProps, type SidePanelId } from "./registry";

/**
 * Shows one side panel. Its code is fetched when it is first shown, so the
 * boundary here covers the wait and a failed fetch (a deploy in between, a
 * dropped connection) without taking the session screen down.
 */
export function PanelHost(props: PanelProps & { panel: SidePanelId }) {
  return (
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
      {/* `on`: switching panels shows the fallback at once instead of holding the
          navigation, tab and address included, until the next panel's code has arrived. */}
      <Loading fallback={<p class="p-3 text-sm text-muted">Loading…</p>} on={props.panel}>
        <For each={sidePanels}>
          {(definition) => (
            <Show when={props.panel === definition.id}>
              <definition.component store={props.store} />
            </Show>
          )}
        </For>
      </Loading>
    </Errored>
  );
}
