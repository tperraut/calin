import {
  createSignal,
  createUniqueId,
  For,
  Match,
  onSettled,
  Show,
  Switch,
  type Accessor,
  type Setter,
} from "solid-js";
import { HttpError, type IssueQuery } from "../api/http";
import type { Issue } from "../gen/Issue";
import type { IssueState } from "../gen/IssueState";
import { errorMessage } from "../lib/errors";
import { absoluteTime, relativeTime } from "../lib/format";

/** How long typing must pause before the search is sent. */
export const ISSUE_SEARCH_DELAY_MS = 300;

type Listing =
  | { status: "loading" }
  | { status: "ready"; issues: readonly Issue[] }
  | { status: "error"; message: string }
  /** A 409: this repo's issues cannot be listed (no GitHub remote, or `gh` unusable). */
  | { status: "unavailable"; reason: string };

type IssueListProps = {
  repoId: string;
  repoName: string;
  now: number;
  list: (repoId: string, query: IssueQuery) => Promise<Issue[]>;
  onPick: (issue: Issue) => void;
};

const states: readonly IssueState[] = ["open", "closed"];
const stateLabels: Record<IssueState, string> = { open: "Open", closed: "Closed" };

/** The query for a state and what is typed; a blank search is no search. */
function issueQuery(state: IssueState, search: string): IssueQuery {
  const query: IssueQuery = { state };
  const q = search.trim();
  if (q !== "") query.q = q;
  return query;
}

/** A 409 the server sent says this repo's issues cannot be listed at all; anything else may pass. */
function failedListing(error: unknown): Listing {
  if (error instanceof HttpError && error.status === 409 && error.fromServer) {
    return { status: "unavailable", reason: error.message };
  }
  return { status: "error", message: errorMessage(error) };
}

/** What an empty list says: no issue in that state, or none that matches the search. */
function emptyNote(state: IssueState, search: string, repoName: string): string {
  const q = search.trim();
  return q === "" ? `No ${state} issues in ${repoName}.` : `No ${state} issue matches "${q}".`;
}

/**
 * The search, the state and the listing they gave. Only the answer to the
 * newest request is shown: an older one that arrives late is dropped.
 */
// A class rather than a closure of functions: each method then stands, and is measured, on its own.
class IssueSearch {
  readonly search: Accessor<string>;
  readonly state: Accessor<IssueState>;
  readonly listing: Accessor<Listing>;
  private readonly setSearch: Setter<string>;
  private readonly setState: Setter<IssueState>;
  private readonly setListing: Setter<Listing>;
  private readonly props: IssueListProps;
  private asked = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;

  constructor(props: IssueListProps) {
    this.props = props;
    [this.search, this.setSearch] = createSignal("");
    [this.state, this.setState] = createSignal<IssueState>("open");
    [this.listing, this.setListing] = createSignal<Listing>({ status: "loading" });
  }

  load(): void {
    this.stop();
    this.asked += 1;
    const mine = this.asked;
    // A list already shown stays while the next one loads.
    this.setListing((current) => (current.status === "ready" ? current : { status: "loading" }));
    this.props.list(this.props.repoId, issueQuery(this.state(), this.search())).then(
      (issues) => this.settle(mine, { status: "ready", issues }),
      (error: unknown) => this.settle(mine, failedListing(error)),
    );
  }

  /** Sends the search once typing pauses. */
  type(text: string): void {
    this.setSearch(text);
    this.stop();
    this.timer = setTimeout(() => this.load(), ISSUE_SEARCH_DELAY_MS);
  }

  show(state: IssueState): void {
    if (this.state() === state) return;
    this.setState(state);
    // The setter applies later; `load` reads the state it was asked for.
    queueMicrotask(() => this.load());
  }

  stop(): void {
    clearTimeout(this.timer);
    this.timer = undefined;
  }

  ready(): readonly Issue[] | undefined {
    const current = this.listing();
    return current.status === "ready" ? current.issues : undefined;
  }

  failure(): string | undefined {
    const current = this.listing();
    return current.status === "error" ? current.message : undefined;
  }

  unavailable(): string | undefined {
    const current = this.listing();
    return current.status === "unavailable" ? current.reason : undefined;
  }

  private settle(mine: number, listing: Listing): void {
    if (mine === this.asked) this.setListing(listing);
  }
}

function StateButtons(props: { state: IssueState; onState: (state: IssueState) => void }) {
  return (
    <div role="group" aria-label="Issue state" class="flex gap-1">
      <For each={states}>
        {(one) => (
          <button
            type="button"
            class="btn px-3 text-sm"
            aria-pressed={props.state === one ? "true" : "false"}
            onClick={() => props.onState(one)}
          >
            {stateLabels[one]}
          </button>
        )}
      </For>
    </div>
  );
}

function IssueRow(props: { issue: Issue; now: number; onPick: () => void }) {
  return (
    <li>
      <button
        type="button"
        class="flex min-h-11 w-full flex-col items-start gap-1 px-1 py-2 text-left hover:bg-sunken"
        onClick={() => props.onPick()}
      >
        <span class="flex w-full min-w-0 items-baseline gap-2">
          <span class="shrink-0 font-mono text-sm text-muted">#{props.issue.number}</span>
          <span class="min-w-0 font-medium wrap-break-word">{props.issue.title}</span>
        </span>
        <span class="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted">
          <For each={props.issue.labels}>
            {(label) => (
              <span class="rounded-full border border-line px-2 py-0.5 text-fg">{label}</span>
            )}
          </For>
          <Show when={props.issue.author}>{(author) => <span>{author()}</span>}</Show>
          <time
            datetime={new Date(props.issue.updated_at).toISOString()}
            title={absoluteTime(props.issue.updated_at)}
          >
            updated {relativeTime(props.issue.updated_at, props.now)}
          </time>
        </span>
      </button>
    </li>
  );
}

/**
 * A repo's GitHub issues to start a session from: a search (sent once typing
 * pauses), open or closed, and the list. Loaded on demand from the composer's
 * "Issues" disclosure, so the first visit does not download it.
 */
export default function IssueList(props: IssueListProps) {
  const issues = new IssueSearch(props);
  const searchId = createUniqueId();

  onSettled(() => {
    issues.load();
    return () => issues.stop();
  });

  return (
    <Show
      when={issues.unavailable() === undefined}
      fallback={
        <p role="status" class="text-sm text-muted">
          {issues.unavailable()}
        </p>
      }
    >
      <div class="flex flex-col gap-2">
        <div class="flex flex-wrap items-center gap-2">
          <label for={searchId} class="sr-only">
            Search the issues of {props.repoName}
          </label>
          <input
            id={searchId}
            type="search"
            class="field h-11 min-w-0 flex-1 basis-40 px-3"
            placeholder="Search issues"
            autocomplete="off"
            autocapitalize="off"
            spellcheck={false}
            value={issues.search()}
            onInput={(event) => issues.type(event.currentTarget.value)}
          />
          <StateButtons state={issues.state()} onState={(state) => issues.show(state)} />
        </div>
        <Switch>
          <Match when={issues.listing().status === "loading"}>
            <p class="text-sm text-muted">Loading issues…</p>
          </Match>
          <Match when={issues.failure()}>
            {(message) => (
              <p role="alert" class="flex items-center justify-between gap-2 text-sm">
                <span class="min-w-0 wrap-break-word">
                  The issues could not be loaded: {message()}
                </span>
                <button type="button" class="btn" onClick={() => issues.load()}>
                  Retry
                </button>
              </p>
            )}
          </Match>
          <Match when={issues.ready()?.length === 0}>
            <p class="text-sm text-muted">
              {emptyNote(issues.state(), issues.search(), props.repoName)}
            </p>
          </Match>
          <Match when={issues.ready()}>
            {(list) => (
              <ul
                aria-label={`Issues of ${props.repoName}`}
                class="flex flex-col divide-y divide-line"
              >
                <For each={list()} keyed={(issue) => issue.number}>
                  {(issue) => (
                    <IssueRow
                      issue={issue()}
                      now={props.now}
                      onPick={() => props.onPick(issue())}
                    />
                  )}
                </For>
              </ul>
            )}
          </Match>
        </Switch>
      </div>
    </Show>
  );
}
