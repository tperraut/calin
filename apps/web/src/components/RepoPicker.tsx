import { createSignal, createUniqueId, For, Match, Show, Switch } from "solid-js";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import { errorMessage } from "../lib/errors";
import type { ComposerRepoMode, RepoChoice } from "../lib/prefs";
import { repoModeLabel } from "../lib/status";
import type { RepoRegistry } from "../state/app";

const composerModes: readonly ComposerRepoMode[] = ["write", "live"];

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
  onMode: (mode: ComposerRepoMode) => void;
  onMakePrimary: () => void;
  onRemove: () => Promise<void>;
}) {
  const [confirming, setConfirming] = createSignal(false);
  const [removing, setRemoving] = createSignal(false);
  const [error, setError] = createSignal<string>();
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

  return (
    <li class="py-1">
      <div class="flex items-center gap-2">
        <label class="flex min-h-11 min-w-0 flex-1 cursor-pointer items-center gap-3">
          <input
            type="checkbox"
            class="size-5 shrink-0 accent-accent"
            checked={props.choice !== undefined}
            onChange={(event) => props.onToggle(event.currentTarget.checked)}
          />
          <span class="min-w-0">
            <span class="flex items-center gap-2">
              <span class="truncate font-medium">{props.repo.name}</span>
              <Show when={props.primary}>
                <span class="shrink-0 rounded-full border border-line px-2 py-0.5 text-xs font-medium text-tone-active">
                  Primary
                </span>
              </Show>
            </span>
            <span class="block truncate font-mono text-xs text-muted">{props.repo.path}</span>
          </span>
        </label>
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
      </div>
      <Show when={props.choice}>
        {(choice) => (
          <div class="flex flex-wrap items-center gap-2 pb-1 pl-8">
            <label for={modeId} class="sr-only">
              How {props.repo.name} is attached
            </label>
            <select
              id={modeId}
              class="field h-11 w-auto min-w-0 px-2"
              onChange={(event) => {
                const picked = composerModes.find((mode) => mode === event.currentTarget.value);
                if (picked) props.onMode(picked);
              }}
            >
              <For each={composerModes}>
                {(mode) => (
                  <option value={mode} selected={mode === choice().mode}>
                    {repoModeLabel(mode)}
                  </option>
                )}
              </For>
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

/**
 * Chooses the session's repo set from the registry. `choices` is in attach
 * order: the first one is the primary repo, the agent's working directory.
 */
export function RepoPicker(props: {
  registry: RepoRegistry;
  choices: readonly RepoChoice[];
  onChange: (choices: RepoChoice[]) => void;
  onRegister: (request: RegisterRepoRequest) => Promise<Repo>;
  onRemove: (id: string) => Promise<void>;
  onRetry: () => void;
}) {
  const [registering, setRegistering] = createSignal(false);
  const headingId = createUniqueId();

  const repos = (): readonly Repo[] =>
    props.registry.status === "ready" ? props.registry.repos : [];
  const loadError = () => (props.registry.status === "error" ? props.registry : undefined);
  const primaryName = (): string | undefined => {
    const first = props.choices.at(0);
    return first && repos().find((repo) => repo.id === first.repoId)?.name;
  };

  const without = (id: string): RepoChoice[] =>
    props.choices.filter((choice) => choice.repoId !== id);

  function toggle(id: string, chosen: boolean): void {
    props.onChange(chosen ? [...without(id), { repoId: id, mode: "write" }] : without(id));
  }

  function setMode(id: string, mode: ComposerRepoMode): void {
    props.onChange(
      props.choices.map((choice) => (choice.repoId === id ? { ...choice, mode } : choice)),
    );
  }

  function makePrimary(id: string): void {
    const choice = props.choices.find((one) => one.repoId === id);
    if (choice) props.onChange([choice, ...without(id)]);
  }

  return (
    <section aria-labelledby={headingId} class="flex flex-col gap-1">
      <div class="flex min-h-11 items-center justify-between gap-2">
        <h2 id={headingId} class="text-sm font-medium">
          Repos
        </h2>
        <Show when={repos().length > 0 && !registering()}>
          <button type="button" class="btn text-sm" onClick={() => setRegistering(true)}>
            Register a repo
          </button>
        </Show>
      </div>
      <Switch>
        <Match when={props.registry.status === "loading"}>
          <p class="text-sm text-muted">Loading repos…</p>
        </Match>
        <Match when={loadError()}>
          {(failure) => (
            <p role="alert" class="flex items-center justify-between gap-2 text-sm">
              <span class="min-w-0">The repos could not be loaded: {failure().message}</span>
              <button type="button" class="btn" onClick={() => props.onRetry()}>
                Retry
              </button>
            </p>
          )}
        </Match>
        <Match when={repos().length === 0}>
          <div class="flex flex-col items-start gap-2 text-sm">
            <p class="font-medium">No repo is registered yet</p>
            <p class="text-muted">
              A repo is a git clone on the server's machine. A session attaches the repos the agent
              should work in, each as its own worktree or edited in place. With none, the agent
              works in an empty folder.
            </p>
            <Show when={!registering()}>
              <button type="button" class="btn" onClick={() => setRegistering(true)}>
                Register a repo
              </button>
            </Show>
          </div>
        </Match>
        <Match when={true}>
          <ul class="divide-y divide-line">
            <For each={repos()} keyed={(repo) => repo.id}>
              {(repo) => (
                <RepoRow
                  repo={repo()}
                  choice={props.choices.find((choice) => choice.repoId === repo().id)}
                  primary={props.choices.at(0)?.repoId === repo().id}
                  onToggle={(chosen) => toggle(repo().id, chosen)}
                  onMode={(mode) => setMode(repo().id, mode)}
                  onMakePrimary={() => makePrimary(repo().id)}
                  onRemove={() => props.onRemove(repo().id)}
                />
              )}
            </For>
          </ul>
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
        </Match>
      </Switch>
      <Show when={registering()}>
        <RegisterRepoForm
          onRegister={props.onRegister}
          onRegistered={(repo) => {
            setRegistering(false);
            toggle(repo.id, true);
          }}
          onCancel={() => setRegistering(false)}
        />
      </Show>
    </section>
  );
}
