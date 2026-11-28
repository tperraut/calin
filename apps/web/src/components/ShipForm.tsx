import { createSignal, createUniqueId, For, onSettled, Show } from "solid-js";
import type { ShipRequest } from "../gen/ShipRequest";
import { skippedLine, stepLine, type ShipPlan } from "../lib/delivery";
import { errorMessage } from "../lib/errors";
import { onFormSubmit, onSubmitChord } from "../lib/submit";
import type { Delivery, SessionStore } from "../state/sessionStore";

/** What the reader filled in, as a request: a blank field is left to the server's default. */
function shipRequest(
  title: string,
  description: string,
  plan: ShipPlan,
  asDrafts: boolean,
): ShipRequest {
  const request: ShipRequest = {};
  if (title.trim() !== "") request.title = title.trim();
  if (description.trim() !== "") request.body = description.trim();
  if (opensSome(plan)) request.draft = asDrafts;
  return request;
}

// A draft is something a pull request is opened as: with none to open there is nothing to choose.
const opensSome = (plan: ShipPlan): boolean =>
  plan.steps.some((step) => step.updates === undefined);

/** Whether `key` closes the form: Escape does while nothing was typed, and never throws a description away. */
const closesOnKey = (key: string, titleEdit: string | undefined, description: string): boolean =>
  key === "Escape" && titleEdit === undefined && description === "";

/** Whether shipping is out of reach: a request on its way, GitHub unusable, a ship running, or nothing to ship. */
const cannotShip = (busy: boolean, delivery: Delivery, plan: ShipPlan): boolean =>
  busy ||
  delivery.blocked() !== undefined ||
  delivery.shipping() !== undefined ||
  plan.steps.length === 0;

/**
 * Asks what the pull requests should say, shows what shipping will do repo by
 * repo, and sends `POST /ship`. Nothing is shown as opened here: the form
 * closes once the server took the request, and the pull requests appear with
 * their events. A refusal stays in the form, in the server's words.
 */
export function ShipForm(props: { store: SessionStore; plan: ShipPlan; onClose: () => void }) {
  // Until the reader types, the title is the session's (delivery.rs, `ShipRequest`).
  const [titleEdit, setTitleEdit] = createSignal<string>();
  const [description, setDescription] = createSignal("");
  const [asDrafts, setAsDrafts] = createSignal(true);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const titleId = createUniqueId();
  const descriptionId = createUniqueId();
  const errorId = createUniqueId();
  let titleField: HTMLInputElement | undefined;
  onSettled(() => titleField?.focus());

  const delivery = () => props.store.delivery;
  const sessionTitle = (): string => props.store.session()?.title ?? "";
  const title = (): string => titleEdit() ?? sessionTitle();
  const blocked = (): boolean => cannotShip(busy(), delivery(), props.plan);

  async function submit(): Promise<void> {
    if (blocked()) return;
    const request = shipRequest(title(), description(), props.plan, asDrafts());
    setBusy(true);
    setError(undefined);
    try {
      await delivery().ship(request);
      props.onClose();
    } catch (reason) {
      setError(errorMessage(reason));
      // A refusal may be GitHub having become unusable: the reason then shows with the action.
      delivery().recheck();
    } finally {
      setBusy(false);
    }
  }

  const onChord = onSubmitChord(submit);

  return (
    <form
      aria-label="Ship the session"
      class="flex max-h-[70dvh] flex-col gap-2 overflow-y-auto overscroll-contain border-b border-line bg-attention-soft p-3"
      onSubmit={onFormSubmit(submit)}
      onKeyDown={(event) => {
        if (closesOnKey(event.key, titleEdit(), description())) props.onClose();
      }}
    >
      <label for={titleId} class="flex flex-col gap-1 text-sm">
        Pull request title
        <input
          id={titleId}
          ref={(element) => (titleField = element)}
          type="text"
          class="field h-11 px-3"
          placeholder={sessionTitle()}
          value={title()}
          disabled={busy()}
          onInput={(event) => setTitleEdit(event.currentTarget.value)}
          onKeyDown={onChord}
        />
      </label>
      <label for={descriptionId} class="flex flex-col gap-1 text-sm">
        <span>
          Description <span class="text-muted">(optional, Markdown)</span>
        </span>
        <textarea
          id={descriptionId}
          class="field min-h-11 resize-y px-3 py-2"
          rows="3"
          value={description()}
          disabled={busy()}
          aria-describedby={error() === undefined ? undefined : errorId}
          onInput={(event) => setDescription(event.currentTarget.value)}
          onKeyDown={onChord}
        />
      </label>
      <Show when={opensSome(props.plan)}>
        <label class="flex min-h-11 cursor-pointer items-center gap-3 text-sm">
          <input
            type="checkbox"
            class="size-5 shrink-0 accent-accent"
            checked={asDrafts()}
            disabled={busy()}
            onChange={(event) => setAsDrafts(event.currentTarget.checked)}
          />
          Open as drafts
        </label>
      </Show>
      <PlanPreview plan={props.plan} dryRun={delivery().dryRun()} />
      <ShipProblems blocked={delivery().blocked()} error={error()} errorId={errorId} />
      <ShipActions busy={busy()} blocked={blocked()} onCancel={() => props.onClose()} />
    </form>
  );
}

function ShipActions(props: { busy: boolean; blocked: boolean; onCancel: () => void }) {
  return (
    <div class="flex justify-end gap-2">
      <button type="button" class="btn" onClick={() => props.onCancel()}>
        Cancel
      </button>
      <button
        type="submit"
        class="btn btn-primary"
        title="Ship (Ctrl+Enter)"
        aria-keyshortcuts="Control+Enter Meta+Enter"
        disabled={props.blocked}
      >
        {props.busy ? "Shipping…" : "Ship"}
      </button>
    </div>
  );
}

/** What shipping will do, repo by repo, what it leaves out, and whether it is only a dry run. */
function PlanPreview(props: { plan: ShipPlan; dryRun: boolean }) {
  return (
    <div class="text-sm">
      <ul aria-label="What will happen" class="font-mono text-xs wrap-break-word">
        <For each={props.plan.steps} keyed={(step) => step.repo.id}>
          {(step) => <li class="py-0.5">{stepLine(step())}</li>}
        </For>
      </ul>
      <Show when={skippedLine(props.plan)}>
        {(line) => (
          <p data-part="skipped" class="mt-1 text-xs wrap-break-word text-muted">
            {line()}
          </p>
        )}
      </Show>
      <Show when={props.dryRun}>
        <p data-part="dry-run" class="mt-1 text-xs font-medium text-tone-attention">
          Dry run: no pull request is really opened
        </p>
      </Show>
    </div>
  );
}

/** Why the form cannot ship: GitHub being unusable, and the server's refusal of the last try. */
function ShipProblems(props: {
  blocked: string | undefined;
  error: string | undefined;
  errorId: string;
}) {
  return (
    <>
      <Show when={props.blocked}>
        {(reason) => (
          <p class="text-sm">
            <span class="font-medium text-tone-attention">GitHub is not available:</span> {reason()}
          </p>
        )}
      </Show>
      <Show when={props.error}>
        {(message) => (
          <p id={props.errorId} role="alert" class="text-sm text-tone-danger">
            {message()}
          </p>
        )}
      </Show>
    </>
  );
}
