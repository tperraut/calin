import { createMemo, createSignal, createUniqueId, Show } from "solid-js";
import type { PermissionDecision } from "../gen/PermissionDecision";
import { errorMessage } from "../lib/errors";
import type { PendingPermission } from "../state/sessionReducer";

type Announcement = { seq: number; text: string };

/**
 * The oldest unanswered permission request, pinned above the message box, with
 * Allow and Deny. Nothing here is optimistic: a request leaves the bar when its
 * `permission_resolved` event arrives, not when a button is pressed.
 */
export function PermissionBar(props: {
  /** Oldest first. Empty while the transcript is still replaying. */
  pending: readonly PendingPermission[];
  onResolve: (requestId: string, decision: PermissionDecision, message?: string) => Promise<void>;
}) {
  // The request the reader stepped to; the oldest one until they do, and again once it is answered.
  const [picked, setPicked] = createSignal<string>();
  const [denying, setDenying] = createSignal<string>();
  const [reason, setReason] = createSignal("");
  const [inFlight, setInFlight] = createSignal<string>();
  // Answers the server accepted whose event has not arrived yet: asking twice would be a 409.
  const [sent, setSent] = createSignal<ReadonlySet<string>>(new Set());
  const [failure, setFailure] = createSignal<{ requestId: string; message: string }>();
  const reasonId = createUniqueId();
  let reasonField: HTMLInputElement | undefined;

  const index = (): number => {
    const at = props.pending.findIndex((one) => one.request.id === picked());
    return at === -1 ? 0 : at;
  };
  const current = (): PendingPermission | undefined => props.pending.at(index());
  const currentId = (): string | undefined => current()?.request.id;
  const waiting = (): boolean => {
    const id = currentId();
    return id !== undefined && (inFlight() === id || sent().has(id));
  };
  const error = (): string | undefined => {
    const failed = failure();
    return failed && failed.requestId === currentId() ? failed.message : undefined;
  };

  // Each request is read out once, when it arrives: the text only changes for a newer one.
  const announcement = createMemo<Announcement>((previous) => {
    const last = previous ?? { seq: 0, text: "" };
    const newest = props.pending.at(-1);
    if (!newest) return last.text === "" ? last : { seq: last.seq, text: "" };
    if (newest.seq <= last.seq) return last;
    const others = props.pending.length - 1;
    const more = others === 0 ? "" : ` ${others} more waiting.`;
    return {
      seq: newest.seq,
      text: `Permission requested: ${newest.request.tool_kind}, ${newest.request.title}.${more}`,
    };
  });

  function step(by: number): void {
    const next = props.pending.at(index() + by);
    if (!next) return;
    setPicked(next.request.id);
    setDenying(undefined);
  }

  async function resolve(decision: PermissionDecision): Promise<void> {
    const requestId = currentId();
    if (requestId === undefined || waiting()) return;
    const message = decision === "deny" ? reason().trim() : "";
    setInFlight(requestId);
    setFailure(undefined);
    try {
      await props.onResolve(requestId, decision, message === "" ? undefined : message);
      setSent((ids) => new Set(ids).add(requestId));
      setDenying(undefined);
      setReason("");
    } catch (cause) {
      setFailure({ requestId, message: errorMessage(cause) });
    } finally {
      setInFlight(undefined);
    }
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
                <span class="ml-auto flex shrink-0 items-center gap-1 text-xs">
                  <button
                    type="button"
                    class="btn px-0"
                    aria-label="Previous request"
                    disabled={index() === 0}
                    onClick={() => step(-1)}
                  >
                    <span aria-hidden="true">‹</span>
                  </button>
                  <span>
                    {index() + 1} of {props.pending.length}
                  </span>
                  <button
                    type="button"
                    class="btn px-0"
                    aria-label="Next request"
                    disabled={index() === props.pending.length - 1}
                    onClick={() => step(1)}
                  >
                    <span aria-hidden="true">›</span>
                  </button>
                </span>
              </Show>
            </div>
            <p class="line-clamp-3 font-mono text-sm wrap-break-word">{pending().request.title}</p>
            <details class="group text-sm">
              <summary class="inline-flex min-h-11 items-center gap-1.5 text-muted">
                <span aria-hidden="true" class="transition-transform group-open:rotate-90">
                  ▸
                </span>
                Input · <span class="font-mono">{pending().request.tool_name}</span>
              </summary>
              <pre
                tabindex="0"
                aria-label="Requested tool input"
                class="max-h-48 overflow-auto rounded bg-surface p-2 font-mono text-xs"
              >
                {JSON.stringify(pending().request.input, null, 2)}
              </pre>
            </details>
            <Show when={error()}>
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
                  <Show when={waiting()}>
                    <span class="mr-auto text-xs text-muted">
                      {inFlight() === undefined ? "Sent, waiting for the agent…" : "Sending…"}
                    </span>
                  </Show>
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
              <form
                class="flex flex-col gap-2"
                onSubmit={(event) => {
                  event.preventDefault();
                  void resolve("deny");
                }}
              >
                <label for={reasonId} class="text-sm">
                  Reason for the agent <span class="text-muted">(optional)</span>
                </label>
                <input
                  id={reasonId}
                  ref={(element) => (reasonField = element)}
                  type="text"
                  class="field h-11 px-3"
                  autocomplete="off"
                  value={reason()}
                  disabled={waiting()}
                  onInput={(event) => setReason(event.currentTarget.value)}
                />
                <div class="flex items-center justify-end gap-2">
                  <button
                    type="button"
                    class="btn"
                    disabled={waiting()}
                    onClick={() => setDenying(undefined)}
                  >
                    Back
                  </button>
                  <button type="submit" class="btn btn-primary" disabled={waiting()}>
                    {waiting() ? "Sending…" : "Confirm deny"}
                  </button>
                </div>
              </form>
            </Show>
          </section>
        )}
      </Show>
    </>
  );
}
