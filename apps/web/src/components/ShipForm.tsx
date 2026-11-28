import { createSignal, createUniqueId, For, onSettled, Show } from "solid-js";
import type { ShipRequest } from "../gen/ShipRequest";
import { skippedLine, stepLine, type ShipPlan } from "../lib/delivery";
import { errorMessage } from "../lib/errors";
import { onFormSubmit, onSubmitChord } from "../lib/submit";
import type { SessionStore } from "../state/sessionStore";

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
  // A draft is something a pull request is opened as: with none to open there is nothing to choose.
  const opensSome = (): boolean => props.plan.steps.some((step) => step.updates === undefined);
  const blocked = (): boolean =>
    busy() ||
    delivery().blocked() !== undefined ||
    delivery().shipping() !== undefined ||
    props.plan.steps.length === 0;

  async function submit(): Promise<void> {
    if (blocked()) return;
    const request: ShipRequest = {};
    if (title().trim() !== "") request.title = title().trim();
    if (description().trim() !== "") request.body = description().trim();
    if (opensSome()) request.draft = asDrafts();
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
        // Escape closes a form nothing was typed in; it never throws a description away.
        if (event.key === "Escape" && titleEdit() === undefined && description() === "") {
          props.onClose();
        }
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
      <Show when={opensSome()}>
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
        <Show when={delivery().dryRun()}>
          <p data-part="dry-run" class="mt-1 text-xs font-medium text-tone-attention">
            Dry run: no pull request is really opened
          </p>
        </Show>
      </div>
      <Show when={delivery().blocked()}>
        {(reason) => (
          <p class="text-sm">
            <span class="font-medium text-tone-attention">GitHub is not available:</span> {reason()}
          </p>
        )}
      </Show>
      <Show when={error()}>
        {(message) => (
          <p id={errorId} role="alert" class="text-sm text-tone-danger">
            {message()}
          </p>
        )}
      </Show>
      <div class="flex justify-end gap-2">
        <button type="button" class="btn" onClick={() => props.onClose()}>
          Cancel
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          title="Ship (Ctrl+Enter)"
          aria-keyshortcuts="Control+Enter Meta+Enter"
          disabled={blocked()}
        >
          {busy() ? "Shipping…" : "Ship"}
        </button>
      </div>
    </form>
  );
}
