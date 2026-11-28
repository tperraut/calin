import { createMemo, createSignal, createUniqueId, Show } from "solid-js";
import type { PermissionDecision } from "../gen/PermissionDecision";
import { announceNewest, type Announcement } from "../lib/announcement";
import { createPermissionAnswers, type PermissionAnswers } from "../state/permissionAnswers";
import type { PendingPermission } from "../state/sessionReducer";
import { WaitingNote } from "./WaitingNote";

/** Where the request the reader picked is in the list; the oldest when it is not there (any more). */
function pickedIndex(pending: readonly PendingPermission[], picked: string | undefined): number {
  return Math.max(
    0,
    pending.findIndex((one) => one.request.id === picked),
  );
}

/** What the answers say of one request; nothing while there is none. */
function answerState(answers: PermissionAnswers, id: string | undefined) {
  if (id === undefined) return { waiting: false, sending: false, error: undefined };
  return { waiting: answers.waiting(id), sending: answers.sending(id), error: answers.error(id) };
}

/** What goes to the agent with an answer: a denial's reason, when one was typed. */
function answerMessage(decision: PermissionDecision, reason: string): string | undefined {
  const message = decision === "deny" ? reason.trim() : "";
  return message === "" ? undefined : message;
}

const sayRequest = (newest: PendingPermission, others: number): string => {
  const more = others === 0 ? "" : ` ${others} more waiting.`;
  return `Permission requested: ${newest.request.tool_kind}, ${newest.request.title}.${more}`;
};

/**
 * The oldest unanswered permission request, pinned above the message box, with
 * Allow and Deny. Nothing here is optimistic: a request leaves the bar when its
 * `permission_resolved` event arrives, not when a button is pressed.
 */
export function PermissionBar(props: {
  /** Oldest first. Empty while the transcript is still replaying. */
  pending: readonly PendingPermission[];
  onResolve: (requestId: string, decision: PermissionDecision, message?: string) => Promise<void>;
  /** Shared with the transcript's cards when both can answer; the bar keeps its own otherwise. */
  answers?: PermissionAnswers;
}) {
  const answers =
    props.answers ??
    createPermissionAnswers((requestId, decision, message) =>
      props.onResolve(requestId, decision, message),
    );
  // The request the reader stepped to; the oldest one until they do, and again once it is answered.
  const [picked, setPicked] = createSignal<string>();
  const [denying, setDenying] = createSignal<string>();
  const [reason, setReason] = createSignal("");
  let reasonField: HTMLInputElement | undefined;

  const index = (): number => pickedIndex(props.pending, picked());
  const current = (): PendingPermission | undefined => props.pending.at(index());
  const currentId = (): string | undefined => current()?.request.id;
  const state = () => answerState(answers, currentId());
  const waiting = (): boolean => state().waiting;

  // Each request is read out once, when it arrives: the text only changes for a newer one.
  const announcement = createMemo<Announcement>((previous) =>
    announceNewest(previous, props.pending, sayRequest),
  );

  function step(by: number): void {
    const next = props.pending.at(index() + by);
    if (!next) return;
    setPicked(next.request.id);
    setDenying(undefined);
  }

  async function resolve(decision: PermissionDecision): Promise<void> {
    const requestId = currentId();
    if (requestId === undefined || waiting()) return;
    const taken = await answers.answer(requestId, decision, answerMessage(decision, reason()));
    if (!taken) return;
    setDenying(undefined);
    setReason("");
  }

  return (
    <>
      <p class="sr-only" role="status" aria-live="polite">
        {announcement().text}
      </p>
      <Show when={current()}>
        {(pending) => (
          <section
            aria-label="Permission request"
            // Capped, so an opened input or the reason field never takes the whole of a phone screen.
            class="flex max-h-[55dvh] shrink-0 flex-col gap-2 overflow-y-auto overscroll-contain border-t border-tone-attention bg-attention-soft p-3"
          >
            <div class="flex min-w-0 items-center gap-2">
              <span class="text-sm font-medium">Allow this?</span>
              <span class="shrink-0 rounded border border-line bg-surface px-1.5 py-0.5 font-mono text-xs text-muted">
                {pending().request.tool_kind}
              </span>
              <Show when={props.pending.length > 1}>
                <RequestStepper index={index()} count={props.pending.length} onStep={step} />
              </Show>
            </div>
            <RequestDetails pending={pending()} />
            <Show when={state().error}>
              {(message) => (
                <p role="alert" class="text-sm text-tone-danger">
                  {message()}
                </p>
              )}
            </Show>
            <Show
              when={denying() === pending().request.id}
              fallback={
                <div class="flex items-center justify-end gap-2">
                  <WaitingNote
                    waiting={waiting()}
                    sending={state().sending}
                    asker="agent"
                    class="mr-auto text-xs text-muted"
                  />
                  <button
                    type="button"
                    class="btn"
                    disabled={waiting()}
                    onClick={() => {
                      setDenying(pending().request.id);
                      queueMicrotask(() => reasonField?.focus());
                    }}
                  >
                    Deny
                  </button>
                  <button
                    type="button"
                    class="btn btn-primary"
                    disabled={waiting()}
                    onClick={() => void resolve("allow")}
                  >
                    Allow
                  </button>
                </div>
              }
            >
              <DenyForm
                reason={reason()}
                waiting={waiting()}
                field={(element) => (reasonField = element)}
                onReason={setReason}
                onBack={() => setDenying(undefined)}
                onDeny={() => void resolve("deny")}
              />
            </Show>
          </section>
        )}
      </Show>
    </>
  );
}

/** "2 of 3", between buttons to the previous and the next request. */
function RequestStepper(props: { index: number; count: number; onStep: (by: number) => void }) {
  return (
    <span class="ml-auto flex shrink-0 items-center gap-1 text-xs">
      <button
        type="button"
        class="btn px-0"
        aria-label="Previous request"
        disabled={props.index === 0}
        onClick={() => props.onStep(-1)}
      >
        <span aria-hidden="true">‹</span>
      </button>
      <span>
        {props.index + 1} of {props.count}
      </span>
      <button
        type="button"
        class="btn px-0"
        aria-label="Next request"
        disabled={props.index === props.count - 1}
        onClick={() => props.onStep(1)}
      >
        <span aria-hidden="true">›</span>
      </button>
    </span>
  );
}

/** The request's title, and the tool's input as JSON behind a fold. */
function RequestDetails(props: { pending: PendingPermission }) {
  return (
    <>
      <p class="line-clamp-3 font-mono text-sm wrap-break-word">{props.pending.request.title}</p>
      <details class="group text-sm">
        <summary class="inline-flex min-h-11 items-center gap-1.5 text-muted">
          <span aria-hidden="true" class="transition-transform group-open:rotate-90">
            ▸
          </span>
          Input · <span class="font-mono">{props.pending.request.tool_name}</span>
        </summary>
        <pre
          tabindex="0"
          aria-label="Requested tool input"
          class="max-h-48 overflow-auto rounded bg-surface p-2 font-mono text-xs"
        >
          {JSON.stringify(props.pending.request.input, null, 2)}
        </pre>
      </details>
    </>
  );
}

/** A denial, with an optional reason for the agent. */
function DenyForm(props: {
  reason: string;
  waiting: boolean;
  field: (element: HTMLInputElement) => void;
  onReason: (reason: string) => void;
  onBack: () => void;
  onDeny: () => void;
}) {
  const reasonId = createUniqueId();
  return (
    <form
      class="flex flex-col gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        props.onDeny();
      }}
    >
      <label for={reasonId} class="text-sm">
        Reason for the agent <span class="text-muted">(optional)</span>
      </label>
      <input
        id={reasonId}
        ref={props.field}
        type="text"
        class="field h-11 px-3"
        autocomplete="off"
        value={props.reason}
        disabled={props.waiting}
        onInput={(event) => props.onReason(event.currentTarget.value)}
      />
      <div class="flex items-center justify-end gap-2">
        <button type="button" class="btn" disabled={props.waiting} onClick={() => props.onBack()}>
          Back
        </button>
        <button type="submit" class="btn btn-primary" disabled={props.waiting}>
          {props.waiting ? "Sending…" : "Confirm deny"}
        </button>
      </div>
    </form>
  );
}
