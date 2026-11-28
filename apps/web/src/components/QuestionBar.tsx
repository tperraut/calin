import { createMemo, createSignal, createUniqueId, For, Show } from "solid-js";
import { announceNewest, type Announcement } from "../lib/announcement";
import { filled } from "../lib/format";
import { answerState, createAnswers } from "../state/permissionAnswers";
import type { OpenQuestion } from "../state/sessionReducer";
import { ErrorNote } from "./ErrorNote";
import { Markdown } from "./Markdown";
import { WaitingNote } from "./WaitingNote";

const ANNOUNCED_CHARS = 200;

type OnAnswer = (questionId: string, answer: string, comment?: string) => Promise<void>;

const askerOf = (open: OpenQuestion): string =>
  open.question.gate ? "Workflow gate" : "The agent asks";

const sectionLabelOf = (open: OpenQuestion): string =>
  open.question.gate ? "Workflow gate" : "Question from the agent";

const sayQuestion = (newest: OpenQuestion): string =>
  `${askerOf(newest)}: ${newest.question.text.slice(0, ANNOUNCED_CHARS)}`;

/** Sends through `onAnswer`, passing a comment only when there is one. */
const answerWith =
  (onAnswer: OnAnswer) =>
  (id: string, answer: string, comment?: string): Promise<void> =>
    comment === undefined ? onAnswer(id, answer) : onAnswer(id, answer, comment);

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
  onAnswer: OnAnswer;
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
  const answers = createAnswers<string>((id, answer, comment) =>
    answerWith(props.onAnswer)(id, answer, comment),
  );

  const current = (): OpenQuestion | undefined => props.questions.at(0);
  const currentId = (): string | undefined => current()?.question.id;
  const state = () => answerState(answers, currentId());
  const waiting = (): boolean => state().waiting;
  const folded = (): boolean => props.compact === true && opened() !== currentId();

  // Each question is read out once, when it arrives.
  const announcement = createMemo<Announcement>((previous) =>
    announceNewest(previous, props.questions, sayQuestion),
  );

  async function answer(value: string, note?: string): Promise<void> {
    const id = currentId();
    if (id === undefined || value.trim() === "" || waiting()) return;
    if (!(await answers.answer(id, value.trim(), filled(note)))) return;
    setText("");
    setComment("");
  }

  return (
    <>
      <p class="sr-only" role="status" aria-live="polite">
        {announcement().text}
      </p>
      <Show when={current()}>
        {(open) => (
          <section
            aria-label={sectionLabelOf(open())}
            data-gate={open().question.gate ? "" : undefined}
            // Capped, so a long question never takes the whole of a phone screen.
            class="flex max-h-[55dvh] shrink-0 flex-col gap-2 overflow-y-auto overscroll-contain border-t border-tone-attention bg-attention-soft p-3"
          >
            <QuestionHeading
              open={open()}
              others={props.questions.length - 1}
              compact={props.compact === true}
              folded={folded()}
              onOpen={setOpened}
            />
            <Show when={!folded()}>
              <div class="text-sm">
                <Markdown text={open().question.text} />
              </div>
              <Show when={open().question.gate}>
                <GateForm
                  comment={comment()}
                  waiting={waiting()}
                  sending={state().sending}
                  error={state().error}
                  onComment={setComment}
                  onDecide={(decision) => void answer(decision, comment())}
                />
              </Show>
              <Show when={!open().question.gate}>
                <SuggestedAnswers
                  options={open().question.options}
                  waiting={waiting()}
                  onAnswer={(option) => void answer(option)}
                />
                <ErrorNote message={state().error} />
              </Show>
              <FreeAnswerForm
                hidden={open().question.gate}
                text={text()}
                waiting={waiting()}
                sending={state().sending}
                onText={setText}
                onSend={() => void answer(text())}
              />
            </Show>
          </section>
        )}
      </Show>
    </>
  );
}

/** Who asks, then, on a compact bar, the question folded to one line and the button that opens it. */
function QuestionHeading(props: {
  open: OpenQuestion;
  /** How many more questions wait behind this one. */
  others: number;
  compact: boolean;
  folded: boolean;
  /** The question opened, or none once the reader hides it again. */
  onOpen: (questionId: string | undefined) => void;
}) {
  return (
    <div class="flex min-w-0 items-center gap-2">
      <span class="shrink-0 text-sm font-medium">{askerOf(props.open)}</span>
      <Show when={props.folded}>
        <span class="min-w-0 flex-1 truncate text-sm text-muted">{props.open.question.text}</span>
      </Show>
      <Show when={props.others > 0 && !props.folded}>
        <span class="ml-auto shrink-0 text-xs text-muted">{props.others} more waiting</span>
      </Show>
      <Show when={props.compact}>
        <button
          type="button"
          class={["btn shrink-0 px-2.5", { "ml-auto": !props.folded }]}
          aria-expanded={props.folded ? "false" : "true"}
          onClick={() => props.onOpen(props.folded ? props.open.question.id : undefined)}
        >
          {props.folded ? "Answer" : "Hide"}
        </button>
      </Show>
    </div>
  );
}

/** A workflow gate's two decisions, with a comment that goes with either. */
function GateForm(props: {
  comment: string;
  waiting: boolean;
  sending: boolean;
  error: string | undefined;
  onComment: (comment: string) => void;
  onDecide: (decision: "approve" | "reject") => void;
}) {
  const commentId = createUniqueId();
  return (
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
          value={props.comment}
          disabled={props.waiting}
          onInput={(event) => props.onComment(event.currentTarget.value)}
        />
      </div>
      <ErrorNote message={props.error} />
      <div class="flex flex-wrap items-center justify-end gap-2">
        <WaitingNote
          waiting={props.waiting}
          sending={props.sending}
          asker="workflow"
          class="mr-auto text-xs text-muted"
        />
        <button
          type="button"
          class="btn"
          disabled={props.waiting}
          onClick={() => props.onDecide("reject")}
        >
          Reject
        </button>
        <button
          type="button"
          class="btn btn-primary"
          disabled={props.waiting}
          onClick={() => props.onDecide("approve")}
        >
          Approve
        </button>
      </div>
    </form>
  );
}

/** The answers the agent suggested, one button each; none shown when it suggested none. */
function SuggestedAnswers(props: {
  options: readonly string[];
  waiting: boolean;
  onAnswer: (option: string) => void;
}) {
  return (
    <Show when={props.options.length > 0}>
      <div role="group" aria-label="Suggested answers" class="flex flex-wrap gap-2">
        <For each={props.options}>
          {(option) => (
            <button
              type="button"
              class="btn h-auto max-w-full py-2 text-left whitespace-normal"
              disabled={props.waiting}
              onClick={() => props.onAnswer(option)}
            >
              {option}
            </button>
          )}
        </For>
      </div>
    </Show>
  );
}

/** Any other answer, typed. */
function FreeAnswerForm(props: {
  hidden: boolean;
  text: string;
  waiting: boolean;
  sending: boolean;
  onText: (text: string) => void;
  onSend: () => void;
}) {
  const fieldId = createUniqueId();
  return (
    <form
      hidden={props.hidden}
      class="flex flex-col gap-2 sm:flex-row sm:items-end"
      onSubmit={(event) => {
        event.preventDefault();
        props.onSend();
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
          value={props.text}
          disabled={props.waiting}
          onInput={(event) => props.onText(event.currentTarget.value)}
        />
      </div>
      <div class="flex items-center justify-end gap-2">
        <WaitingNote
          waiting={props.waiting}
          sending={props.sending}
          asker="agent"
          class="mr-auto text-xs text-muted sm:mr-0"
        />
        <button
          type="submit"
          class="btn btn-primary"
          disabled={props.waiting || props.text.trim() === ""}
        >
          Send answer
        </button>
      </div>
    </form>
  );
}
