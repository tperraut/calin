import { createMemo, createSignal, createUniqueId, For, Show } from "solid-js";
import { announceNewest, type Announcement } from "../lib/announcement";
import { errorMessage } from "../lib/errors";
import type { OpenQuestion } from "../state/sessionReducer";
import { Markdown } from "./Markdown";
import { WaitingNote } from "./WaitingNote";

const ANNOUNCED_CHARS = 200;

const sayQuestion = (newest: OpenQuestion): string => {
  const asker = newest.question.gate ? "Workflow gate" : "The agent asks";
  return `${asker}: ${newest.question.text.slice(0, ANNOUNCED_CHARS)}`;
};

/**
 * The oldest open question an agent asked, pinned above the message box: its
 * text, the suggested answers as buttons and a field for any other answer. A
 * workflow gate (api.rs, "A gate") is asked by the run, not the agent: it gets
 * Approve and Reject, and a comment that goes with either.
 * Nothing here is optimistic: a question leaves the bar when its
 * `question_answered` event arrives, not when an answer is sent.
 */
export function QuestionBar(props: {
  /** Oldest first. Empty while the transcript is still replaying, or while a permission waits. */
  questions: readonly OpenQuestion[];
  /** Rejects with the server's reason when the answer is refused. `comment` is a gate's note. */
  onAnswer: (questionId: string, answer: string, comment?: string) => Promise<void>;
  /**
   * One line until the reader opens it: on a phone, under a panel that needs
   * the room. The question is still announced, and still one tap away.
   */
  compact?: boolean;
}) {
  // What the reader chose for the compact bar; closed again for the next question.
  const [opened, setOpened] = createSignal<string>();
  const [text, setText] = createSignal("");
  const [comment, setComment] = createSignal("");
  const [sending, setSending] = createSignal<string>();
  // Answers the server accepted whose event has not arrived yet: answering twice would be a 409.
  const [sent, setSent] = createSignal<ReadonlySet<string>>(new Set());
  const [failure, setFailure] = createSignal<{ id: string; message: string }>();
  const fieldId = createUniqueId();
  const commentId = createUniqueId();
  // Signals show a tick later; two taps in one tick must not send two answers.
  let busy = false;

  const current = (): OpenQuestion | undefined => props.questions.at(0);
  const currentId = (): string | undefined => current()?.question.id;
  const waiting = (): boolean => {
    const id = currentId();
    return id !== undefined && (sending() === id || sent().has(id));
  };
  const folded = (): boolean => props.compact === true && opened() !== currentId();
  const error = (): string | undefined => {
    const failed = failure();
    return failed !== undefined && failed.id === currentId() ? failed.message : undefined;
  };

  // Each question is read out once, when it arrives.
  const announcement = createMemo<Announcement>((previous) =>
    announceNewest(previous, props.questions, sayQuestion),
  );

  async function answer(value: string, note?: string): Promise<void> {
    const id = currentId();
    const trimmed = value.trim();
    if (id === undefined || trimmed === "" || busy || waiting()) return;
    busy = true;
    setSending(id);
    setFailure(undefined);
    try {
      const said = note?.trim() ?? "";
      await (said === "" ? props.onAnswer(id, trimmed) : props.onAnswer(id, trimmed, said));
      setSent((ids) => new Set(ids).add(id));
      setText("");
      setComment("");
    } catch (cause) {
      setFailure({ id, message: errorMessage(cause) });
    } finally {
      busy = false;
      setSending(undefined);
    }
  }

  return (
    <>
      <p class="sr-only" role="status" aria-live="polite">
        {announcement().text}
      </p>
      <Show when={current()}>
        {(open) => (
          <section
            aria-label={open().question.gate ? "Workflow gate" : "Question from the agent"}
            data-gate={open().question.gate ? "" : undefined}
            // Capped, so a long question never takes the whole of a phone screen.
            class="flex max-h-[55dvh] shrink-0 flex-col gap-2 overflow-y-auto overscroll-contain border-t border-tone-attention bg-attention-soft p-3"
          >
            <div class="flex min-w-0 items-center gap-2">
              <span class="shrink-0 text-sm font-medium">
                {open().question.gate ? "Workflow gate" : "The agent asks"}
              </span>
              <Show when={folded()}>
                <span class="min-w-0 flex-1 truncate text-sm text-muted">
                  {open().question.text}
                </span>
              </Show>
              <Show when={props.questions.length > 1 && !folded()}>
                <span class="ml-auto shrink-0 text-xs text-muted">
                  {props.questions.length - 1} more waiting
                </span>
              </Show>
              <Show when={props.compact === true}>
                <button
                  type="button"
                  class={["btn shrink-0 px-2.5", { "ml-auto": !folded() }]}
                  aria-expanded={folded() ? "false" : "true"}
                  onClick={() => setOpened(folded() ? open().question.id : undefined)}
                >
                  {folded() ? "Answer" : "Hide"}
                </button>
              </Show>
            </div>
            <Show when={!folded()}>
              <div class="text-sm">
                <Markdown text={open().question.text} />
              </div>
              <Show when={open().question.gate}>
                <form
                  class="flex flex-col gap-2"
                  onSubmit={(event) => {
                    event.preventDefault();
                  }}
                >
                  <div class="flex min-w-0 flex-col gap-1">
                    <label for={commentId} class="text-sm">
                      Comment (optional)
                    </label>
                    <textarea
                      id={commentId}
                      rows="2"
                      class="field min-h-11 resize-y px-3 py-2"
                      value={comment()}
                      disabled={waiting()}
                      onInput={(event) => setComment(event.currentTarget.value)}
                    />
                  </div>
                  <Show when={error()}>
                    {(message) => (
                      <p role="alert" class="text-sm text-tone-danger">
                        {message()}
                      </p>
                    )}
                  </Show>
                  <div class="flex flex-wrap items-center justify-end gap-2">
                    <WaitingNote
                      waiting={waiting()}
                      sending={sending() !== undefined}
                      asker="workflow"
                      class="mr-auto text-xs text-muted"
                    />
                    <button
                      type="button"
                      class="btn"
                      disabled={waiting()}
                      onClick={() => void answer("reject", comment())}
                    >
                      Reject
                    </button>
                    <button
                      type="button"
                      class="btn btn-primary"
                      disabled={waiting()}
                      onClick={() => void answer("approve", comment())}
                    >
                      Approve
                    </button>
                  </div>
                </form>
              </Show>
              <Show when={!open().question.gate && open().question.options.length > 0}>
                <div role="group" aria-label="Suggested answers" class="flex flex-wrap gap-2">
                  <For each={open().question.options}>
                    {(option) => (
                      <button
                        type="button"
                        class="btn h-auto max-w-full py-2 text-left whitespace-normal"
                        disabled={waiting()}
                        onClick={() => void answer(option)}
                      >
                        {option}
                      </button>
                    )}
                  </For>
                </div>
              </Show>
              <Show when={!open().question.gate && error()}>
                {(message) => (
                  <p role="alert" class="text-sm text-tone-danger">
                    {message()}
                  </p>
                )}
              </Show>
              <form
                hidden={open().question.gate}
                class="flex flex-col gap-2 sm:flex-row sm:items-end"
                onSubmit={(event) => {
                  event.preventDefault();
                  void answer(text());
                }}
              >
                <div class="flex min-w-0 flex-1 flex-col gap-1">
                  <label for={fieldId} class="text-sm">
                    Your answer
                  </label>
                  <input
                    id={fieldId}
                    type="text"
                    class="field h-11 px-3"
                    autocomplete="off"
                    value={text()}
                    disabled={waiting()}
                    onInput={(event) => setText(event.currentTarget.value)}
                  />
                </div>
                <div class="flex items-center justify-end gap-2">
                  <WaitingNote
                    waiting={waiting()}
                    sending={sending() !== undefined}
                    asker="agent"
                    class="mr-auto text-xs text-muted sm:mr-0"
                  />
                  <button
                    type="submit"
                    class="btn btn-primary"
                    disabled={waiting() || text().trim() === ""}
                  >
                    Send answer
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
