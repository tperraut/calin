import { createSignal, createUniqueId, For, Match, Show, Switch } from "solid-js";
import type { AttachRepoRequest } from "../gen/AttachRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoMode } from "../gen/RepoMode";
import type { SessionRepo } from "../gen/SessionRepo";
import { createRequest } from "../lib/request";
import { defaultRepoMode, repoModeFrom } from "../lib/status";
import { onFormSubmit } from "../lib/submit";
import type { RepoRegistry } from "../state/app";
import { LoadFailure } from "./AccessSettings";
import { RepoModeOptions } from "./RepoPicker";
import { ErrorNote } from "./ErrorNote";

/** The repos that can still join: cloned, and not attached already (one at most once, api.rs). */
function attachable(registry: RepoRegistry, attached: readonly SessionRepo[]): readonly Repo[] {
  if (registry.status !== "ready") return [];
  return registry.repos.filter(
    (repo) => repo.available && !attached.some((one) => one.repo_id === repo.id),
  );
}

const registryFailure = (registry: RepoRegistry) =>
  registry.status === "error" ? registry : undefined;

/** The two choices: which repo, and how it is attached. */
function AttachFields(props: {
  candidates: readonly Repo[];
  repo: Repo | undefined;
  mode: RepoMode;
  busy: boolean;
  onRepo: (id: string) => void;
  onMode: (mode: RepoMode | undefined) => void;
}) {
  const repoId = createUniqueId();
  const modeId = createUniqueId();
  return (
    <div class="flex flex-wrap gap-2">
      <label for={repoId} class="flex min-w-0 flex-1 basis-40 flex-col gap-1 text-sm">
        Repo to attach
        <select
          id={repoId}
          class="field h-11 px-2"
          disabled={props.busy}
          value={props.repo?.id}
          onChange={(event) => props.onRepo(event.currentTarget.value)}
        >
          <For each={props.candidates} keyed={(one) => one.id}>
            {(one) => (
              <option value={one().id}>
                {one().name}
                {one().home ? " (home)" : ""}
              </option>
            )}
          </For>
        </select>
      </label>
      <label for={modeId} class="flex min-w-0 flex-1 basis-40 flex-col gap-1 text-sm">
        Attached as
        <select
          id={modeId}
          class="field h-11 px-2"
          disabled={props.busy}
          value={props.mode}
          onChange={(event) => props.onMode(repoModeFrom(event.currentTarget.value))}
        >
          <RepoModeOptions />
        </select>
      </label>
    </div>
  );
}

/**
 * Picks a repo to add to a session under way, and how. Nothing shows as
 * attached here: `onAttach` resolves once the server did it, and the repo
 * appears with its `repo_attached` event. A refusal shows the server's reason.
 */
export function AttachRepo(props: {
  registry: RepoRegistry;
  /** The session's repos: one is attached at most once (api.rs). */
  attached: readonly SessionRepo[];
  /** False once the session left the statuses in which a repo can join. */
  enabled: boolean;
  onAttach: (request: AttachRepoRequest) => Promise<void>;
  onRetry: () => void;
  onClose: () => void;
}) {
  const [picked, setPicked] = createSignal<string>();
  // Until the reader chooses, the mode is the default of the repo picked.
  const [modeChoice, setModeChoice] = createSignal<RepoMode>();

  const candidates = (): readonly Repo[] => attachable(props.registry, props.attached);
  const repo = (): Repo | undefined =>
    candidates().find((one) => one.id === picked()) ?? candidates().at(0);
  const mode = (): RepoMode => {
    const chosen = repo();
    return modeChoice() ?? (chosen ? defaultRepoMode(chosen) : "write");
  };

  const attaching = createRequest(async (chosen: Repo) => {
    await props.onAttach({ repo_id: chosen.id, mode: mode() });
    props.onClose();
  });
  async function attach(): Promise<void> {
    const chosen = repo();
    if (chosen && props.enabled) await attaching.run(chosen);
  }
  function pickRepo(id: string): void {
    setPicked(id);
    setModeChoice(undefined);
  }

  return (
    <form
      aria-label="Attach a repo"
      class="flex flex-col gap-2 border-b border-line bg-attention-soft p-3"
      onSubmit={onFormSubmit(attach)}
    >
      <Switch>
        <Match when={props.registry.status === "loading"}>
          <p class="text-sm text-muted">Loading repos…</p>
        </Match>
        <Match when={registryFailure(props.registry)}>
          {(failure) => (
            <LoadFailure
              what="The repos"
              message={failure().message}
              onRetry={() => props.onRetry()}
            />
          )}
        </Match>
        <Match when={candidates().length === 0}>
          <p class="text-sm">
            Every repo that is cloned on this machine is already attached to this session.
          </p>
        </Match>
        <Match when={true}>
          <AttachFields
            candidates={candidates()}
            repo={repo()}
            mode={mode()}
            busy={attaching.busy()}
            onRepo={pickRepo}
            onMode={setModeChoice}
          />
        </Match>
      </Switch>
      <ErrorNote message={attaching.error()} />
      <Show when={!props.enabled}>
        <p class="text-sm text-muted">
          A repo can be attached while the session is running, waiting or in review.
        </p>
      </Show>
      <div class="flex justify-end gap-2">
        <button type="button" class="btn" onClick={() => props.onClose()}>
          Cancel
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          disabled={attaching.busy() || !props.enabled || repo() === undefined}
        >
          {attaching.busy() ? "Attaching…" : "Attach"}
        </button>
      </div>
    </form>
  );
}
