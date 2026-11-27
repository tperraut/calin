import { createMemo, createSignal, createUniqueId, For, Show } from "solid-js";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { PermissionMode } from "../gen/PermissionMode";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoSet } from "../gen/RepoSet";
import type { WorkflowInfo } from "../gen/WorkflowInfo";
import { errorMessage } from "../lib/errors";
import { loadComposerPrefs, saveComposerPrefs, type RepoChoice } from "../lib/prefs";
import { defaultRepoMode, permissionModeLook, permissionModes } from "../lib/status";
import { dollars, isUsable, QUICK_WORKFLOW, shapeSummary } from "../lib/workflow";
import type { RepoRegistry, WorkflowCatalog } from "../state/app";
import { RepoPicker } from "./RepoPicker";

/** True for the chord that submits a multi-line field: Ctrl+Enter, or Cmd+Enter on a Mac. */
export const isSubmitChord = (event: KeyboardEvent): boolean =>
  event.key === "Enter" && (event.ctrlKey || event.metaKey);

/**
 * What is attached given what the reader picked or the last session remembered.
 * Repos that left the catalog or are not cloned are dropped. With nothing
 * picked, or nothing left of it, a session starts in the home repo, edited in
 * place (design spec, section 13); an empty list the reader made stays empty.
 */
export function attachedRepos(
  picked: readonly RepoChoice[] | undefined,
  repos: readonly Repo[],
): readonly RepoChoice[] {
  const usable = (picked ?? []).filter((choice) =>
    repos.some((repo) => repo.id === choice.repoId && repo.available),
  );
  if (picked !== undefined && (picked.length === 0 || usable.length > 0)) return usable;
  const home = repos.find((repo) => repo.home && repo.available);
  return home ? [{ repoId: home.id, mode: defaultRepoMode(home) }] : [];
}

/**
 * The workflow a new session runs: the one chosen, while it still exists and
 * validates; the built-in `quick` otherwise (workflows spec, section 1).
 */
export function chosenWorkflow(
  wanted: string | undefined,
  workflows: readonly WorkflowInfo[],
): WorkflowInfo | undefined {
  const usable = (id: string | undefined): WorkflowInfo | undefined =>
    workflows.find((one) => one.id === id && isUsable(one));
  return usable(wanted) ?? usable(QUICK_WORKFLOW);
}

/**
 * The form that starts a session. `runners` comes from `GET /health`, default
 * first, `registry` from `GET /repos` and `sets` from `GET /repo-sets`.
 * `onCreate` resolves once the session exists and rejects with the reason. The
 * choices of the last session started here are the starting point of the next one.
 */
export function Composer(props: {
  runners: readonly string[];
  registry: RepoRegistry;
  sets: readonly RepoSet[];
  /** `GET /workflows`; absent where no workflow can be chosen. */
  workflows?: WorkflowCatalog;
  disabled?: boolean;
  onCreate: (request: CreateSessionRequest) => Promise<void>;
  onRegisterRepo: (request: RegisterRepoRequest) => Promise<Repo>;
  onRemoveRepo: (id: string) => Promise<void>;
  onCloneRepo: (id: string) => Promise<void>;
  onRetryRepos: () => void;
}) {
  const initial = loadComposerPrefs();
  const [prompt, setPrompt] = createSignal("");
  // `undefined` until the reader picks: nothing is remembered, so the default applies.
  const [choices, setChoices] = createSignal<readonly RepoChoice[] | undefined>(initial.repos);
  const [permissionMode, setPermissionMode] = createSignal<PermissionMode>(initial.permissionMode);
  const [runner, setRunner] = createSignal(initial.runner ?? "");
  const [model, setModel] = createSignal("");
  const [workflow, setWorkflow] = createSignal<string | undefined>(initial.workflow);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const formId = createUniqueId();
  const promptId = createUniqueId();
  const runnerId = createUniqueId();
  const modelId = createUniqueId();
  const errorId = createUniqueId();
  const modeGroup = createUniqueId();
  const workflowId = createUniqueId();
  const workflowNoteId = createUniqueId();

  const workflowList = (): readonly WorkflowInfo[] =>
    props.workflows?.status === "ready" ? props.workflows.workflows : [];
  // Only worth a control when there is something besides the built-in one.
  const offersWorkflows = (): boolean => workflowList().some((one) => one.id !== QUICK_WORKFLOW);
  const pickedWorkflow = createMemo(() => chosenWorkflow(workflow(), workflowList()));
  /** The id a session is started with; `quick` is said by saying nothing. */
  const workflowToSend = (): string | undefined => {
    const id = pickedWorkflow()?.id;
    return id === undefined || id === QUICK_WORKFLOW ? undefined : id;
  };

  // A remembered runner the server no longer offers gives way to its default (the first).
  const selectedRunner = (): string | undefined =>
    props.runners.includes(runner()) ? runner() : props.runners[0];
  // Nothing is attached until the registry is read.
  const chosen = createMemo((): readonly RepoChoice[] =>
    props.registry.status === "ready" ? attachedRepos(choices(), props.registry.repos) : [],
  );
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
    // Without a list, nothing can be checked; the last choice is kept for the next visit.
    const known = props.workflows?.status === "ready";
    const usedWorkflow = known ? workflowToSend() : undefined;
    if (usedWorkflow !== undefined) request.workflow = usedWorkflow;
    setBusy(true);
    setError(undefined);
    try {
      await props.onCreate(request);
      // Without a registry nothing could be attached; that is no reason to forget the last choices.
      const remembered = props.registry.status === "ready" ? repos : choices();
      saveComposerPrefs({
        repos: remembered?.slice(),
        permissionMode: mode,
        runner: usedRunner,
        workflow: known ? usedWorkflow : workflow(),
      });
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
        sets={props.sets}
        choices={chosen()}
        onChange={setChoices}
        onRegister={props.onRegisterRepo}
        onRemove={props.onRemoveRepo}
        onClone={props.onCloneRepo}
        onRetry={props.onRetryRepos}
      />

      <Show when={offersWorkflows()}>
        <div class="flex flex-col gap-1.5">
          <label for={workflowId} class="text-sm font-medium">
            Workflow
          </label>
          <select
            id={workflowId}
            class="field h-11 px-2"
            aria-describedby={workflowNoteId}
            disabled={props.disabled === true}
            onChange={(event) => setWorkflow(event.currentTarget.value)}
          >
            <For each={workflowList()} keyed={(one) => one.id}>
              {(one) => (
                <option
                  value={one().id}
                  disabled={!isUsable(one())}
                  selected={one().id === pickedWorkflow()?.id}
                >
                  {one().id === QUICK_WORKFLOW
                    ? "Quick (no workflow)"
                    : isUsable(one())
                      ? one().name
                      : `${one().name} (has errors)`}
                </option>
              )}
            </For>
          </select>
          <div id={workflowNoteId} class="flex flex-col gap-0.5 text-xs text-muted">
            <Show when={pickedWorkflow()}>
              {(picked) => (
                <>
                  <Show when={picked().description}>
                    {(description) => <p class="wrap-break-word">{description()}</p>}
                  </Show>
                  <Show when={picked().id !== QUICK_WORKFLOW && picked().graph}>
                    {(graph) => <p data-part="workflow-shape">{shapeSummary(graph())}</p>}
                  </Show>
                  <Show when={picked().budget_usd}>
                    {(budget) => <p>Budget: {dollars(budget())}</p>}
                  </Show>
                </>
              )}
            </Show>
          </div>
        </div>
      </Show>

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
