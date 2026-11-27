import {
  createContext,
  createEffect,
  createMemo,
  createSignal,
  For,
  Match,
  onSettled,
  Show,
  Switch,
  useContext,
} from "solid-js";
import type { Item } from "../gen/Item";
import type { ItemContent } from "../gen/ItemContent";
import type { ItemStatus } from "../gen/ItemStatus";
import type { PermissionRequest } from "../gen/PermissionRequest";
import type { SessionRepo } from "../gen/SessionRepo";
import { locateInRepos } from "../lib/paths";
import { decisionSourceLabel, repoModeLabel, type Look, type Tone } from "../lib/status";
import type { FileTarget } from "../panels/registry";
import type { PermissionAnswers } from "../state/permissionAnswers";
import type { PermissionOutcome, ToolPermission, TranscriptEntry } from "../state/sessionReducer";
import { Badge, ItemStatusBadge } from "./Badge";
import { Markdown } from "./Markdown";

type ToolContent = Extract<ItemContent, { kind: "tool" }>;

/** What a row may need from the session around the transcript. */
type Surroundings = {
  repos: () => readonly SessionRepo[];
  /** Absent where requests are only shown, and while a replay is still passing through old ones. */
  answers: () => PermissionAnswers | undefined;
  /** Absent where there is no Files panel to open a file in. */
  openFile: () => ((target: FileTarget) => void) | undefined;
};

const SurroundingsContext = createContext<Surroundings>();

const NO_PERMISSIONS: readonly ToolPermission[] = [];

const toneText: Record<Tone, string> = {
  neutral: "text-muted",
  active: "text-tone-active",
  attention: "text-tone-attention",
  success: "text-tone-success",
  danger: "text-tone-danger",
};

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
    <div>
      {/* The cursor of a running message is drawn by the stylesheet, after its last block. */}
      <Markdown text={props.text} streaming={props.status === "running" && !props.interrupted} />
      <Show when={props.status === "failed" || props.interrupted}>
        <span class="text-xs text-tone-danger">(interrupted)</span>
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

function permissionLook(outcome: PermissionOutcome): Look {
  if (outcome.state === "pending") return { label: "Waiting for you", tone: "attention" };
  if (outcome.state === "lapsed") return { label: "No longer needed", tone: "neutral" };
  const by = decisionSourceLabel(outcome.by);
  return outcome.decision === "allow"
    ? { label: `Allowed by ${by}`, tone: "success" }
    : { label: `Denied by ${by}`, tone: "danger" };
}

/** What the agent was told with a denial, or why the request was closed unanswered. */
const permissionNote = (outcome: PermissionOutcome): string | null =>
  outcome.state === "lapsed" || (outcome.state === "resolved" && outcome.decision === "deny")
    ? outcome.message
    : null;

/**
 * A tool call's permission request, inside the call's card. While it waits it
 * asks, with the same two answers as the pinned bar (a denial with a reason is
 * given there); once answered it is one line saying what became of it.
 */
function ToolPermissionLine(props: { asked: ToolPermission }) {
  const around = useContext(SurroundingsContext);
  const id = () => props.asked.request.id;
  const waiting = (): boolean => around.answers()?.waiting(id()) === true;
  const look = () => permissionLook(props.asked.outcome);
  // Never empty: a button named just "Allow" or "Deny" is the pinned bar's.
  const what = (): string => props.asked.request.title.trim() || "this call";
  return (
    <Show
      when={props.asked.outcome.state === "pending"}
      fallback={
        <p
          data-permission={props.asked.outcome.state}
          class="border-t border-line px-3 py-1.5 text-xs wrap-break-word"
        >
          <span class={["font-medium", toneText[look().tone]]}>{look().label}</span>
          <Show when={permissionNote(props.asked.outcome)}>
            {(note) => <span class="text-muted">: {note()}</span>}
          </Show>
        </p>
      }
    >
      <div
        role="group"
        aria-label={`Permission for ${props.asked.request.title}`}
        data-permission="pending"
        class="flex flex-wrap items-center gap-2 rounded-b-md border-t border-tone-attention bg-attention-soft px-3 py-2"
      >
        <span class="text-sm font-medium">Allow this?</span>
        <Show when={around.answers()?.error(id())}>
          {(message) => (
            <p role="alert" class="basis-full text-sm text-tone-danger">
              {message()}
            </p>
          )}
        </Show>
        <Show when={around.answers()}>
          {(answers) => (
            <span class="ml-auto flex flex-wrap items-center justify-end gap-2">
              <Show when={waiting()}>
                <span class="text-xs text-muted">
                  {answers().sending(id()) ? "Sending…" : "Sent, waiting for the agent…"}
                </span>
              </Show>
              {/* Named after the call, so each button says what it answers; the bar's are the plain ones. */}
              <button
                type="button"
                class="btn"
                aria-label={`Deny ${what()}`}
                disabled={waiting()}
                onClick={() => void answers().answer(id(), "deny")}
              >
                Deny
              </button>
              <button
                type="button"
                class="btn btn-primary"
                aria-label={`Allow ${what()}`}
                disabled={waiting()}
                onClick={() => void answers().answer(id(), "allow")}
              >
                Allow
              </button>
            </span>
          )}
        </Show>
      </div>
    </Show>
  );
}

/** The files a tool call names, each under the session repo it is in. */
function ToolLocations(props: { locations: readonly string[] }) {
  const around = useContext(SurroundingsContext);
  const places = createMemo(() =>
    [...new Set(props.locations)].map((absolute) => ({
      absolute,
      found: locateInRepos(absolute, around.repos()),
    })),
  );
  return (
    <section>
      <h3 class="text-xs font-medium text-muted">Files</h3>
      <ul aria-label="Files of this tool call" class="mt-1">
        <For each={places()} keyed={(place) => place.absolute}>
          {(place) => (
            <li class="font-mono text-sm wrap-anywhere">
              <Show
                when={place().found}
                // Outside every repo of the session: there is nothing to open it in.
                fallback={<span class="text-muted">{place().absolute}</span>}
              >
                {(found) => (
                  <Show
                    when={found().path !== "" && around.openFile()}
                    fallback={
                      <span>
                        <span class="text-muted">{found().repo.name}/</span>
                        {found().path}
                      </span>
                    }
                  >
                    {(openFile) => (
                      <button
                        type="button"
                        class="flex min-h-11 w-full items-center text-left underline decoration-line underline-offset-4 hover:decoration-current"
                        title={`Open ${found().repo.name}/${found().path} in Files`}
                        onClick={() =>
                          openFile()({ sessionRepoId: found().repo.id, path: found().path })
                        }
                      >
                        <span>
                          <span class="text-muted">{found().repo.name}/</span>
                          {found().path}
                        </span>
                      </button>
                    )}
                  </Show>
                )}
              </Show>
            </li>
          )}
        </For>
      </ul>
    </section>
  );
}

function ToolCard(props: {
  content: ToolContent;
  status: ItemStatus;
  interrupted: boolean;
  permissions: readonly ToolPermission[];
}) {
  // Input and output are built only once the card is open: a long transcript
  // holds hundreds of these and most are never looked at.
  const [open, setOpen] = createSignal(false);
  return (
    <div class="rounded-md border border-line bg-surface">
      <details class="group" onToggle={(event) => setOpen(event.currentTarget.open)}>
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
            <Show when={props.content.locations.length > 0}>
              <ToolLocations locations={props.content.locations} />
            </Show>
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
      {/* Outside the fold: a question, or what was answered, shows on the closed card too. */}
      <For each={props.permissions} keyed={(asked) => asked.request.id}>
        {(asked) => <ToolPermissionLine asked={asked()} />}
      </For>
    </div>
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

/**
 * A permission request that names no tool call, where it was asked, with what
 * became of it. The pinned bar answers it.
 */
function PermissionRow(props: { request: PermissionRequest; outcome: PermissionOutcome }) {
  const note = () => permissionNote(props.outcome);
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

/** A repo that joined the session while it was under way. */
function RepoAttachedRow(props: { repo: SessionRepo; by: "user" | "agent" }) {
  return (
    <p class="px-1 text-xs wrap-break-word text-muted" title={props.repo.path}>
      Repo <span class="font-medium text-fg">{props.repo.name}</span> attached by{" "}
      {props.by === "user" ? "you" : "the agent"} · {repoModeLabel(props.repo.mode)}
    </p>
  );
}

function ItemRow(props: {
  item: Item;
  interrupted: boolean;
  permissions: readonly ToolPermission[];
}) {
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
            permissions={props.permissions}
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
  const repo = () => (props.entry.kind === "repo" ? props.entry : undefined);
  const error = () => (props.entry.kind === "error" ? props.entry : undefined);
  return (
    <Switch>
      <Match when={user()}>{(entry) => <UserMessage text={entry().text} />}</Match>
      <Match when={item()}>
        {(entry) => (
          <ItemRow
            item={entry().item}
            interrupted={entry().interrupted}
            permissions={entry().permissions ?? NO_PERMISSIONS}
          />
        )}
      </Match>
      <Match when={permission()}>
        {(entry) => <PermissionRow request={entry().request} outcome={entry().outcome} />}
      </Match>
      <Match when={repo()}>
        {(entry) => <RepoAttachedRow repo={entry().repo} by={entry().by} />}
      </Match>
      <Match when={error()}>{(entry) => <ErrorRow message={entry().message} />}</Match>
    </Switch>
  );
}

/** A reasoning item with nothing in it (runners announce them, some never fill them) is not a row. */
const hasRow = (entry: TranscriptEntry): boolean =>
  entry.kind !== "item" ||
  entry.item.content.kind !== "reasoning" ||
  entry.item.content.text.trim() !== "";

const NO_REPOS: readonly SessionRepo[] = [];

/**
 * The scrolling transcript. It follows new content while the reader is at the
 * bottom; once they scroll up it stays put and offers "Jump to latest".
 */
export function Transcript(props: {
  entries: readonly TranscriptEntry[];
  synced: boolean;
  /** The session's repos: a tool call's files are shown relative to the one they are in. */
  repos?: readonly SessionRepo[];
  /** Lets a tool call's card answer its own permission request, as the pinned bar does. */
  answers?: PermissionAnswers;
  /** Opens a file of one of the session's repos in the Files panel. */
  onOpenFile?: (target: FileTarget) => void;
}) {
  const rows = createMemo(() => props.entries.filter(hasRow));
  const surroundings: Surroundings = {
    repos: () => props.repos ?? NO_REPOS,
    // A replay passes through every request ever made; only the caught-up ones can be answered.
    answers: () => (props.synced ? props.answers : undefined),
    openFile: () => props.onOpenFile,
  };
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
            when={rows().length > 0}
            fallback={
              <p class="py-8 text-center text-sm text-muted">
                {props.synced ? "Nothing has happened in this session yet." : "Loading transcript…"}
              </p>
            }
          >
            <SurroundingsContext value={surroundings}>
              <ol class="mx-auto flex max-w-3xl flex-col gap-3">
                <For each={rows()} keyed={(entry) => entry.key}>
                  {(entry) => (
                    <li>
                      <EntryRow entry={entry()} />
                    </li>
                  )}
                </For>
              </ol>
            </SurroundingsContext>
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
