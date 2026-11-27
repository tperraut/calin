import { createEffect, createSignal, For, Match, onSettled, Show, Switch } from "solid-js";
import type { Item } from "../gen/Item";
import type { ItemContent } from "../gen/ItemContent";
import type { ItemStatus } from "../gen/ItemStatus";
import type { PermissionRequest } from "../gen/PermissionRequest";
import { decisionSourceLabel, type Look } from "../lib/status";
import type { PermissionOutcome, TranscriptEntry } from "../state/sessionReducer";
import { Badge, ItemStatusBadge } from "./Badge";

type ToolContent = Extract<ItemContent, { kind: "tool" }>;

/** How close to the end still counts as "at the bottom", in pixels. */
const PIN_SLACK = 48;

const prose = "whitespace-pre-wrap wrap-break-word";

function Chevron() {
  return (
    <span aria-hidden="true" class="text-muted transition-transform group-open:rotate-90">
      ▸
    </span>
  );
}

function UserMessage(props: { text: string }) {
  return (
    <div class="rounded-md border border-line bg-accent-soft px-3 py-2">
      <p class="text-xs font-medium text-muted">You</p>
      <div class={prose}>{props.text}</div>
    </div>
  );
}

function AgentText(props: { text: string; status: ItemStatus; interrupted: boolean }) {
  return (
    <div class={prose}>
      {props.text}
      <Show when={props.status === "running" && !props.interrupted}>
        <span aria-hidden="true" class="pulse text-muted">
          ▍
        </span>
      </Show>
      <Show when={props.status === "failed" || props.interrupted}>
        <span class="ml-1 text-xs text-tone-danger">(interrupted)</span>
      </Show>
    </div>
  );
}

function Reasoning(props: { text: string; status: ItemStatus; interrupted: boolean }) {
  return (
    <details class="group text-sm text-muted">
      <summary class="inline-flex min-h-11 items-center gap-1.5">
        <Chevron />
        {props.status === "running" && !props.interrupted ? "Reasoning…" : "Reasoning"}
      </summary>
      <div class={["border-l-2 border-line pb-1 pl-3", prose]}>{props.text}</div>
    </details>
  );
}

function ToolCard(props: { content: ToolContent; status: ItemStatus; interrupted: boolean }) {
  // Input and output are built only once the card is open: a long transcript
  // holds hundreds of these and most are never looked at.
  const [open, setOpen] = createSignal(false);
  return (
    <details
      class="group rounded-md border border-line bg-surface"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary class="flex min-h-11 items-center gap-2 px-3 py-1.5">
        <Chevron />
        <span class="shrink-0 rounded border border-line px-1.5 py-0.5 font-mono text-xs text-muted">
          {props.content.tool_kind}
        </span>
        <span class="min-w-0 flex-1 truncate font-mono text-sm">{props.content.title}</span>
        <ItemStatusBadge status={props.status} interrupted={props.interrupted} />
      </summary>
      <Show when={open()}>
        <div class="space-y-2 border-t border-line p-3">
          <section>
            <h3 class="text-xs font-medium text-muted">
              Input · <span class="font-mono">{props.content.name}</span>
            </h3>
            <pre
              tabindex="0"
              aria-label="Tool input"
              class="mt-1 max-h-80 overflow-auto rounded bg-sunken p-2 font-mono text-xs"
            >
              {JSON.stringify(props.content.input, null, 2)}
            </pre>
          </section>
          <Show when={props.content.output !== null}>
            <section>
              <h3 class="text-xs font-medium text-muted">Output</h3>
              <pre
                tabindex="0"
                aria-label="Tool output"
                class="mt-1 max-h-80 overflow-auto rounded bg-sunken p-2 font-mono text-xs"
              >
                {props.content.output}
              </pre>
            </section>
          </Show>
        </div>
      </Show>
    </details>
  );
}

function ErrorRow(props: { message: string }) {
  return (
    <div class="flex gap-2 rounded-md border border-tone-danger bg-danger-soft px-3 py-2">
      <strong class="shrink-0 font-medium text-tone-danger">Error</strong>
      <span class={["min-w-0", prose]}>{props.message}</span>
    </div>
  );
}

function permissionLook(outcome: PermissionOutcome): Look {
  if (outcome.state === "pending") return { label: "Waiting for you", tone: "attention" };
  if (outcome.state === "lapsed") return { label: "No longer needed", tone: "neutral" };
  const by = decisionSourceLabel(outcome.by);
  return outcome.decision === "allow"
    ? { label: `Allowed by ${by}`, tone: "success" }
    : { label: `Denied by ${by}`, tone: "danger" };
}

/** A permission request where it was asked, with what became of it. The pinned bar answers it. */
function PermissionRow(props: { request: PermissionRequest; outcome: PermissionOutcome }) {
  // What the agent was told with a denial, or why the request was closed unanswered.
  const note = () =>
    props.outcome.state === "lapsed" ||
    (props.outcome.state === "resolved" && props.outcome.decision === "deny")
      ? props.outcome.message
      : null;
  return (
    <div class="rounded-md border border-line bg-surface px-3 py-2">
      <div class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        <span class="text-xs font-medium text-muted">Permission</span>
        <span class="shrink-0 rounded border border-line px-1.5 py-0.5 font-mono text-xs text-muted">
          {props.request.tool_kind}
        </span>
        <span class="min-w-0 flex-1 basis-40 truncate font-mono text-sm">
          {props.request.title}
        </span>
        <Badge look={permissionLook(props.outcome)} pulse={props.outcome.state === "pending"} />
      </div>
      <Show when={note()}>
        {(message) => (
          <p class={["mt-1 border-l-2 border-line pl-3 text-sm text-muted", prose]}>{message()}</p>
        )}
      </Show>
    </div>
  );
}

function ItemRow(props: { item: Item; interrupted: boolean }) {
  const text = () => (props.item.content.kind === "text" ? props.item.content : undefined);
  const reasoning = () =>
    props.item.content.kind === "reasoning" ? props.item.content : undefined;
  const tool = () => (props.item.content.kind === "tool" ? props.item.content : undefined);
  return (
    // No fallback: an item kind this build does not know leaves an empty row.
    <Switch>
      <Match when={text()}>
        {(content) => (
          <AgentText
            text={content().text}
            status={props.item.status}
            interrupted={props.interrupted}
          />
        )}
      </Match>
      <Match when={reasoning()}>
        {(content) => (
          <Reasoning
            text={content().text}
            status={props.item.status}
            interrupted={props.interrupted}
          />
        )}
      </Match>
      <Match when={tool()}>
        {(content) => (
          <ToolCard
            content={content()}
            status={props.item.status}
            interrupted={props.interrupted}
          />
        )}
      </Match>
    </Switch>
  );
}

function EntryRow(props: { entry: TranscriptEntry }) {
  const user = () => (props.entry.kind === "user" ? props.entry : undefined);
  const item = () => (props.entry.kind === "item" ? props.entry : undefined);
  const permission = () => (props.entry.kind === "permission" ? props.entry : undefined);
  const error = () => (props.entry.kind === "error" ? props.entry : undefined);
  return (
    <Switch>
      <Match when={user()}>{(entry) => <UserMessage text={entry().text} />}</Match>
      <Match when={item()}>
        {(entry) => <ItemRow item={entry().item} interrupted={entry().interrupted} />}
      </Match>
      <Match when={permission()}>
        {(entry) => <PermissionRow request={entry().request} outcome={entry().outcome} />}
      </Match>
      <Match when={error()}>{(entry) => <ErrorRow message={entry().message} />}</Match>
    </Switch>
  );
}

/**
 * The scrolling transcript. It follows new content while the reader is at the
 * bottom; once they scroll up it stays put and offers "Jump to latest".
 */
export function Transcript(props: { entries: readonly TranscriptEntry[]; synced: boolean }) {
  let scroller: HTMLDivElement | undefined;
  let content: HTMLDivElement | undefined;
  const [pinned, setPinned] = createSignal(true);
  // Mirrors `pinned` for the effect below, which must not re-run when it flips.
  let follow = true;

  function pin(value: boolean): void {
    follow = value;
    setPinned(value);
  }

  function toBottom(): void {
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
  }

  function measure(): void {
    // Nothing to learn while the transcript is hidden behind another panel.
    if (!scroller || scroller.clientHeight === 0) return;
    pin(scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= PIN_SLACK);
  }

  createEffect(
    () => props.entries,
    () => {
      if (follow) toBottom();
    },
  );

  onSettled(() => {
    if (!scroller || !content || typeof ResizeObserver === "undefined") return;
    const box = scroller;
    const observer = new ResizeObserver((changes) => {
      for (const change of changes) {
        if (change.target === box) {
          // The viewport changed (a rotation, the on-screen keyboard): keep the end in view.
          if (follow) toBottom();
        } else {
          // The content changed height without a new entry, for example a card
          // was opened: the reader may no longer be at the end, so stop following.
          measure();
        }
      }
    });
    observer.observe(box);
    observer.observe(content);
    return () => observer.disconnect();
  });

  return (
    <div class="relative min-h-0 flex-1">
      <div
        ref={(element) => (scroller = element)}
        // Deltas would be read out one by one; the screen announces whole messages instead.
        role="log"
        aria-live="off"
        aria-label="Transcript"
        tabindex="0"
        class="h-full overflow-y-auto overscroll-contain px-3 py-3"
        onScroll={measure}
      >
        <div ref={(element) => (content = element)}>
          <Show
            when={props.entries.length > 0}
            fallback={
              <p class="py-8 text-center text-sm text-muted">
                {props.synced ? "Nothing has happened in this session yet." : "Loading transcript…"}
              </p>
            }
          >
            <ol class="mx-auto flex max-w-3xl flex-col gap-3">
              <For each={props.entries} keyed={(entry) => entry.key}>
                {(entry) => (
                  <li>
                    <EntryRow entry={entry()} />
                  </li>
                )}
              </For>
            </ol>
          </Show>
        </div>
      </div>
      <Show when={!pinned()}>
        <button
          type="button"
          class="btn absolute bottom-3 left-1/2 -translate-x-1/2 shadow-md"
          onClick={() => {
            pin(true);
            toBottom();
          }}
        >
          Jump to latest ↓
        </button>
      </Show>
    </div>
  );
}
