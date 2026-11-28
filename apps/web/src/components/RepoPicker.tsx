import {
  createMemo,
  createSignal,
  createUniqueId,
  For,
  Match,
  Show,
  Switch,
  type Accessor,
  type Setter,
} from "solid-js";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoMode } from "../gen/RepoMode";
import type { RepoSet } from "../gen/RepoSet";
import { createMediaQuery, WIDE_QUERY } from "../lib/media";
import type { RepoChoice } from "../lib/prefs";
import {
  filteredRepos,
  leftOutNote,
  madePrimary,
  selectionSummary,
  setChosen,
  setMembers,
  toggled,
  toggledSet,
  withMode,
} from "../lib/repoChoices";
import { createRequest } from "../lib/request";
import { repoModeFrom, repoModeLabel, repoModes } from "../lib/status";
import { onFormSubmit } from "../lib/submit";
import type { RepoRegistry } from "../state/app";
import { LoadFailure } from "./AccessSettings";
import { ErrorNote } from "./ErrorNote";

/** From this many repos on, the list gets a filter box. */
const FILTER_FROM = 9;

const tag = "shrink-0 rounded-full border border-line px-2 py-0.5 text-xs font-medium";

/** The ways a repo can be attached, as a select's options. */
export function RepoModeOptions() {
  return (
    <For each={repoModes}>{(mode) => <option value={mode}>{repoModeLabel(mode)}</option>}</For>
  );
}

/** What is sent to register a clone; `undefined` while the path is blank. */
function registerRequest(path: string, name: string): RegisterRepoRequest | undefined {
  const request: RegisterRepoRequest = { path: path.trim() };
  if (request.path === "") return undefined;
  if (name.trim() !== "") request.name = name.trim();
  return request;
}

/** Adds a clone on the server's machine to the registry. `onRegister` rejects with the server's reason. */
export function RegisterRepoForm(props: {
  onRegister: (request: RegisterRepoRequest) => Promise<Repo>;
  onRegistered: (repo: Repo) => void;
  onCancel: () => void;
}) {
  const [path, setPath] = createSignal("");
  const [name, setName] = createSignal("");
  const pathId = createUniqueId();
  const nameId = createUniqueId();
  const errorId = createUniqueId();

  const registering = createRequest(async (request: RegisterRepoRequest) => {
    props.onRegistered(await props.onRegister(request));
  });
  async function submit(): Promise<void> {
    const request = registerRequest(path(), name());
    if (request) await registering.run(request);
  }

  return (
    <form
      aria-label="Register a repo"
      class="flex flex-col gap-2 rounded-md border border-line bg-sunken p-3"
      onSubmit={onFormSubmit(submit)}
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
        aria-describedby={registering.error() === undefined ? undefined : errorId}
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
      <ErrorNote message={registering.error()} id={errorId} />
      <div class="flex justify-end gap-2">
        <button type="button" class="btn" onClick={() => props.onCancel()}>
          Cancel
        </button>
        <button type="submit" class="btn btn-primary" disabled={registering.busy()}>
          {registering.busy() ? "Registering…" : "Register"}
        </button>
      </div>
    </form>
  );
}

/** Where the clone is, or why the repo cannot be used. */
function repoPlace(repo: Repo): string {
  if (repo.available) return repo.path;
  return repo.declared ? "not cloned" : "not available";
}

/** The checkbox that attaches a repo, with its name, its tags and where it is. */
function RepoLabel(props: {
  repo: Repo;
  chosen: boolean;
  primary: boolean;
  onToggle: (chosen: boolean) => void;
}) {
  return (
    <label
      class={[
        "flex min-h-11 min-w-0 flex-1 items-center gap-3",
        props.repo.available ? "cursor-pointer" : "text-muted",
      ]}
    >
      <input
        type="checkbox"
        class="size-5 shrink-0 accent-accent"
        checked={props.chosen}
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
        <span class={["block truncate text-xs text-muted", { "font-mono": props.repo.available }]}>
          {repoPlace(props.repo)}
        </span>
      </span>
    </label>
  );
}

/** A repo with no clone yet: Clone when it has a url, or that a clone is running. */
function CloneAction(props: { repo: Repo; asking: boolean; onClone: () => void }) {
  const cloning = (): boolean => props.repo.cloning || props.asking;
  return (
    <>
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
          onClick={() => props.onClone()}
        >
          Clone
        </button>
      </Show>
    </>
  );
}

/** How a chosen repo is attached, and the way to make it the primary one. */
function ModeLine(props: {
  name: string;
  mode: RepoMode;
  primary: boolean;
  onMode: (mode: RepoMode) => void;
  onMakePrimary: () => void;
}) {
  const modeId = createUniqueId();
  function pick(value: string): void {
    const picked = repoModeFrom(value);
    if (picked) props.onMode(picked);
  }
  return (
    <div class="flex flex-wrap items-center gap-2 pb-1 pl-8">
      <label for={modeId} class="sr-only">
        How {props.name} is attached
      </label>
      <select
        id={modeId}
        class="field h-11 w-auto min-w-0 px-2"
        value={props.mode}
        onChange={(event) => pick(event.currentTarget.value)}
      >
        <RepoModeOptions />
      </select>
      <Show when={!props.primary}>
        <button type="button" class="btn text-sm" onClick={() => props.onMakePrimary()}>
          Make primary
        </button>
      </Show>
    </div>
  );
}

/** The question asked before a repo leaves the registry. */
function RemoveConfirm(props: {
  name: string;
  busy: boolean;
  error: string | undefined;
  onKeep: () => void;
  onRemove: () => void;
}) {
  return (
    <div
      role="group"
      aria-label={`Remove ${props.name}`}
      class="mb-1 flex flex-col gap-2 rounded-md border border-line bg-sunken p-3 text-sm"
    >
      <p>
        Remove <span class="font-medium">{props.name}</span> from the registry? The clone on disk is
        not touched.
      </p>
      <Show when={props.error}>
        {(message) => (
          <p role="alert" class="text-tone-danger">
            {message()}
          </p>
        )}
      </Show>
      <div class="flex justify-end gap-2">
        <button type="button" class="btn" onClick={() => props.onKeep()}>
          Keep
        </button>
        <button type="button" class="btn" disabled={props.busy} onClick={() => props.onRemove()}>
          {props.busy ? "Removing…" : "Remove from registry"}
        </button>
      </div>
    </div>
  );
}

const rowError = "pb-1 pl-8 text-xs wrap-break-word text-tone-danger";

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
  const removal = createRequest(() => props.onRemove());
  const cloning = createRequest(() => props.onClone());
  // What the home repo declares belongs to it, and the home repo to the settings: neither is removed here.
  const removable = (): boolean => !props.repo.declared && !props.repo.home;
  function askRemove(): void {
    removal.clearError();
    setConfirming((open) => !open);
  }

  return (
    <li class="py-1">
      <div class="flex items-center gap-2">
        <RepoLabel
          repo={props.repo}
          chosen={props.choice !== undefined}
          primary={props.primary}
          onToggle={props.onToggle}
        />
        <CloneAction repo={props.repo} asking={cloning.busy()} onClone={() => void cloning.run()} />
        <Show when={removable()}>
          <button
            type="button"
            class="btn px-2 text-sm"
            aria-label={`Remove ${props.repo.name} from the registry`}
            aria-expanded={confirming() ? "true" : "false"}
            onClick={askRemove}
          >
            Remove
          </button>
        </Show>
      </div>
      <Show when={props.repo.problem}>{(problem) => <p class={rowError}>{problem()}</p>}</Show>
      <Show when={cloning.error()}>
        {(message) => (
          <p role="alert" class={rowError}>
            {message()}
          </p>
        )}
      </Show>
      <Show when={props.choice}>
        {(choice) => (
          <ModeLine
            name={props.repo.name}
            mode={choice().mode}
            primary={props.primary}
            onMode={props.onMode}
            onMakePrimary={props.onMakePrimary}
          />
        )}
      </Show>
      <Show when={confirming()}>
        <RemoveConfirm
          name={props.repo.name}
          busy={removal.busy()}
          error={removal.error()}
          onKeep={() => setConfirming(false)}
          onRemove={() => void removal.run()}
        />
      </Show>
    </li>
  );
}

type RepoPickerProps = {
  registry: RepoRegistry;
  /** The home repo's named groups, offered as shortcuts. */
  sets: readonly RepoSet[];
  choices: readonly RepoChoice[];
  onChange: (choices: RepoChoice[]) => void;
  onRegister: (request: RegisterRepoRequest) => Promise<Repo>;
  onRemove: (id: string) => Promise<void>;
  onClone: (id: string) => Promise<void>;
  onRetry: () => void;
};

/**
 * What the picker decides apart from its markup: the fold, the filter, the
 * register form and the set last chosen, and each change to the selection.
 */
// A class rather than a closure of functions: each method then stands, and is measured, on its own.
class PickerState {
  readonly registering: Accessor<boolean>;
  readonly setRegistering: Setter<boolean>;
  readonly filter: Accessor<string>;
  readonly setFilter: Setter<string>;
  readonly wide: Accessor<boolean>;
  readonly byId: Accessor<Map<string, Repo>>;
  readonly chosenNames: Accessor<string[]>;
  readonly visible: Accessor<readonly Repo[]>;
  readonly props: RepoPickerProps;
  // What the reader last asked of the fold; until they do, it follows the selection.
  private readonly openChoice: Accessor<boolean | undefined>;
  private readonly setOpenChoice: Setter<boolean | undefined>;
  // The set last chosen: what it could not select is said for as long as that stays true.
  private readonly notedSet: Accessor<string | undefined>;
  private readonly setNotedSet: Setter<string | undefined>;

  constructor(props: RepoPickerProps) {
    this.props = props;
    this.wide = createMediaQuery(WIDE_QUERY);
    [this.registering, this.setRegistering] = createSignal(false);
    [this.openChoice, this.setOpenChoice] = createSignal<boolean>();
    [this.filter, this.setFilter] = createSignal("");
    [this.notedSet, this.setNotedSet] = createSignal<string>();
    this.byId = createMemo(() => new Map(this.repos().map((repo) => [repo.id, repo])));
    this.chosenNames = createMemo(() =>
      props.choices.flatMap((choice) => this.byId().get(choice.repoId)?.name ?? []),
    );
    this.visible = createMemo(() => filteredRepos(this.repos(), this.filter(), FILTER_FROM));
  }

  repos(): readonly Repo[] {
    return this.props.registry.status === "ready" ? this.props.registry.repos : [];
  }

  loadError(): { message: string } | undefined {
    return this.props.registry.status === "error" ? this.props.registry : undefined;
  }

  ready(): boolean {
    return this.props.registry.status === "ready";
  }

  /** The line said for the selection; `…` until the registry is read. */
  summary(): string {
    return this.ready() ? selectionSummary(this.chosenNames()) : "…";
  }

  /** Open on a wide screen; on a phone, as the reader left it, else while nothing is selected. */
  open(): boolean {
    if (this.wide()) return true;
    return this.openChoice() ?? (!this.ready() || this.props.choices.length === 0);
  }

  toggleOpen(): void {
    this.setOpenChoice(!this.open());
  }

  choiceOf(id: string): RepoChoice | undefined {
    return this.props.choices.find((choice) => choice.repoId === id);
  }

  isPrimary(id: string): boolean {
    return this.props.choices.at(0)?.repoId === id;
  }

  primaryName(): string | undefined {
    return this.chosenNames().at(0);
  }

  /** Whether the agent starts in a repo it may only read. */
  readOnlyPrimary(): boolean {
    return this.primaryName() !== undefined && this.props.choices.at(0)?.mode === "read";
  }

  toggle(repo: Repo, chosen: boolean): void {
    this.change(toggled(this.props.choices, repo, chosen));
  }

  setMode(id: string, mode: RepoMode): void {
    this.change(withMode(this.props.choices, id, mode));
  }

  makePrimary(id: string): void {
    const next = madePrimary(this.props.choices, id);
    if (next) this.change(next);
  }

  setChosen(set: RepoSet): boolean {
    return setChosen(this.props.choices, set, this.byId());
  }

  members(set: RepoSet): { available: Repo[]; missing: Repo[] } {
    return setMembers(set, this.byId());
  }

  toggleSet(set: RepoSet): void {
    const removing = this.setChosen(set);
    this.change(toggledSet(this.props.choices, set, this.byId()));
    this.setNotedSet(removing ? undefined : set.name);
  }

  /** Which repos of the chosen set were left out; gone once they are cloned. */
  setNote(): string | undefined {
    const set = this.props.sets.find((one) => one.name === this.notedSet());
    return set ? leftOutNote(set, this.byId()) : undefined;
  }

  /** The repo that was just registered is the one the reader wants attached. */
  registered(repo: Repo): void {
    this.setRegistering(false);
    this.toggle(repo, true);
  }

  /** A choice made in the list: it stays open under the reader's hand. */
  private change(next: RepoChoice[]): void {
    this.setOpenChoice(true);
    this.props.onChange(next);
  }
}

/** The heading, with what is selected: a fold on a phone, a line on a wide screen. */
function PickerHeading(props: { state: PickerState; headingId: string; bodyId: string }) {
  const state = (): PickerState => props.state;
  return (
    <div class="flex min-h-11 items-center gap-2">
      <h2 id={props.headingId} class="shrink-0 text-sm font-medium">
        Repos
      </h2>
      <Show
        when={!state().wide()}
        fallback={
          <span class="min-w-0 flex-1 truncate text-sm text-muted">
            <Show when={state().ready() && state().repos().length > 0}>
              {selectionSummary(state().chosenNames())}
            </Show>
          </span>
        }
      >
        <button
          type="button"
          class="flex min-h-11 min-w-0 flex-1 items-center gap-1.5 text-left text-sm"
          aria-expanded={state().open() ? "true" : "false"}
          aria-controls={props.bodyId}
          onClick={() => state().toggleOpen()}
        >
          <span class="sr-only">Selected:</span>
          <span class="truncate text-muted">{state().summary()}</span>
          <span
            aria-hidden="true"
            class={["text-muted transition-transform", { "rotate-90": state().open() }]}
          >
            ▸
          </span>
        </button>
      </Show>
      <Show when={state().open() && state().repos().length > 0 && !state().registering()}>
        <button
          type="button"
          class="btn shrink-0 text-sm"
          onClick={() => state().setRegistering(true)}
        >
          Register a repo
        </button>
      </Show>
    </div>
  );
}

function NoRepoYet(props: { registering: boolean; onRegister: () => void }) {
  return (
    <div class="flex flex-col items-start gap-2 text-sm">
      <p class="font-medium">No repo is registered yet</p>
      <p class="text-muted">
        A repo is a git clone on the server's machine. A session attaches the repos the agent should
        work in, each as its own worktree or edited in place. With none, the agent works in an empty
        folder.
      </p>
      <p class="text-muted">
        A home repo lists your repos once for every session:{" "}
        <a href="/settings" class="text-accent underline">
          set it in Settings
        </a>
        .
      </p>
      <Show when={!props.registering}>
        <button type="button" class="btn" onClick={() => props.onRegister()}>
          Register a repo
        </button>
      </Show>
    </div>
  );
}

function SetChip(props: { state: PickerState; set: RepoSet }) {
  const members = () => props.state.members(props.set);
  return (
    <button
      type="button"
      class="btn rounded-full px-3 text-sm"
      aria-pressed={props.state.setChosen(props.set) ? "true" : "false"}
      onClick={() => props.state.toggleSet(props.set)}
    >
      {props.set.name}
      <span class="text-xs font-normal text-muted">
        {members().available.length}
        <Show when={members().missing.length > 0}>
          /{members().available.length + members().missing.length}
        </Show>
      </span>
    </button>
  );
}

function SetChips(props: { state: PickerState; sets: readonly RepoSet[] }) {
  return (
    <>
      <div role="group" aria-label="Repo sets" class="flex flex-wrap items-center gap-2">
        <span class="text-xs text-muted">Sets</span>
        <For each={props.sets} keyed={(set) => set.name}>
          {(set) => <SetChip state={props.state} set={set()} />}
        </For>
      </div>
      <Show when={props.state.setNote()}>
        {(note) => (
          <p role="status" class="text-xs text-tone-attention">
            {note()}
          </p>
        )}
      </Show>
    </>
  );
}

function RepoList(props: { state: PickerState }) {
  const state = (): PickerState => props.state;
  return (
    <ul class="divide-y divide-line lg:max-h-[50dvh] lg:overflow-y-auto lg:overscroll-contain">
      <For each={state().visible()} keyed={(repo) => repo.id}>
        {(repo) => (
          <RepoRow
            repo={repo()}
            choice={state().choiceOf(repo().id)}
            primary={state().isPrimary(repo().id)}
            onToggle={(chosen) => state().toggle(repo(), chosen)}
            onMode={(mode) => state().setMode(repo().id, mode)}
            onMakePrimary={() => state().makePrimary(repo().id)}
            onRemove={() => state().props.onRemove(repo().id)}
            onClone={() => state().props.onClone(repo().id)}
          />
        )}
      </For>
    </ul>
  );
}

/** The registered repos: the sets, the filter, the list, and where the agent starts. */
function Catalog(props: { state: PickerState }) {
  const state = (): PickerState => props.state;
  const filterId = createUniqueId();
  return (
    <>
      <Show when={state().props.sets.length > 0}>
        <SetChips state={state()} sets={state().props.sets} />
      </Show>
      <Show when={state().repos().length >= FILTER_FROM}>
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
          value={state().filter()}
          onInput={(event) => state().setFilter(event.currentTarget.value)}
        />
      </Show>
      <RepoList state={state()} />
      <Show when={state().visible().length === 0}>
        <p class="text-sm text-muted">No repo matches "{state().filter().trim()}".</p>
      </Show>
      <p class="text-xs text-muted">
        <Show
          when={state().primaryName()}
          fallback="No repo selected: the agent works in an empty folder."
        >
          {(name) => (
            <>
              The agent starts in <span class="font-medium">{name()}</span>, the primary repo.
            </>
          )}
        </Show>
      </p>
      <Show when={state().readOnlyPrimary()}>
        <p data-part="read-only-primary" class="text-xs text-muted">
          The agent starts in a read-only repo and will have to ask before changing anything there.
        </p>
      </Show>
    </>
  );
}

/**
 * Chooses the session's repo set from the catalog. `choices` is in attach
 * order: the first one is the primary repo, the agent's working directory.
 *
 * On a phone the list folds behind a line saying what is selected, so the
 * prompt and Start stay within reach; it opens by itself while nothing is.
 */
export function RepoPicker(props: RepoPickerProps) {
  const state = new PickerState(props);
  const headingId = createUniqueId();
  const bodyId = createUniqueId();

  return (
    <section aria-labelledby={headingId} class="flex flex-col gap-1">
      <PickerHeading state={state} headingId={headingId} bodyId={bodyId} />
      <div id={bodyId} hidden={!state.open()} class="flex flex-col gap-1">
        <Switch>
          <Match when={props.registry.status === "loading"}>
            <p class="text-sm text-muted">Loading repos…</p>
          </Match>
          <Match when={state.loadError()}>
            {(failure) => (
              <LoadFailure
                what="The repos"
                message={failure().message}
                onRetry={() => props.onRetry()}
              />
            )}
          </Match>
          <Match when={state.repos().length === 0}>
            <NoRepoYet
              registering={state.registering()}
              onRegister={() => state.setRegistering(true)}
            />
          </Match>
          <Match when={true}>
            <Catalog state={state} />
          </Match>
        </Switch>
        <Show when={state.registering()}>
          <RegisterRepoForm
            onRegister={props.onRegister}
            onRegistered={(repo) => state.registered(repo)}
            onCancel={() => state.setRegistering(false)}
          />
        </Show>
      </div>
    </section>
  );
}
