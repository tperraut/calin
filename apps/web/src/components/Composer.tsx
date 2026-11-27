import { createMemo, createSignal, createUniqueId, lazy, Loading, Show } from "solid-js";
import type { IssueQuery } from "../api/http";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { Issue } from "../gen/Issue";
import type { IssueDetail } from "../gen/IssueDetail";
import type { PermissionMode } from "../gen/PermissionMode";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoSet } from "../gen/RepoSet";
import type { RunnerLimits } from "../gen/RunnerLimits";
import type { WorkflowInfo } from "../gen/WorkflowInfo";
import { errorMessage } from "../lib/errors";
import { loadComposerPrefs, saveComposerPrefs, type RepoChoice } from "../lib/prefs";
import { defaultRepoMode, permissionModes } from "../lib/status";
import { refusalText } from "../lib/usage";
import { isUsable, QUICK_WORKFLOW } from "../lib/workflow";
import type { RepoRegistry, WorkflowCatalog } from "../state/app";
import { refusingWindow } from "../state/limits";
import { Markdown } from "./Markdown";
import { RepoPicker } from "./RepoPicker";
import { PermissionField, RunnerOptions, WorkflowField } from "./SessionOptions";

// Only someone who opens the Issues disclosure needs the list, so it is fetched then.
const IssueList = lazy(() => import("./IssueList"));

/** Where the composer reads a repo's GitHub issues; absent when GitHub cannot be used. */
export type IssueSource = {
  list: (repoId: string, query: IssueQuery) => Promise<Issue[]>;
  get: (repoId: string, number: number) => Promise<IssueDetail>;
};

/** The issue a session is to start from, with its body once read. */
type PickedIssue = {
  repoId: string;
  issue: Issue;
  body:
    | { status: "loading" }
    | { status: "ready"; text: string }
    | { status: "error"; message: string };
};

export const DEFAULT_PLACEHOLDER = "Describe what the agent should do";
export const ISSUE_PLACEHOLDER = "Extra instructions (optional)";

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
  /** Where issues are read; absent when GitHub cannot be used, and no Issues disclosure shows. */
  issues?: IssueSource;
  /** The newest limits per runner: a window refusing requests is said above Start. */
  limits?: readonly RunnerLimits[];
  /** The app's clock, for relative and reset times. */
  now?: number;
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
  const [picked, setPicked] = createSignal<PickedIssue>();
  const [issuesOpen, setIssuesOpen] = createSignal(false);
  const formId = createUniqueId();
  const promptId = createUniqueId();
  const errorId = createUniqueId();
  const noticeId = createUniqueId();

  const workflowList = (): readonly WorkflowInfo[] =>
    props.workflows?.status === "ready" ? props.workflows.workflows : [];
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

  /** The primary repo, whose issues the disclosure lists. */
  const primary = createMemo(() => {
    const first = chosen().at(0);
    if (!first || props.registry.status !== "ready") return undefined;
    return props.registry.repos.find((repo) => repo.id === first.repoId);
  });
  const refusing = () => refusingWindow(props.limits ?? [], selectedRunner());

  const bodyText = (): string | undefined => {
    const body = picked()?.body;
    return body?.status === "ready" ? body.text : undefined;
  };
  const bodyError = (): string | undefined => {
    const body = picked()?.body;
    return body?.status === "error" ? body.message : undefined;
  };

  function pickIssue(repoId: string, issue: Issue): void {
    setPicked({ repoId, issue, body: { status: "loading" } });
    setIssuesOpen(false);
    const source = props.issues;
    if (!source) return;
    const settle = (body: PickedIssue["body"]): void => {
      setPicked((current) =>
        current?.repoId === repoId && current.issue.number === issue.number
          ? { ...current, body }
          : current,
      );
    };
    source.get(repoId, issue.number).then(
      (detail) => settle({ status: "ready", text: detail.body }),
      (reason: unknown) => settle({ status: "error", message: errorMessage(reason) }),
    );
  }

  async function submit(): Promise<void> {
    const text = prompt().trim();
    const issue = picked();
    // With an issue the issue is the task, and what is typed only adds to it.
    if ((text === "" && !issue) || blocked()) return;
    const repos = chosen();
    const mode = permissionMode();
    const request: CreateSessionRequest = {
      prompt: text,
      permission_mode: mode,
      repos: repos.map((choice) => ({ repo_id: choice.repoId, mode: choice.mode })),
    };
    if (issue) request.issue = { repo_id: issue.repoId, number: issue.issue.number };
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
      setPicked(undefined);
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
        <Show when={picked()}>
          {(chosenIssue) => (
            <div class="flex flex-col gap-1">
              <div class="flex min-w-0 items-center gap-1">
                <a
                  href={chosenIssue().issue.url}
                  target="_blank"
                  rel="noopener noreferrer"
                  data-part="issue-chip"
                  class="flex min-h-11 min-w-0 items-center gap-1.5 rounded-full border border-accent bg-accent-soft px-3 text-sm"
                >
                  <span class="shrink-0 font-mono">#{chosenIssue().issue.number}</span>
                  <span class="truncate">{chosenIssue().issue.title}</span>
                </a>
                <button
                  type="button"
                  class="btn btn-ghost shrink-0 px-2"
                  aria-label={`Remove issue #${chosenIssue().issue.number}`}
                  onClick={() => setPicked(undefined)}
                >
                  <span aria-hidden="true">✕</span>
                </button>
              </div>
              <details class="group rounded-md border border-line">
                <summary class="flex min-h-11 items-center gap-1.5 px-3 text-sm">
                  <span
                    aria-hidden="true"
                    class="text-muted transition-transform group-open:rotate-90"
                  >
                    ▸
                  </span>
                  Show issue
                </summary>
                <div class="max-h-[40dvh] overflow-y-auto border-t border-line px-3 py-2 text-sm">
                  <Show
                    when={bodyText() !== undefined}
                    fallback={
                      <p class="text-muted">
                        {bodyError() === undefined
                          ? "Loading the issue…"
                          : `The issue could not be loaded: ${bodyError()}`}
                      </p>
                    }
                  >
                    <Show
                      when={(bodyText() ?? "").trim() !== ""}
                      fallback={<p class="text-muted">The issue has no description.</p>}
                    >
                      <Markdown text={bodyText() ?? ""} />
                    </Show>
                  </Show>
                </div>
              </details>
            </div>
          )}
        </Show>
        <textarea
          id={promptId}
          class="field min-h-22 resize-y px-3 py-2"
          rows="3"
          placeholder={picked() ? ISSUE_PLACEHOLDER : DEFAULT_PLACEHOLDER}
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

      <Show when={props.issues && primary()}>
        {(repo) => (
          <details
            class="group"
            open={issuesOpen()}
            onToggle={(event) => setIssuesOpen(event.currentTarget.open)}
          >
            <summary class="flex min-h-11 items-center gap-1.5 text-sm">
              <span aria-hidden="true" class="text-muted transition-transform group-open:rotate-90">
                ▸
              </span>
              <span class="font-medium">Issues</span>
              <span class="truncate text-xs text-muted">{repo().name}</span>
            </summary>
            <Show when={issuesOpen() && props.issues}>
              {(source) => (
                <Loading fallback={<p class="text-sm text-muted">Loading…</p>}>
                  <IssueList
                    repoId={repo().id}
                    repoName={repo().name}
                    now={props.now ?? Date.now()}
                    list={source().list}
                    onPick={(issue) => pickIssue(repo().id, issue)}
                  />
                </Loading>
              )}
            </Show>
          </details>
        )}
      </Show>

      <WorkflowField
        workflows={workflowList()}
        picked={pickedWorkflow()}
        disabled={props.disabled}
        onChange={setWorkflow}
      />

      <PermissionField
        modes={permissionModes}
        value={permissionMode()}
        onChange={setPermissionMode}
      />

      <RunnerOptions
        runners={props.runners}
        runner={selectedRunner()}
        model={model()}
        disabled={props.disabled}
        onRunner={setRunner}
        onModel={setModel}
      />

      <Show when={refusing()}>
        {(window) => (
          <p
            id={noticeId}
            role="status"
            class="rounded-md border border-line bg-attention-soft px-3 py-2 text-sm"
          >
            {refusalText(window(), props.now ?? Date.now())} A session started now will fail or
            wait.
          </p>
        )}
      </Show>

      {/* Stays in view while the choices above scroll, so a session is always one tap away. */}
      <div class="sticky bottom-0 flex items-center justify-between gap-3 border-t border-line bg-surface py-3">
        {/* A keyboard shortcut means nothing on a touch screen; there the line only takes room. */}
        <p class="text-xs text-muted pointer-coarse:invisible">
          Ctrl+Enter (⌘+Enter on a Mac) starts the session.
        </p>
        <button
          type="submit"
          form={formId}
          class="btn btn-primary"
          disabled={blocked()}
          aria-describedby={refusing() ? noticeId : undefined}
        >
          {busy() ? "Starting…" : "Start"}
        </button>
      </div>
    </div>
  );
}
