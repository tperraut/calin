import { createSignal, createUniqueId, For, Match, Show, Switch } from "solid-js";
import type { AttachRepoRequest } from "../gen/AttachRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoMode } from "../gen/RepoMode";
import type { SessionRepo } from "../gen/SessionRepo";
import { errorMessage } from "../lib/errors";
import { defaultRepoMode, repoModeLabel, repoModes } from "../lib/status";
import type { RepoRegistry } from "../state/app";

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
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const repoId = createUniqueId();
  const modeId = createUniqueId();

  const loadError = () => (props.registry.status === "error" ? props.registry : undefined);
  const candidates = (): readonly Repo[] =>
    props.registry.status === "ready"
      ? props.registry.repos.filter(
          (repo) => repo.available && !props.attached.some((one) => one.repo_id === repo.id),
        )
      : [];
  const repo = (): Repo | undefined =>
    candidates().find((one) => one.id === picked()) ?? candidates().at(0);
  const mode = (): RepoMode => {
    const chosen = repo();
    return modeChoice() ?? (chosen ? defaultRepoMode(chosen) : "write");
  };

  async function attach(): Promise<void> {
    const chosen = repo();
    if (!chosen || busy() || !props.enabled) return;
    setBusy(true);
    setError(undefined);
    try {
      await props.onAttach({ repo_id: chosen.id, mode: mode() });
      props.onClose();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      aria-label="Attach a repo"
      class="flex flex-col gap-2 border-b border-line bg-attention-soft p-3"
      onSubmit={(event) => {
        event.preventDefault();
        void attach();
      }}
    >
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
        <Match when={candidates().length === 0}>
          <p class="text-sm">
            Every repo that is cloned on this machine is already attached to this session.
          </p>
        </Match>
        <Match when={true}>
          <div class="flex flex-wrap gap-2">
            <label for={repoId} class="flex min-w-0 flex-1 basis-40 flex-col gap-1 text-sm">
              Repo to attach
              <select
                id={repoId}
                class="field h-11 px-2"
                disabled={busy()}
                value={repo()?.id}
                onChange={(event) => {
                  setPicked(event.currentTarget.value);
                  setModeChoice(undefined);
                }}
              >
                <For each={candidates()} keyed={(one) => one.id}>
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
                disabled={busy()}
                value={mode()}
                onChange={(event) =>
                  setModeChoice(repoModes.find((one) => one === event.currentTarget.value))
                }
              >
                <For each={repoModes}>
                  {(one) => <option value={one}>{repoModeLabel(one)}</option>}
                </For>
              </select>
            </label>
          </div>
        </Match>
      </Switch>
      <Show when={error()}>
        {(message) => (
          <p role="alert" class="text-sm text-tone-danger">
            {message()}
          </p>
        )}
      </Show>
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
          disabled={busy() || !props.enabled || repo() === undefined}
        >
          {busy() ? "Attaching…" : "Attach"}
        </button>
      </div>
    </form>
  );
}
