import { createMemo, createSignal, createUniqueId, For, Match, Show, Switch } from "solid-js";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoSet } from "../gen/RepoSet";
import { errorMessage } from "../lib/errors";
import { createMediaQuery, WIDE_QUERY } from "../lib/media";
import type { RepoMode } from "../gen/RepoMode";
import type { RepoChoice } from "../lib/prefs";
import { defaultRepoMode, repoModeFrom, repoModeLabel, repoModes } from "../lib/status";
import type { RepoRegistry } from "../state/app";
import { LoadFailure } from "./AccessSettings";

/** From this many repos on, the list gets a filter box. */
const FILTER_FROM = 9;

const tag = "shrink-0 rounded-full border border-line px-2 py-0.5 text-xs font-medium";

/** The ways a repo can be attached, as a select's options. */
export function RepoModeOptions() {
  return (
    <For each={repoModes}>{(mode) => <option value={mode}>{repoModeLabel(mode)}</option>}</For>
  );
}

/** Adds a clone on the server's machine to the registry. `onRegister` rejects with the server's reason. */
export function RegisterRepoForm(props: {
  onRegister: (request: RegisterRepoRequest) => Promise<Repo>;
  onRegistered: (repo: Repo) => void;
  onCancel: () => void;
}) {
  const [path, setPath] = createSignal("");
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const pathId = createUniqueId();
  const nameId = createUniqueId();
  const errorId = createUniqueId();

  async function submit(): Promise<void> {
    const request: RegisterRepoRequest = { path: path().trim() };
    if (request.path === "" || busy()) return;
    if (name().trim() !== "") request.name = name().trim();
    setBusy(true);
    setError(undefined);
    try {
      props.onRegistered(await props.onRegister(request));
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      aria-label="Register a repo"
      class="flex flex-col gap-2 rounded-md border border-line bg-sunken p-3"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <label for={pathId} class="text-sm font-medium">
        Path on the server
      </label>
      <input
        id={pathId}
        ref={(element) => queueMicrotask(() => element.focus())}
        type="text"
        class="field h-11 px-3 font-mono"
        placeholder="/home/you/code/project"
        autocomplete="off"
        autocapitalize="off"
        spellcheck={false}
        required
        value={path()}
        aria-describedby={error() === undefined ? undefined : errorId}
        onInput={(event) => setPath(event.currentTarget.value)}
      />
      <label for={nameId} class="text-sm font-medium">
        Name <span class="font-normal text-muted">(optional, defaults to the folder name)</span>
      </label>
      <input
        id={nameId}
        type="text"
        class="field h-11 px-3"
        autocomplete="off"
        autocapitalize="off"
        spellcheck={false}
        value={name()}
        onInput={(event) => setName(event.currentTarget.value)}
      />
      <Show when={error()}>
        {(message) => (
          <p id={errorId} role="alert" class="text-sm text-tone-danger">
            {message()}
          </p>
        )}
      </Show>
      <div class="flex justify-end gap-2">
        <button type="button" class="btn" onClick={() => props.onCancel()}>
          Cancel
        </button>
        <button type="submit" class="btn btn-primary" disabled={busy()}>
          {busy() ? "Registering…" : "Register"}
        </button>
      </div>
    </form>
  );
}

function RepoRow(props: {
  repo: Repo;
  choice: RepoChoice | undefined;
  primary: boolean;
  onToggle: (chosen: boolean) => void;
  onMode: (mode: RepoMode) => void;
  onMakePrimary: () => void;
  onRemove: () => Promise<void>;
  onClone: () => Promise<void>;
}) {
  const [confirming, setConfirming] = createSignal(false);
  const [removing, setRemoving] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const [asking, setAsking] = createSignal(false);
  const [cloneError, setCloneError] = createSignal<string>();
  const modeId = createUniqueId();

  async function remove(): Promise<void> {
    setRemoving(true);
    setError(undefined);
    try {
      await props.onRemove();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setRemoving(false);
    }
  }

  async function clone(): Promise<void> {
    if (asking()) return;
    setAsking(true);
    setCloneError(undefined);
    try {
      await props.onClone();
    } catch (reason) {
      setCloneError(errorMessage(reason));
    } finally {
      setAsking(false);
    }
  }

  const cloning = (): boolean => props.repo.cloning || asking();
  // What the home repo declares belongs to it, and the home repo to the settings: neither is removed here.
  const removable = (): boolean => !props.repo.declared && !props.repo.home;
  const where = (): string => {
    if (props.repo.available) return props.repo.path;
    return props.repo.declared ? "not cloned" : "not available";
  };

  return (
    <li class="py-1">
      <div class="flex items-center gap-2">
        <label
          class={[
            "flex min-h-11 min-w-0 flex-1 items-center gap-3",
            props.repo.available ? "cursor-pointer" : "text-muted",
          ]}
        >
          <input
            type="checkbox"
            class="size-5 shrink-0 accent-accent"
            checked={props.choice !== undefined}
            disabled={!props.repo.available}
            onChange={(event) => props.onToggle(event.currentTarget.checked)}
          />
          <span class="min-w-0">
            <span class="flex items-center gap-2">
              <span class="truncate font-medium">{props.repo.name}</span>
              <Show when={props.repo.home}>
                <span class={[tag, "text-fg"]}>Home</span>
              </Show>
              <Show when={props.primary}>
                <span class={[tag, "text-tone-active"]}>Primary</span>
              </Show>
            </span>
            <span
              class={["block truncate text-xs text-muted", { "font-mono": props.repo.available }]}
            >
              {where()}
            </span>
          </span>
        </label>
        <Show when={!props.repo.available && cloning()}>
          <span role="status" class="pulse shrink-0 text-sm text-tone-active">
            Cloning…
          </span>
        </Show>
        <Show when={!props.repo.available && props.repo.url !== null && !cloning()}>
          <button
            type="button"
            class="btn px-3 text-sm"
            aria-label={`Clone ${props.repo.name}`}
            title={props.repo.url ?? undefined}
            onClick={() => void clone()}
          >
            Clone
          </button>
        </Show>
        <Show when={removable()}>
          <button
            type="button"
            class="btn px-2 text-sm"
            aria-label={`Remove ${props.repo.name} from the registry`}
            aria-expanded={confirming() ? "true" : "false"}
            onClick={() => {
              setError(undefined);
              setConfirming((open) => !open);
            }}
          >
            Remove
          </button>
        </Show>
      </div>
      <Show when={props.repo.problem}>
        {(problem) => <p class="pb-1 pl-8 text-xs wrap-break-word text-tone-danger">{problem()}</p>}
      </Show>
      <Show when={cloneError()}>
        {(message) => (
          <p role="alert" class="pb-1 pl-8 text-xs wrap-break-word text-tone-danger">
            {message()}
          </p>
        )}
      </Show>
      <Show when={props.choice}>
        {(choice) => (
          <div class="flex flex-wrap items-center gap-2 pb-1 pl-8">
            <label for={modeId} class="sr-only">
              How {props.repo.name} is attached
            </label>
            <select
              id={modeId}
              class="field h-11 w-auto min-w-0 px-2"
              value={choice().mode}
              onChange={(event) => {
                const picked = repoModeFrom(event.currentTarget.value);
                if (picked) props.onMode(picked);
              }}
            >
              <RepoModeOptions />
            </select>
            <Show when={!props.primary}>
              <button type="button" class="btn text-sm" onClick={() => props.onMakePrimary()}>
                Make primary
              </button>
            </Show>
          </div>
        )}
      </Show>
      <Show when={confirming()}>
        <div
          role="group"
          aria-label={`Remove ${props.repo.name}`}
          class="mb-1 flex flex-col gap-2 rounded-md border border-line bg-sunken p-3 text-sm"
        >
          <p>
            Remove <span class="font-medium">{props.repo.name}</span> from the registry? The clone
            on disk is not touched.
          </p>
          <Show when={error()}>
            {(message) => (
              <p role="alert" class="text-tone-danger">
                {message()}
              </p>
            )}
          </Show>
          <div class="flex justify-end gap-2">
            <button type="button" class="btn" onClick={() => setConfirming(false)}>
              Keep
            </button>
            <button type="button" class="btn" disabled={removing()} onClick={() => void remove()}>
              {removing() ? "Removing…" : "Remove from registry"}
            </button>
          </div>
        </div>
      </Show>
    </li>
  );
}

const listed = (names: readonly string[]): string =>
  names.length <= 1 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;

/** What is selected, in a line: the primary repo by name, the others counted. */
export function selectionSummary(names: readonly string[]): string {
  const [first] = names;
  if (first === undefined) return "none selected";
  const others = names.length - 1;
  return others === 0 ? first : `${first} + ${others} ${others === 1 ? "repo" : "repos"}`;
}

/**
 * Chooses the session's repo set from the catalog. `choices` is in attach
 * order: the first one is the primary repo, the agent's working directory.
 *
 * On a phone the list folds behind a line saying what is selected, so the
 * prompt and Start stay within reach; it opens by itself while nothing is.
 */
export function RepoPicker(props: {
  registry: RepoRegistry;
  /** The home repo's named groups, offered as shortcuts. */
  sets: readonly RepoSet[];
  choices: readonly RepoChoice[];
  onChange: (choices: RepoChoice[]) => void;
  onRegister: (request: RegisterRepoRequest) => Promise<Repo>;
  onRemove: (id: string) => Promise<void>;
  onClone: (id: string) => Promise<void>;
  onRetry: () => void;
}) {
  const wide = createMediaQuery(WIDE_QUERY);
  const [registering, setRegistering] = createSignal(false);
  // What the reader last asked of the fold; until they do, it follows the selection.
  const [openChoice, setOpenChoice] = createSignal<boolean>();
  const [filter, setFilter] = createSignal("");
  // The set last chosen: what it could not select is said for as long as that stays true.
  const [notedSet, setNotedSet] = createSignal<string>();
  const headingId = createUniqueId();
  const bodyId = createUniqueId();
  const filterId = createUniqueId();

  const repos = (): readonly Repo[] =>
    props.registry.status === "ready" ? props.registry.repos : [];
  const loadError = () => (props.registry.status === "error" ? props.registry : undefined);
  const byId = createMemo(() => new Map(repos().map((repo) => [repo.id, repo])));
  const chosenNames = createMemo(() =>
    props.choices.flatMap((choice) => byId().get(choice.repoId)?.name ?? []),
  );
  const primaryName = (): string | undefined => chosenNames().at(0);

  const open = (): boolean =>
    wide() || (openChoice() ?? (props.registry.status !== "ready" || props.choices.length === 0));

  const visible = createMemo(() => {
    const wanted = filter().trim().toLowerCase();
    if (wanted === "" || repos().length < FILTER_FROM) return repos();
    return repos().filter((repo) => repo.name.toLowerCase().includes(wanted));
  });

  /** A choice made in the list: it stays open under the reader's hand. */
  function change(next: RepoChoice[]): void {
    setOpenChoice(true);
    props.onChange(next);
  }

  const without = (id: string): RepoChoice[] =>
    props.choices.filter((choice) => choice.repoId !== id);

  function toggle(repo: Repo, chosen: boolean): void {
    if (!chosen) change(without(repo.id));
    // The home repo is where the agent starts, so it goes first; the others in the order chosen.
    else if (repo.home)
      change([{ repoId: repo.id, mode: defaultRepoMode(repo) }, ...without(repo.id)]);
    else change([...without(repo.id), { repoId: repo.id, mode: defaultRepoMode(repo) }]);
  }

  function setMode(id: string, mode: RepoMode): void {
    change(props.choices.map((choice) => (choice.repoId === id ? { ...choice, mode } : choice)));
  }

  function makePrimary(id: string): void {
    const choice = props.choices.find((one) => one.repoId === id);
    if (choice) change([choice, ...without(id)]);
  }

  /** A set's repos as the catalog knows them: those that can be attached, and those not cloned. */
  function members(set: RepoSet): { available: Repo[]; missing: Repo[] } {
    const known = set.repo_ids.flatMap((id) => byId().get(id) ?? []);
    return {
      available: known.filter((repo) => repo.available),
      missing: known.filter((repo) => !repo.available),
    };
  }

  const setChosen = (set: RepoSet): boolean => {
    const { available } = members(set);
    return (
      available.length > 0 &&
      available.every((repo) => props.choices.some((choice) => choice.repoId === repo.id))
    );
  };

  function toggleSet(set: RepoSet): void {
    if (setChosen(set)) {
      change(props.choices.filter((choice) => !set.repo_ids.includes(choice.repoId)));
      setNotedSet(undefined);
      return;
    }
    const added = members(set).available.filter(
      (repo) => !props.choices.some((choice) => choice.repoId === repo.id),
    );
    // Added to what is selected, each as a worktree.
    change([
      ...props.choices,
      ...added.map((repo): RepoChoice => ({ repoId: repo.id, mode: "write" })),
    ]);
    setNotedSet(set.name);
  }

  /** Which repos of the chosen set were left out; gone once they are cloned. */
  const setNote = (): string | undefined => {
    const set = props.sets.find((one) => one.name === notedSet());
    const names = set ? members(set).missing.map((repo) => repo.name) : [];
    if (!set || names.length === 0) return undefined;
    const [is, was] = names.length === 1 ? ["is", "was"] : ["are", "were"];
    return `${set.name}: ${listed(names)} ${is} not cloned and ${was} left out.`;
  };

  return (
    <section aria-labelledby={headingId} class="flex flex-col gap-1">
      <div class="flex min-h-11 items-center gap-2">
        <h2 id={headingId} class="shrink-0 text-sm font-medium">
          Repos
        </h2>
        <Show
          when={!wide()}
          fallback={
            <span class="min-w-0 flex-1 truncate text-sm text-muted">
              <Show when={props.registry.status === "ready" && repos().length > 0}>
                {selectionSummary(chosenNames())}
              </Show>
            </span>
          }
        >
          <button
            type="button"
            class="flex min-h-11 min-w-0 flex-1 items-center gap-1.5 text-left text-sm"
            aria-expanded={open() ? "true" : "false"}
            aria-controls={bodyId}
            onClick={() => setOpenChoice(!open())}
          >
            <span class="sr-only">Selected:</span>
            <span class="truncate text-muted">
              {props.registry.status === "ready" ? selectionSummary(chosenNames()) : "…"}
            </span>
            <span
              aria-hidden="true"
              class={["text-muted transition-transform", { "rotate-90": open() }]}
            >
              ▸
            </span>
          </button>
        </Show>
        <Show when={open() && repos().length > 0 && !registering()}>
          <button type="button" class="btn shrink-0 text-sm" onClick={() => setRegistering(true)}>
            Register a repo
          </button>
        </Show>
      </div>
      <div id={bodyId} hidden={!open()} class="flex flex-col gap-1">
        <Switch>
          <Match when={props.registry.status === "loading"}>
            <p class="text-sm text-muted">Loading repos…</p>
          </Match>
          <Match when={loadError()}>
            {(failure) => (
              <LoadFailure
                what="The repos"
                message={failure().message}
                onRetry={() => props.onRetry()}
              />
            )}
          </Match>
          <Match when={repos().length === 0}>
            <div class="flex flex-col items-start gap-2 text-sm">
              <p class="font-medium">No repo is registered yet</p>
              <p class="text-muted">
                A repo is a git clone on the server's machine. A session attaches the repos the
                agent should work in, each as its own worktree or edited in place. With none, the
                agent works in an empty folder.
              </p>
              <p class="text-muted">
                A home repo lists your repos once for every session:{" "}
                <a href="/settings" class="text-accent underline">
                  set it in Settings
                </a>
                .
              </p>
              <Show when={!registering()}>
                <button type="button" class="btn" onClick={() => setRegistering(true)}>
                  Register a repo
                </button>
              </Show>
            </div>
          </Match>
          <Match when={true}>
            <Show when={props.sets.length > 0}>
              <div role="group" aria-label="Repo sets" class="flex flex-wrap items-center gap-2">
                <span class="text-xs text-muted">Sets</span>
                <For each={props.sets} keyed={(set) => set.name}>
                  {(set) => (
                    <button
                      type="button"
                      class="btn rounded-full px-3 text-sm"
                      aria-pressed={setChosen(set()) ? "true" : "false"}
                      onClick={() => toggleSet(set())}
                    >
                      {set().name}
                      <span class="text-xs font-normal text-muted">
                        {members(set()).available.length}
                        <Show when={members(set()).missing.length > 0}>
                          /{members(set()).available.length + members(set()).missing.length}
                        </Show>
                      </span>
                    </button>
                  )}
                </For>
              </div>
              <Show when={setNote()}>
                {(note) => (
                  <p role="status" class="text-xs text-tone-attention">
                    {note()}
                  </p>
                )}
              </Show>
            </Show>
            <Show when={repos().length >= FILTER_FROM}>
              <label for={filterId} class="sr-only">
                Filter repos
              </label>
              <input
                id={filterId}
                type="search"
                class="field h-11 px-3"
                placeholder="Filter repos"
                autocomplete="off"
                autocapitalize="off"
                spellcheck={false}
                value={filter()}
                onInput={(event) => setFilter(event.currentTarget.value)}
              />
            </Show>
            <ul class="divide-y divide-line lg:max-h-[50dvh] lg:overflow-y-auto lg:overscroll-contain">
              <For each={visible()} keyed={(repo) => repo.id}>
                {(repo) => (
                  <RepoRow
                    repo={repo()}
                    choice={props.choices.find((choice) => choice.repoId === repo().id)}
                    primary={props.choices.at(0)?.repoId === repo().id}
                    onToggle={(chosen) => toggle(repo(), chosen)}
                    onMode={(mode) => setMode(repo().id, mode)}
                    onMakePrimary={() => makePrimary(repo().id)}
                    onRemove={() => props.onRemove(repo().id)}
                    onClone={() => props.onClone(repo().id)}
                  />
                )}
              </For>
            </ul>
            <Show when={visible().length === 0}>
              <p class="text-sm text-muted">No repo matches "{filter().trim()}".</p>
            </Show>
            <p class="text-xs text-muted">
              <Show
                when={primaryName()}
                fallback="No repo selected: the agent works in an empty folder."
              >
                {(name) => (
                  <>
                    The agent starts in <span class="font-medium">{name()}</span>, the primary repo.
                  </>
                )}
              </Show>
            </p>
            <Show when={primaryName() !== undefined && props.choices.at(0)?.mode === "read"}>
              <p data-part="read-only-primary" class="text-xs text-muted">
                The agent starts in a read-only repo and will have to ask before changing anything
                there.
              </p>
            </Show>
          </Match>
        </Switch>
        <Show when={registering()}>
          <RegisterRepoForm
            onRegister={props.onRegister}
            onRegistered={(repo) => {
              setRegistering(false);
              toggle(repo, true);
            }}
            onCancel={() => setRegistering(false)}
          />
        </Show>
      </div>
    </section>
  );
}
