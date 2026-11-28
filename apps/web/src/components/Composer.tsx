import {
  createMemo,
  createSignal,
  createUniqueId,
  lazy,
  Loading,
  Show,
  type Accessor,
  type Setter,
} from "solid-js";
import type { IssueQuery } from "../api/http";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { Issue } from "../gen/Issue";
import type { IssueDetail } from "../gen/IssueDetail";
import type { LimitWindow } from "../gen/LimitWindow";
import type { PermissionMode } from "../gen/PermissionMode";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoSet } from "../gen/RepoSet";
import type { RunnerLimits } from "../gen/RunnerLimits";
import type { WorkflowInfo } from "../gen/WorkflowInfo";
import { errorMessage } from "../lib/errors";
import {
  loadComposerPrefs,
  saveComposerPrefs,
  type ComposerPrefs,
  type RepoChoice,
} from "../lib/prefs";
import { createRequest, type Request } from "../lib/request";
import { defaultRepoMode, permissionModes } from "../lib/status";
import { onFormSubmit, onSubmitChord } from "../lib/submit";
import { refusalText } from "../lib/usage";
import { isUsable, QUICK_WORKFLOW, sentWorkflow } from "../lib/workflow";
import { catalogWorkflows, type RepoRegistry, type WorkflowCatalog } from "../state/app";
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

type IssueBody =
  | { status: "loading" }
  | { status: "ready"; text: string }
  | { status: "error"; message: string };

/** The issue a session is to start from, with its body once read. */
type PickedIssue = { repoId: string; issue: Issue; body: IssueBody };

export const DEFAULT_PLACEHOLDER = "Describe what the agent should do";
export const ISSUE_PLACEHOLDER = "Extra instructions (optional)";

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

/** The runner a form starts with: the one chosen while the server still offers it, else its default (the first). */
export const offeredRunner = (wanted: string, runners: readonly string[]): string | undefined =>
  runners.includes(wanted) ? wanted : runners[0];

/** What the form holds when Start is pressed. */
type Draft = {
  /** Trimmed. */
  prompt: string;
  issue: PickedIssue | undefined;
  repos: readonly RepoChoice[];
  permissionMode: PermissionMode;
  runner: string | undefined;
  model: string;
  /** Whether the workflow list was read: without it nothing can be checked, so none is sent. */
  workflowsKnown: boolean;
  workflow: string | undefined;
};

/** The request that starts a session; an issue, a runner, a model and a workflow only when there is one. */
function sessionRequest(draft: Draft): CreateSessionRequest {
  const request: CreateSessionRequest = {
    prompt: draft.prompt,
    permission_mode: draft.permissionMode,
    repos: draft.repos.map((choice) => ({ repo_id: choice.repoId, mode: choice.mode })),
  };
  if (draft.issue)
    request.issue = { repo_id: draft.issue.repoId, number: draft.issue.issue.number };
  if (draft.runner !== undefined) request.runner = draft.runner;
  const model = draft.model.trim();
  if (model !== "") request.model = model;
  if (draft.workflowsKnown && draft.workflow !== undefined) request.workflow = draft.workflow;
  return request;
}

/** The line said in place of an issue's text: while it loads, when it failed, or when it has none. */
function issueBodyNote(body: IssueBody): string | undefined {
  if (body.status === "loading") return "Loading the issue…";
  if (body.status === "error") return `The issue could not be loaded: ${body.message}`;
  return body.text.trim() === "" ? "The issue has no description." : undefined;
}

const issueText = (body: IssueBody): string => (body.status === "ready" ? body.text : "");

type ComposerProps = {
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
};

/**
 * What the composer holds and decides apart from its markup. The choices of
 * the last session started here are the starting point of the next one.
 */
// A class rather than a closure of functions: each method then stands, and is measured, on its own.
// `private`, not `#`: the complexity report does not see `#` methods.
class ComposerState {
  readonly prompt: Accessor<string>;
  readonly setPrompt: Setter<string>;
  // `undefined` until the reader picks: nothing is remembered, so the default applies.
  readonly setChoices: Setter<readonly RepoChoice[] | undefined>;
  readonly permissionMode: Accessor<PermissionMode>;
  readonly setPermissionMode: Setter<PermissionMode>;
  readonly setRunner: Setter<string>;
  readonly model: Accessor<string>;
  readonly setModel: Setter<string>;
  readonly setWorkflow: Setter<string | undefined>;
  readonly picked: Accessor<PickedIssue | undefined>;
  readonly setPicked: Setter<PickedIssue | undefined>;
  readonly issuesOpen: Accessor<boolean>;
  readonly setIssuesOpen: Setter<boolean>;
  readonly pickedWorkflow: Accessor<WorkflowInfo | undefined>;
  /** Nothing is attached until the registry is read. */
  readonly chosen: Accessor<readonly RepoChoice[]>;
  /** The primary repo, whose issues the disclosure lists. */
  readonly primary: Accessor<Repo | undefined>;
  readonly starting: Request<[Draft]>;
  private readonly choices: Accessor<readonly RepoChoice[] | undefined>;
  private readonly runner: Accessor<string>;
  private readonly workflow: Accessor<string | undefined>;
  private readonly props: ComposerProps;

  constructor(props: ComposerProps) {
    this.props = props;
    const initial = loadComposerPrefs();
    [this.prompt, this.setPrompt] = createSignal("");
    [this.choices, this.setChoices] = createSignal<readonly RepoChoice[] | undefined>(
      initial.repos,
    );
    [this.permissionMode, this.setPermissionMode] = createSignal<PermissionMode>(
      initial.permissionMode,
    );
    [this.runner, this.setRunner] = createSignal(initial.runner ?? "");
    [this.model, this.setModel] = createSignal("");
    [this.workflow, this.setWorkflow] = createSignal<string | undefined>(initial.workflow);
    [this.picked, this.setPicked] = createSignal<PickedIssue>();
    [this.issuesOpen, this.setIssuesOpen] = createSignal(false);
    this.pickedWorkflow = createMemo(() => chosenWorkflow(this.workflow(), this.workflowList()));
    this.chosen = createMemo(() => this.attached());
    this.primary = createMemo(() => this.primaryRepo());
    this.starting = createRequest((draft: Draft) => this.start(draft));
  }

  workflowList(): readonly WorkflowInfo[] {
    return catalogWorkflows(this.props.workflows);
  }

  selectedRunner(): string | undefined {
    return offeredRunner(this.runner(), this.props.runners);
  }

  blocked(): boolean {
    return this.props.disabled === true || this.starting.busy();
  }

  refusing(): LimitWindow | undefined {
    return refusingWindow(this.props.limits ?? [], this.selectedRunner());
  }

  now(): number {
    return this.props.now ?? Date.now();
  }

  placeholder(): string {
    return this.picked() ? ISSUE_PLACEHOLDER : DEFAULT_PLACEHOLDER;
  }

  pickIssue(repoId: string, issue: Issue): void {
    this.setPicked({ repoId, issue, body: { status: "loading" } });
    this.setIssuesOpen(false);
    this.props.issues?.get(repoId, issue.number).then(
      (detail) => this.settleBody(repoId, issue.number, { status: "ready", text: detail.body }),
      (reason: unknown) =>
        this.settleBody(repoId, issue.number, { status: "error", message: errorMessage(reason) }),
    );
  }

  async submit(): Promise<void> {
    // With an issue the issue is the task, and what is typed only adds to it.
    const draft = this.draft();
    if ((draft.prompt === "" && !draft.issue) || this.blocked()) return;
    await this.starting.run(draft);
  }

  private attached(): readonly RepoChoice[] {
    const registry = this.props.registry;
    return registry.status === "ready" ? attachedRepos(this.choices(), registry.repos) : [];
  }

  private primaryRepo(): Repo | undefined {
    const first = this.chosen().at(0);
    const registry = this.props.registry;
    if (!first || registry.status !== "ready") return undefined;
    return registry.repos.find((repo) => repo.id === first.repoId);
  }

  /** The body that arrived, unless another issue was picked meanwhile. */
  private settleBody(repoId: string, number: number, body: IssueBody): void {
    this.setPicked((current) =>
      current?.repoId === repoId && current.issue.number === number
        ? { ...current, body }
        : current,
    );
  }

  private draft(): Draft {
    return {
      prompt: this.prompt().trim(),
      issue: this.picked(),
      repos: this.chosen(),
      permissionMode: this.permissionMode(),
      runner: this.selectedRunner(),
      model: this.model(),
      workflowsKnown: this.props.workflows?.status === "ready",
      workflow: sentWorkflow(this.pickedWorkflow()),
    };
  }

  private async start(draft: Draft): Promise<void> {
    await this.props.onCreate(sessionRequest(draft));
    saveComposerPrefs(this.remembered(draft));
    this.setPrompt("");
    this.setPicked(undefined);
  }

  private remembered(draft: Draft): ComposerPrefs {
    // Without a registry nothing could be attached; that is no reason to forget the last choices.
    const repos = this.props.registry.status === "ready" ? draft.repos : this.choices();
    return {
      repos: repos?.slice(),
      permissionMode: draft.permissionMode,
      runner: draft.runner,
      // Without a list, nothing could be checked; the last choice is kept for the next visit.
      workflow: draft.workflowsKnown ? draft.workflow : this.workflow(),
    };
  }
}

/** The picked issue as a removable chip, and its text behind a disclosure. */
function IssueChip(props: { picked: PickedIssue; onRemove: () => void }) {
  return (
    <div class="flex flex-col gap-1">
      <div class="flex min-w-0 items-center gap-1">
        <a
          href={props.picked.issue.url}
          target="_blank"
          rel="noopener noreferrer"
          data-part="issue-chip"
          class="flex min-h-11 min-w-0 items-center gap-1.5 rounded-full border border-accent bg-accent-soft px-3 text-sm"
        >
          <span class="shrink-0 font-mono">#{props.picked.issue.number}</span>
          <span class="truncate">{props.picked.issue.title}</span>
        </a>
        <button
          type="button"
          class="btn btn-ghost shrink-0 px-2"
          aria-label={`Remove issue #${props.picked.issue.number}`}
          onClick={() => props.onRemove()}
        >
          <span aria-hidden="true">✕</span>
        </button>
      </div>
      <details class="group rounded-md border border-line">
        <summary class="flex min-h-11 items-center gap-1.5 px-3 text-sm">
          <span aria-hidden="true" class="text-muted transition-transform group-open:rotate-90">
            ▸
          </span>
          Show issue
        </summary>
        <div class="max-h-[40dvh] overflow-y-auto border-t border-line px-3 py-2 text-sm">
          <Show
            when={issueBodyNote(props.picked.body)}
            fallback={<Markdown text={issueText(props.picked.body)} />}
          >
            {(note) => <p class="text-muted">{note()}</p>}
          </Show>
        </div>
      </details>
    </div>
  );
}

/** The primary repo's issues, listed once the disclosure opens. */
function IssuesDisclosure(props: {
  repo: Repo;
  source: IssueSource;
  open: boolean;
  now: number;
  onToggle: (open: boolean) => void;
  onPick: (issue: Issue) => void;
}) {
  return (
    <details
      class="group"
      open={props.open}
      onToggle={(event) => props.onToggle(event.currentTarget.open)}
    >
      <summary class="flex min-h-11 items-center gap-1.5 text-sm">
        <span aria-hidden="true" class="text-muted transition-transform group-open:rotate-90">
          ▸
        </span>
        <span class="font-medium">Issues</span>
        <span class="truncate text-xs text-muted">{props.repo.name}</span>
      </summary>
      <Show when={props.open}>
        <Loading fallback={<p class="text-sm text-muted">Loading…</p>}>
          <IssueList
            repoId={props.repo.id}
            repoName={props.repo.name}
            now={props.now}
            list={props.source.list}
            onPick={props.onPick}
          />
        </Loading>
      </Show>
    </details>
  );
}

/** Stays in view while the choices above scroll, so a session is always one tap away. */
function StartBar(props: { formId: string; busy: boolean; blocked: boolean; noticeId?: string }) {
  return (
    <div class="sticky bottom-0 flex items-center justify-between gap-3 border-t border-line bg-surface py-3">
      {/* A keyboard shortcut means nothing on a touch screen; there the line only takes room. */}
      <p class="text-xs text-muted pointer-coarse:invisible">
        Ctrl+Enter (⌘+Enter on a Mac) starts the session.
      </p>
      <button
        type="submit"
        form={props.formId}
        class="btn btn-primary"
        disabled={props.blocked}
        aria-describedby={props.noticeId}
      >
        {props.busy ? "Starting…" : "Start"}
      </button>
    </div>
  );
}

/**
 * The form that starts a session. `runners` comes from `GET /health`, default
 * first, `registry` from `GET /repos` and `sets` from `GET /repo-sets`.
 * `onCreate` resolves once the session exists and rejects with the reason. The
 * choices of the last session started here are the starting point of the next one.
 */
export function Composer(props: ComposerProps) {
  const state = new ComposerState(props);
  const formId = createUniqueId();
  const promptId = createUniqueId();
  const errorId = createUniqueId();
  const noticeId = createUniqueId();
  const submit = (): Promise<void> => state.submit();

  return (
    <div class="flex flex-col gap-3">
      <form id={formId} class="flex flex-col gap-2" onSubmit={onFormSubmit(submit)}>
        <label for={promptId} class="text-sm font-medium">
          New session
        </label>
        <Show when={state.picked()}>
          {(picked) => <IssueChip picked={picked()} onRemove={() => state.setPicked(undefined)} />}
        </Show>
        <textarea
          id={promptId}
          class="field min-h-22 resize-y px-3 py-2"
          rows="3"
          placeholder={state.placeholder()}
          value={state.prompt()}
          aria-describedby={state.starting.error() === undefined ? undefined : errorId}
          onInput={(event) => state.setPrompt(event.currentTarget.value)}
          onKeyDown={onSubmitChord(submit)}
        />
        <Show when={state.starting.error()}>
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
        choices={state.chosen()}
        onChange={state.setChoices}
        onRegister={props.onRegisterRepo}
        onRemove={props.onRemoveRepo}
        onClone={props.onCloneRepo}
        onRetry={props.onRetryRepos}
      />

      <Show when={props.issues}>
        {(source) => (
          <Show when={state.primary()}>
            {(repo) => (
              <IssuesDisclosure
                repo={repo()}
                source={source()}
                open={state.issuesOpen()}
                now={state.now()}
                onToggle={state.setIssuesOpen}
                onPick={(issue) => state.pickIssue(repo().id, issue)}
              />
            )}
          </Show>
        )}
      </Show>

      <WorkflowField
        workflows={state.workflowList()}
        picked={state.pickedWorkflow()}
        disabled={props.disabled}
        onChange={state.setWorkflow}
      />

      <PermissionField
        modes={permissionModes}
        value={state.permissionMode()}
        onChange={state.setPermissionMode}
      />

      <RunnerOptions
        runners={props.runners}
        runner={state.selectedRunner()}
        model={state.model()}
        disabled={props.disabled}
        onRunner={state.setRunner}
        onModel={state.setModel}
      />

      <Show when={state.refusing()}>
        {(window) => (
          <p
            id={noticeId}
            role="status"
            class="rounded-md border border-line bg-attention-soft px-3 py-2 text-sm"
          >
            {refusalText(window(), state.now())} A session started now will fail or wait.
          </p>
        )}
      </Show>

      <StartBar
        formId={formId}
        busy={state.starting.busy()}
        blocked={state.blocked()}
        noticeId={state.refusing() ? noticeId : undefined}
      />
    </div>
  );
}
