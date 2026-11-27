import {
  createContext,
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  For,
  Match,
  onSettled,
  Show,
  Switch,
  untrack,
  useContext,
} from "solid-js";
import type { Agent } from "../gen/Agent";
import type { AgentMessage } from "../gen/AgentMessage";
import type { Item } from "../gen/Item";
import type { ItemContent } from "../gen/ItemContent";
import type { ItemStatus } from "../gen/ItemStatus";
import type { PermissionRequest } from "../gen/PermissionRequest";
import type { PullRequest } from "../gen/PullRequest";
import type { Question } from "../gen/Question";
import type { SessionRepo } from "../gen/SessionRepo";
import { isLongBody, messageHeading, type Speaker } from "../lib/messages";
import { locateInRepos } from "../lib/paths";
import {
  agentFinished,
  agentLook,
  decisionSourceLabel,
  repoModeLabel,
  type Look,
  type Tone,
} from "../lib/status";
import { entryAgent, transcriptRows, type TranscriptRow } from "../lib/transcriptRows";
import { kindIcon, noteWords } from "../lib/workflow";
import type { FileTarget } from "../panels/registry";
import type { PermissionAnswers } from "../state/permissionAnswers";
import type {
  PermissionOutcome,
  QuestionOutcome,
  ToolPermission,
  TranscriptEntry,
  WorkflowNote,
} from "../state/sessionReducer";
import { Badge, ItemStatusBadge } from "./Badge";
import { Markdown } from "./Markdown";
import { PullRequestLink } from "./PullRequests";

type ToolContent = Extract<ItemContent, { kind: "tool" }>;

/** What a row may need from the session around the transcript. */
type Surroundings = {
  repos: () => readonly SessionRepo[];
  /** Absent where requests are only shown, and while a replay is still passing through old ones. */
  answers: () => PermissionAnswers | undefined;
  /** Absent where there is no Files panel to open a file in. */
  openFile: () => ((target: FileTarget) => void) | undefined;
  /** Names who sent and who received a message. */
  speaker: () => Speaker;
  /** A loop node's limit, when the server has said it. */
  loopLimit: (nodeId: string) => number | null;
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
          {/* A call held by its permission request is not running yet: it waits for the reader. */}
          <Show
            when={props.permissions.some((asked) => asked.outcome.state === "pending")}
            fallback={<ItemStatusBadge status={props.status} interrupted={props.interrupted} />}
          >
            <Badge look={permissionLook({ state: "pending" })} pulse />
          </Show>
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

/** A pull request was opened, merged or closed: one line, leading to it. */
function PullRequestRow(props: { pullRequest: PullRequest; what: "opened" | "merged" | "closed" }) {
  const around = useContext(SurroundingsContext);
  const repo = (): string | undefined =>
    around.repos().find((one) => one.id === props.pullRequest.session_repo_id)?.name;
  return (
    <p data-pull-request-row={props.what} class="px-1 text-xs wrap-break-word text-muted">
      <PullRequestLink
        pullRequest={props.pullRequest}
        repo={repo()}
        long={true}
        class="lg:min-h-0"
      />{" "}
      {props.what}
      <Show when={repo()}>
        {(name) => (
          <>
            {" "}
            for <span class="font-medium text-fg">{name()}</span>
          </>
        )}
      </Show>
    </p>
  );
}

/**
 * A message between two participants of the session tree: who to whom and what
 * kind, then its text. A long one starts folded so it does not bury the turn.
 */
function MessageRow(props: { message: AgentMessage; direction: "sent" | "received" }) {
  const around = useContext(SurroundingsContext);
  const long = () => isLongBody(props.message.body);
  const [expanded, setExpanded] = createSignal(false);
  return (
    <div
      data-message={props.message.kind}
      data-direction={props.direction}
      class="rounded-md border border-line bg-surface px-3 py-2"
    >
      <p class="text-xs font-medium text-muted">
        {messageHeading(props.message, props.direction, around.speaker())}
      </p>
      <div class={{ "max-h-28 overflow-hidden": long() && !expanded() }}>
        <Markdown text={props.message.body} />
      </div>
      <Show when={long()}>
        <button
          type="button"
          class="inline-flex min-h-11 items-center text-sm text-accent underline underline-offset-4"
          aria-expanded={expanded() ? "true" : "false"}
          onClick={() => setExpanded((open) => !open)}
        >
          {expanded() ? "Show less" : "Show the whole message"}
        </button>
      </Show>
    </div>
  );
}

/**
 * What a workflow run did: one light line, never a card, so the agent's own
 * output stays what the eye lands on. An icon and a word say what the node is.
 */
function WorkflowRow(props: { note: WorkflowNote }) {
  const around = useContext(SurroundingsContext);
  const words = () =>
    noteWords(
      props.note,
      props.note.what === "node_started" ? around.loopLimit(props.note.nodeId) : null,
    );
  const icon = (): string =>
    props.note.what === "node_started" || props.note.what === "node_finished"
      ? kindIcon(props.note.nodeKind)
      : "◆";
  return (
    <p
      data-workflow={props.note.what}
      data-tone={words().tone}
      class="flex min-w-0 items-baseline gap-1.5 px-1 text-xs text-muted"
    >
      <span aria-hidden="true" class="w-3 shrink-0 text-center">
        {icon()}
      </span>
      <span class="min-w-0 wrap-break-word">
        <span class={["font-medium", toneText[words().tone]]}>{words().text}</span>
        <Show when={words().detail}>
          {(detail) => <span class="whitespace-pre-wrap"> · {detail()}</span>}
        </Show>
      </span>
    </p>
  );
}

function questionLook(outcome: QuestionOutcome): Look {
  if (outcome.state === "pending") return { label: "Waiting for you", tone: "attention" };
  if (outcome.state === "expired") return { label: "No longer needed", tone: "neutral" };
  return { label: `Answered by ${decisionSourceLabel(outcome.by)}`, tone: "success" };
}

/** A question an agent asked the user, where it was asked, with what became of it. */
function QuestionRow(props: { question: Question; outcome: QuestionOutcome }) {
  const answer = () => (props.outcome.state === "answered" ? props.outcome.answer : undefined);
  return (
    <div
      data-question={props.outcome.state}
      class="rounded-md border border-line bg-surface px-3 py-2"
    >
      <div class="flex min-w-0 flex-wrap items-center justify-between gap-x-2 gap-y-1">
        <span class="text-xs font-medium text-muted">
          {props.question.gate ? "Workflow gate" : "Question for you"}
        </span>
        <Badge look={questionLook(props.outcome)} pulse={props.outcome.state === "pending"} />
      </div>
      <Markdown text={props.question.text} />
      <Show when={answer()}>
        {(text) => (
          <p class={["mt-1 border-l-2 border-line pl-3 text-sm", prose]}>
            <span class="text-muted">Answer: </span>
            {text()}
          </p>
        )}
      </Show>
    </div>
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
  const pullRequest = () => (props.entry.kind === "pull_request" ? props.entry : undefined);
  const error = () => (props.entry.kind === "error" ? props.entry : undefined);
  const message = () => (props.entry.kind === "message" ? props.entry : undefined);
  const question = () => (props.entry.kind === "question" ? props.entry : undefined);
  const workflow = () => (props.entry.kind === "workflow" ? props.entry : undefined);
  return (
    <Switch>
      <Match when={workflow()}>{(entry) => <WorkflowRow note={entry().note} />}</Match>
      <Match when={message()}>
        {(entry) => <MessageRow message={entry().message} direction={entry().direction} />}
      </Match>
      <Match when={question()}>
        {(entry) => <QuestionRow question={entry().question} outcome={entry().outcome} />}
      </Match>
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
      <Match when={pullRequest()}>
        {(entry) => <PullRequestRow pullRequest={entry().pullRequest} what={entry().what} />}
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
const NO_AGENTS: readonly Agent[] = [];
const NO_CALLS: Readonly<Record<string, string>> = {};
const plainSpeaker: Speaker = (_sessionId, agentId) => (agentId === null ? "you" : "another agent");

/** The id a row carries so it can be scrolled to: its item's, for an item; its key, for a workflow row. */
const itemIdOf = (entry: TranscriptEntry): string | undefined =>
  entry.kind === "item" ? entry.item.id : entry.kind === "workflow" ? entry.key : undefined;

function EntryItem(props: { entry: TranscriptEntry; revealed: string | undefined }) {
  return (
    <li
      data-item={itemIdOf(props.entry)}
      data-revealed={
        props.revealed !== undefined && props.revealed === itemIdOf(props.entry)
          ? "true"
          : undefined
      }
      class="rounded-md data-revealed:outline-2 data-revealed:outline-offset-2 data-revealed:outline-accent"
    >
      <EntryRow entry={props.entry} />
    </li>
  );
}

type SubagentRow = Extract<TranscriptRow, { kind: "subagent" }>;

const toolCallsLabel = (count: number): string =>
  count === 1 ? "1 tool call" : `${count} tool calls`;

/**
 * What a subagent did, gathered under the tool call that started it. One line
 * (role, tool calls, status) that opens onto its rows; folded once it is done.
 */
function SubagentGroup(props: {
  row: SubagentRow;
  /** What the reader chose; absent until they do. */
  chosen: boolean | undefined;
  revealed: string | undefined;
  onToggle: (open: boolean) => void;
}) {
  const open = (): boolean => props.chosen ?? !agentFinished(props.row.agent.status);
  const look = () => agentLook(props.row.agent.status);
  return (
    <div
      role="group"
      aria-label={`Subagent ${props.row.agent.role}`}
      data-subagent={props.row.agentId}
      class="ml-3 border-l-2 border-line pl-3"
    >
      <button
        type="button"
        class="flex min-h-11 w-full min-w-0 items-center gap-1.5 text-left text-sm"
        aria-expanded={open() ? "true" : "false"}
        onClick={() => props.onToggle(!open())}
      >
        <span
          aria-hidden="true"
          class={["text-muted transition-transform", { "rotate-90": open() }]}
        >
          ▸
        </span>
        <span class="min-w-0 truncate">
          <span class="font-medium">{props.row.agent.role}</span>
          <span class="text-muted"> · {toolCallsLabel(props.row.toolCalls)} · </span>
          <span class={["font-medium", toneText[look().tone]]}>{look().label}</span>
        </span>
      </button>
      <Show when={open()}>
        <Show
          when={props.row.entries.length > 0}
          fallback={<p class="pb-2 text-sm text-muted">Nothing from this subagent yet.</p>}
        >
          <ol class="flex flex-col gap-3 pb-2">
            <For each={props.row.entries} keyed={(entry) => entry.key}>
              {(entry) => <EntryItem entry={entry()} revealed={props.revealed} />}
            </For>
          </ol>
        </Show>
      </Show>
    </div>
  );
}

/** What the filter calls each agent: its role, told apart from a namesake by its position. */
function agentOptions(agents: readonly Agent[]): { id: string; label: string }[] {
  const seen = new Map<string, number>();
  return agents.map((agent) => {
    const count = (seen.get(agent.role) ?? 0) + 1;
    seen.set(agent.role, count);
    const name = count === 1 ? agent.role : `${agent.role} ${count}`;
    return { id: agent.id, label: agent.kind === "subagent" ? `${name} (subagent)` : name };
  });
}

/** A request to bring one item into view; `seq` tells apart two requests for the same item. */
export type RevealRequest = { itemId: string; seq: number };

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
  /** The session's agents: a subagent's rows are gathered under the call that started it. */
  agents?: readonly Agent[];
  /** Subagent id to the id of the tool call that started it (see the reducer). */
  subagentCalls?: Readonly<Record<string, string>>;
  /** Names who sent and who received a message. */
  speaker?: Speaker;
  /** The one agent whose rows show; `null` or absent for all of them. */
  filter?: string | null;
  onFilter?: (agentId: string | null) => void;
  /** The item to bring into view, opening the group it is in. */
  reveal?: RevealRequest;
  /** A loop node's limit, when known, so its rows can say "attempt 2 of 3". */
  loopLimit?: (nodeId: string) => number | null;
}) {
  const agents = () => props.agents ?? NO_AGENTS;
  // A filter naming an agent that is not (or no longer) in the session shows everything.
  const filter = (): string | null => {
    const wanted = props.filter ?? null;
    return wanted !== null && agents().some((agent) => agent.id === wanted) ? wanted : null;
  };
  const rows = createMemo(() =>
    transcriptRows(
      props.entries.filter(hasRow),
      agents(),
      props.subagentCalls ?? NO_CALLS,
      filter(),
    ),
  );
  const options = createMemo(() => agentOptions(agents()));
  const surroundings: Surroundings = {
    repos: () => props.repos ?? NO_REPOS,
    // A replay passes through every request ever made; only the caught-up ones can be answered.
    answers: () => (props.synced ? props.answers : undefined),
    openFile: () => props.onOpenFile,
    speaker: () => props.speaker ?? plainSpeaker,
    loopLimit: (nodeId) => props.loopLimit?.(nodeId) ?? null,
  };
  // Which subagent groups the reader opened or closed; the others follow their agent's status.
  const [groups, setGroups] = createSignal<ReadonlyMap<string, boolean>>(new Map());
  const [revealed, setRevealed] = createSignal<string>();
  const filterId = createUniqueId();
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

  createEffect(
    // Only a new request runs this: what the transcript holds is read without following it.
    () => {
      const request = props.reveal;
      if (!request) return undefined;
      const entry = untrack(() => props.entries.find((one) => itemIdOf(one) === request.itemId));
      return entry ? { itemId: request.itemId, owner: entryAgent(entry) } : undefined;
    },
    (found) => {
      if (!found) return;
      const { itemId, owner } = found;
      if (owner !== null) setGroups((all) => new Map(all).set(owner, true));
      setRevealed(itemId);
      pin(false);
      // Once the group has opened and the row exists.
      const timer = setTimeout(() => {
        const row = [...(scroller?.querySelectorAll<HTMLElement>("[data-item]") ?? [])].find(
          (one) => one.dataset.item === itemId,
        );
        row?.scrollIntoView?.({ block: "center" });
      }, 0);
      return () => clearTimeout(timer);
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
    <div class="relative flex min-h-0 flex-1 flex-col">
      <Show when={props.onFilter !== undefined && (agents().length > 1 || filter() !== null)}>
        <div class="flex shrink-0 items-center gap-2 border-b border-line bg-surface px-3 text-sm">
          <label for={filterId} class="shrink-0 text-muted">
            Show
          </label>
          <select
            id={filterId}
            aria-label="Agent filter"
            class="field h-11 min-w-0 flex-1 border-transparent bg-transparent px-1 lg:max-w-xs"
            value={filter() ?? ""}
            onChange={(event) =>
              props.onFilter?.(event.currentTarget.value === "" ? null : event.currentTarget.value)
            }
          >
            <option value="">All agents</option>
            <For each={options()} keyed={(option) => option.id}>
              {(option) => <option value={option().id}>{option().label}</option>}
            </For>
          </select>
        </div>
      </Show>
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
                  {!props.synced
                    ? "Loading transcript…"
                    : filter() !== null
                      ? "Nothing from this agent yet."
                      : "Nothing has happened in this session yet."}
                </p>
              }
            >
              <SurroundingsContext value={surroundings}>
                <ol class="mx-auto flex max-w-3xl flex-col gap-3">
                  <For each={rows()} keyed={(row) => row.key}>
                    {(row) => (
                      <Show
                        when={subagentRow(row())}
                        fallback={<EntryItem entry={entryOf(row())} revealed={revealed()} />}
                      >
                        {(group) => (
                          <li>
                            <SubagentGroup
                              row={group()}
                              chosen={groups().get(group().agentId)}
                              revealed={revealed()}
                              onToggle={(open) =>
                                setGroups((all) => new Map(all).set(group().agentId, open))
                              }
                            />
                          </li>
                        )}
                      </Show>
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
    </div>
  );
}

const subagentRow = (row: TranscriptRow): SubagentRow | undefined =>
  row.kind === "subagent" ? row : undefined;

/** The entry of a plain row; a group never reaches here (see the `Show` above). */
function entryOf(row: TranscriptRow): TranscriptEntry {
  if (row.kind !== "entry") throw new Error("a subagent group is not an entry");
  return row.entry;
}
