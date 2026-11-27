import { createSignal, createUniqueId, For, Match, onSettled, Show, Switch } from "solid-js";
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

/**
 * A repo's GitHub issues to start a session from: a search (sent once typing
 * pauses), open or closed, and the list. Loaded on demand from the composer's
 * "Issues" disclosure, so the first visit does not download it.
 */
export default function IssueList(props: {
  repoId: string;
  repoName: string;
  now: number;
  list: (repoId: string, query: IssueQuery) => Promise<Issue[]>;
  onPick: (issue: Issue) => void;
}) {
  const [search, setSearch] = createSignal("");
  const [state, setState] = createSignal<IssueState>("open");
  const [listing, setListing] = createSignal<Listing>({ status: "loading" });
  const searchId = createUniqueId();
  let asked = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;

  function load(): void {
    clearTimeout(timer);
    timer = undefined;
    asked += 1;
    const mine = asked;
    const query: IssueQuery = { state: state() };
    const q = search().trim();
    if (q !== "") query.q = q;
    setListing((current) => (current.status === "ready" ? current : { status: "loading" }));
    props.list(props.repoId, query).then(
      (issues) => {
        if (mine === asked) setListing({ status: "ready", issues });
      },
      (error: unknown) => {
        if (mine !== asked) return;
        if (error instanceof HttpError && error.status === 409 && error.fromServer) {
          setListing({ status: "unavailable", reason: error.message });
        } else {
          setListing({ status: "error", message: errorMessage(error) });
        }
      },
    );
  }

  onSettled(() => {
    load();
    return () => clearTimeout(timer);
  });

  const ready = (): readonly Issue[] | undefined => {
    const current = listing();
    return current.status === "ready" ? current.issues : undefined;
  };
  const failure = (): string | undefined => {
    const current = listing();
    return current.status === "error" ? current.message : undefined;
  };
  const unavailable = (): string | undefined => {
    const current = listing();
    return current.status === "unavailable" ? current.reason : undefined;
  };

  return (
    <Show
      when={unavailable() === undefined}
      fallback={
        <p role="status" class="text-sm text-muted">
          {unavailable()}
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
            value={search()}
            onInput={(event) => {
              setSearch(event.currentTarget.value);
              clearTimeout(timer);
              timer = setTimeout(load, ISSUE_SEARCH_DELAY_MS);
            }}
          />
          <div role="group" aria-label="Issue state" class="flex gap-1">
            <For each={["open", "closed"] as const}>
              {(one) => (
                <button
                  type="button"
                  class="btn px-3 text-sm"
                  aria-pressed={state() === one ? "true" : "false"}
                  onClick={() => {
                    if (state() === one) return;
                    setState(one);
                    // The setter applies later; `load` reads the state it was asked for.
                    queueMicrotask(load);
                  }}
                >
                  {one === "open" ? "Open" : "Closed"}
                </button>
              )}
            </For>
          </div>
        </div>
        <Switch>
          <Match when={listing().status === "loading"}>
            <p class="text-sm text-muted">Loading issues…</p>
          </Match>
          <Match when={failure()}>
            {(message) => (
              <p role="alert" class="flex items-center justify-between gap-2 text-sm">
                <span class="min-w-0 wrap-break-word">
                  The issues could not be loaded: {message()}
                </span>
                <button type="button" class="btn" onClick={() => load()}>
                  Retry
                </button>
              </p>
            )}
          </Match>
          <Match when={ready()?.length === 0}>
            <p class="text-sm text-muted">
              {search().trim() === ""
                ? `No ${state()} issues in ${props.repoName}.`
                : `No ${state()} issue matches "${search().trim()}".`}
            </p>
          </Match>
          <Match when={ready()}>
            {(issues) => (
              <ul
                aria-label={`Issues of ${props.repoName}`}
                class="flex flex-col divide-y divide-line"
              >
                <For each={issues()} keyed={(issue) => issue.number}>
                  {(issue) => (
                    <li>
                      <button
                        type="button"
                        class="flex min-h-11 w-full flex-col items-start gap-1 px-1 py-2 text-left hover:bg-sunken"
                        onClick={() => props.onPick(issue())}
                      >
                        <span class="flex w-full min-w-0 items-baseline gap-2">
                          <span class="shrink-0 font-mono text-sm text-muted">
                            #{issue().number}
                          </span>
                          <span class="min-w-0 font-medium wrap-break-word">{issue().title}</span>
                        </span>
                        <span class="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted">
                          <For each={issue().labels}>
                            {(label) => (
                              <span class="rounded-full border border-line px-2 py-0.5 text-fg">
                                {label}
                              </span>
                            )}
                          </For>
                          <Show when={issue().author}>{(author) => <span>{author()}</span>}</Show>
                          <time
                            datetime={new Date(issue().updated_at).toISOString()}
                            title={absoluteTime(issue().updated_at)}
                          >
                            updated {relativeTime(issue().updated_at, props.now)}
                          </time>
                        </span>
                      </button>
                    </li>
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
