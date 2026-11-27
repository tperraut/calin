import { createSignal, createUniqueId, For, Show } from "solid-js";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { PermissionMode } from "../gen/PermissionMode";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import { errorMessage } from "../lib/errors";
import { loadComposerPrefs, saveComposerPrefs, type RepoChoice } from "../lib/prefs";
import { permissionModeLook, permissionModes } from "../lib/status";
import type { RepoRegistry } from "../state/app";
import { RepoPicker } from "./RepoPicker";

/** True for the chord that submits a multi-line field: Ctrl+Enter, or Cmd+Enter on a Mac. */
export const isSubmitChord = (event: KeyboardEvent): boolean =>
  event.key === "Enter" && (event.ctrlKey || event.metaKey);

/**
 * The form that starts a session. `runners` comes from `GET /health`, default
 * first, and `registry` from `GET /repos`. `onCreate` resolves once the session
 * exists and rejects with the reason. The choices of the last session started
 * here are the starting point of the next one.
 */
export function Composer(props: {
  runners: readonly string[];
  registry: RepoRegistry;
  disabled?: boolean;
  onCreate: (request: CreateSessionRequest) => Promise<void>;
  onRegisterRepo: (request: RegisterRepoRequest) => Promise<Repo>;
  onRemoveRepo: (id: string) => Promise<void>;
  onRetryRepos: () => void;
}) {
  const initial = loadComposerPrefs();
  const [prompt, setPrompt] = createSignal("");
  const [choices, setChoices] = createSignal<readonly RepoChoice[]>(initial.repos);
  const [permissionMode, setPermissionMode] = createSignal<PermissionMode>(initial.permissionMode);
  const [runner, setRunner] = createSignal(initial.runner ?? "");
  const [model, setModel] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const formId = createUniqueId();
  const promptId = createUniqueId();
  const runnerId = createUniqueId();
  const modelId = createUniqueId();
  const errorId = createUniqueId();
  const modeGroup = createUniqueId();

  // A remembered runner the server no longer offers gives way to its default (the first).
  const selectedRunner = (): string | undefined =>
    props.runners.includes(runner()) ? runner() : props.runners[0];
  // A remembered repo that left the registry is not attached; nothing is, until the registry is read.
  const chosen = (): readonly RepoChoice[] => {
    const registry = props.registry;
    if (registry.status !== "ready") return [];
    return choices().filter((choice) => registry.repos.some((repo) => repo.id === choice.repoId));
  };
  const blocked = (): boolean => props.disabled === true || busy();

  async function submit(): Promise<void> {
    const text = prompt().trim();
    if (text === "" || blocked()) return;
    const repos = chosen();
    const mode = permissionMode();
    const request: CreateSessionRequest = {
      prompt: text,
      permission_mode: mode,
      repos: repos.map((choice) => ({ repo_id: choice.repoId, mode: choice.mode })),
    };
    const usedRunner = selectedRunner();
    if (usedRunner !== undefined) request.runner = usedRunner;
    if (model().trim() !== "") request.model = model().trim();
    setBusy(true);
    setError(undefined);
    try {
      await props.onCreate(request);
      // Without a registry nothing could be attached; that is no reason to forget the last choices.
      const remembered = props.registry.status === "ready" ? repos : choices();
      saveComposerPrefs({ repos: remembered.slice(), permissionMode: mode, runner: usedRunner });
      setPrompt("");
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="flex flex-col gap-3">
      <form
        id={formId}
        class="flex flex-col gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        <label for={promptId} class="text-sm font-medium">
          New session
        </label>
        <textarea
          id={promptId}
          class="field min-h-22 resize-y px-3 py-2"
          rows="3"
          placeholder="Describe what the agent should do"
          value={prompt()}
          aria-describedby={error() === undefined ? undefined : errorId}
          onInput={(event) => setPrompt(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (!isSubmitChord(event)) return;
            event.preventDefault();
            void submit();
          }}
        />
        <Show when={error()}>
          {(message) => (
            <p id={errorId} role="alert" class="text-sm text-tone-danger">
              {message()}
            </p>
          )}
        </Show>
      </form>

      {/* Outside the form above: registering a repo is a form of its own, and forms do not nest. */}
      <RepoPicker
        registry={props.registry}
        choices={chosen()}
        onChange={setChoices}
        onRegister={props.onRegisterRepo}
        onRemove={props.onRemoveRepo}
        onRetry={props.onRetryRepos}
      />

      <fieldset class="flex flex-col gap-1.5">
        <legend class="mb-1.5 text-sm font-medium">Permissions</legend>
        <For each={permissionModes}>
          {(mode) => (
            <label class="flex min-h-11 cursor-pointer items-start gap-3 rounded-md border border-line px-3 py-2 has-checked:border-accent has-checked:bg-accent-soft">
              <input
                type="radio"
                name={modeGroup}
                value={mode}
                class="mt-0.5 size-5 shrink-0 accent-accent"
                checked={permissionMode() === mode}
                onChange={() => setPermissionMode(mode)}
              />
              <span class="min-w-0">
                <span class="block font-medium">{permissionModeLook(mode).label}</span>
                <span class="block text-xs text-muted">{permissionModeLook(mode).hint}</span>
              </span>
            </label>
          )}
        </For>
      </fieldset>

      <details class="group">
        <summary class="flex min-h-11 items-center gap-1.5 text-sm">
          <span aria-hidden="true" class="text-muted transition-transform group-open:rotate-90">
            ▸
          </span>
          <span class="font-medium">Options</span>
          <span class="truncate font-mono text-xs text-muted group-open:hidden">
            {selectedRunner()}
            {model().trim() === "" ? "" : ` · ${model().trim()}`}
          </span>
        </summary>
        <div class="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-2 pb-1 lg:max-w-md">
          <label for={runnerId} class="text-sm text-muted">
            Runner
          </label>
          <select
            id={runnerId}
            class="field h-11 px-2 font-mono"
            disabled={props.disabled === true || props.runners.length === 0}
            onChange={(event) => setRunner(event.currentTarget.value)}
          >
            <For each={props.runners}>
              {(name) => (
                <option value={name} selected={name === selectedRunner()}>
                  {name}
                </option>
              )}
            </For>
          </select>
          <label for={modelId} class="text-sm text-muted">
            Model
          </label>
          <input
            id={modelId}
            type="text"
            class="field h-11 px-3 font-mono"
            placeholder="runner default"
            autocomplete="off"
            autocapitalize="off"
            spellcheck={false}
            value={model()}
            onInput={(event) => setModel(event.currentTarget.value)}
          />
        </div>
      </details>

      {/* Stays in view while the choices above scroll, so a session is always one tap away. */}
      <div class="sticky bottom-0 flex items-center justify-between gap-3 border-t border-line bg-surface py-3">
        <p class="text-xs text-muted">Ctrl+Enter (⌘+Enter on a Mac) starts the session.</p>
        <button type="submit" form={formId} class="btn btn-primary" disabled={blocked()}>
          {busy() ? "Starting…" : "Start"}
        </button>
      </div>
    </div>
  );
}
