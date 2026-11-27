import { createMemo, createUniqueId, Show } from "solid-js";
import { shipPlan, type ShipPlan } from "../lib/delivery";
import { canShip } from "../lib/status";
import { sessionPath } from "../panels/registry";
import type { SessionStore } from "../state/sessionStore";

/** What shipping would do now, from the changes list the session's store holds. */
export const currentShipPlan = (store: SessionStore): ShipPlan =>
  shipPlan(store.repos(), store.changes.list() ?? [], store.delivery.pullRequests());

/** True while the session can be shipped and has something to ship (api.rs, "Shipping"). */
export const shipOffered = (store: SessionStore, plan: ShipPlan): boolean =>
  canShip(store.status()) && plan.steps.length > 0;

/**
 * The way to the ship form, with what stands in its way or went wrong last
 * time. It changes nothing by itself: while a ship is under way it only says
 * so, until the session's events have told what became of each repo.
 */
export function ShipAction(props: {
  store: SessionStore;
  /** Whether the form is showing; absent where the button cannot tell. */
  open?: boolean;
  onOpen: () => void;
  class?: string;
}) {
  const delivery = () => props.store.delivery;
  const reasonId = createUniqueId();
  const plan = createMemo(() => currentShipPlan(props.store));
  const updating = (): boolean => delivery().pullRequests().length > 0;
  const label = (): string => {
    const shipping = delivery().shipping();
    if (shipping === "opening") return "Opening pull requests…";
    if (shipping === "updating") return "Updating pull requests…";
    return updating() ? "Update pull requests" : "Open pull requests";
  };
  const failed = (): string => {
    const count = delivery().failed();
    return count === 1 ? "1 repo failed" : `${count} repos failed`;
  };

  return (
    <Show when={shipOffered(props.store, plan())}>
      <div
        role="group"
        aria-label="Delivery"
        class={["flex max-w-full min-w-0 flex-wrap items-center gap-x-2 gap-y-1", props.class]}
      >
        <button
          type="button"
          // Review ends here; once pull requests are open, shipping again is the lesser thing to do.
          class={["btn px-2.5 text-sm", { "btn-primary": props.store.status() === "review" }]}
          aria-expanded={props.open === undefined ? undefined : props.open ? "true" : "false"}
          aria-describedby={delivery().blocked() === undefined ? undefined : reasonId}
          disabled={delivery().blocked() !== undefined || delivery().shipping() !== undefined}
          onClick={() => props.onOpen()}
        >
          {label()}
        </button>
        <Show when={delivery().blocked()}>
          {(reason) => (
            <p class="flex min-w-0 flex-1 basis-48 flex-wrap items-center gap-x-2 text-xs wrap-break-word text-fg">
              <span id={reasonId} class="min-w-0">
                <span class="font-medium text-tone-attention">GitHub is not available:</span>{" "}
                {reason()}
              </span>
              <button
                type="button"
                class="inline-flex min-h-11 items-center font-medium text-accent underline underline-offset-4 lg:min-h-0"
                onClick={() => delivery().recheck()}
              >
                Check GitHub again
              </button>
            </p>
          )}
        </Show>
        <Show when={delivery().failed() > 0}>
          <p data-part="ship-failures" class="text-xs text-tone-danger">
            <span class="font-medium">{failed()} in the last attempt.</span>{" "}
            <a
              href={sessionPath(props.store.id)}
              class="inline-flex min-h-11 items-center text-accent underline underline-offset-4 lg:min-h-0"
            >
              See why in the transcript
            </a>
          </p>
        </Show>
      </div>
    </Show>
  );
}
