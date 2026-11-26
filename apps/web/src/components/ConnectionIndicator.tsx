import { Show, useContext } from "solid-js";
import { AppContext } from "../state/app";

/** Says so when live updates are not flowing. Shows nothing while the socket is open. */
export function ConnectionIndicator() {
  const app = useContext(AppContext);
  const label = (): string => {
    const state = app.connection();
    if (state === "open") return "";
    return state === "connecting" ? "Connecting…" : "Reconnecting…";
  };
  return (
    <div class="pointer-events-none fixed inset-x-0 top-[calc(env(safe-area-inset-top)+0.5rem)] z-20 flex justify-center">
      {/* Always in the page, so a screen reader hears the text arrive at once;
          the visible pill waits a moment so a quick connect does not flash. */}
      <p role="status" aria-live="polite" class="sr-only">
        {label()}
      </p>
      <Show when={label() !== ""}>
        <p
          aria-hidden="true"
          class="appear-late rounded-full border border-tone-attention bg-attention-soft px-3 py-1 text-xs font-medium text-fg shadow-sm"
        >
          {label()}
        </p>
      </Show>
    </div>
  );
}
