import { useNavigate, useParams } from "@solidjs/router";
import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  For,
  Match,
  onSettled,
  Show,
  Switch,
  useContext,
} from "solid-js";
import { HttpError } from "../api/http";
import { SessionStatusBadge } from "../components/Badge";
import { chosenWorkflow } from "../components/Composer";
import { RepoPicker } from "../components/RepoPicker";
import { PermissionField, RunnerOptions, WorkflowField } from "../components/SessionOptions";
import type { Automation } from "../gen/Automation";
import type { AutomationRequest } from "../gen/AutomationRequest";
import type { AutomationRun } from "../gen/AutomationRun";
import type { AutomationTrigger } from "../gen/AutomationTrigger";
import type { CronPreview } from "../gen/CronPreview";
import type { PermissionMode } from "../gen/PermissionMode";
import type { Repo } from "../gen/Repo";
import type { WorkflowInfo } from "../gen/WorkflowInfo";
import {
  automationModes,
  blameField,
  causeText,
  cronPresets,
  outcomeText,
  placeholders,
  runNumberLabel,
  triggerKinds,
  type BlamedField,
  type TriggerKind,
} from "../lib/automations";
import { errorMessage } from "../lib/errors";
import { absoluteTime, listInWords, relativeTime } from "../lib/format";
import type { RepoChoice } from "../lib/prefs";
import { QUICK_WORKFLOW } from "../lib/workflow";
import { sessionPath } from "../panels/registry";
import { AppContext } from "../state/app";

/** How long typing in the cron field must pause before the server is asked what it means. */
export const CRON_PREVIEW_DELAY_MS = 400;

const fieldError = "text-sm wrap-break-word text-tone-danger";

type Preview =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready"; preview: CronPreview }
  | { status: "error"; message: string };

const weekdayTime = (ts: number): string =>
  new Date(ts).toLocaleString(undefined, {
    weekday: "long",
    day: "numeric",
    month: "long",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  });

/** What a session started by an automation will be given, said for the kind of trigger. */
function placeholderNote(kind: TriggerKind): string {
  const [date, ...item] = placeholders(kind);
  if (item.length === 0) return `You can use ${date}, the local date.`;
  const what = kind === "issue" ? "the issue" : "the pull request";
  return `You can use ${date}, the local date, and ${listInWords(item)}, which describe ${what}.`;
}

/** The new-or-edit form. `stored` is absent for a new automation. */
function AutomationEditor(props: {
  stored: Automation | undefined;
  onSaved: (automation: Automation, created: boolean) => void;
}) {
  const app = useContext(AppContext);
  const navigate = useNavigate();
  const stored = props.stored;
  const [name, setName] = createSignal(stored?.name ?? "");
  const [enabled, setEnabled] = createSignal(stored?.enabled ?? false);
  const [kind, setKind] = createSignal<TriggerKind>(stored?.trigger.kind ?? "schedule");
  const [cron, setCron] = createSignal(
    stored?.trigger.kind === "schedule" ? stored.trigger.cron : "0 8 * * 1-5",
  );
  const [triggerRepo, setTriggerRepo] = createSignal(
    stored && stored.trigger.kind !== "schedule" ? stored.trigger.repo_id : "",
  );
  const [label, setLabel] = createSignal(
    stored?.trigger.kind === "issue" ? (stored.trigger.label ?? "") : "",
  );
  const [prompt, setPrompt] = createSignal(stored?.session.prompt ?? "");
  const [choices, setChoices] = createSignal<readonly RepoChoice[]>(
    (stored?.session.repos ?? []).map((repo) => ({
      repoId: repo.repo_id,
      // Absent means a worktree (repo.rs); every other mode is kept as it was saved.
      mode: repo.mode ?? "write",
    })),
  );
  const [workflow, setWorkflow] = createSignal<string | undefined>(
    stored?.session.workflow ?? undefined,
  );
  const [runner, setRunner] = createSignal(stored?.session.runner ?? "");
  const [model, setModel] = createSignal(stored?.session.model ?? "");
  const [mode, setMode] = createSignal<PermissionMode>(
    stored && automationModes.includes(stored.session.permission_mode)
      ? stored.session.permission_mode
      : "accept_edits",
  );
  const [busy, setBusy] = createSignal(false);
  const [saved, setSaved] = createSignal(false);
  const [failure, setFailure] = createSignal<{ message: string; field?: BlamedField }>();
  const [preview, setPreview] = createSignal<Preview>({ status: "idle" });
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  const [deleting, setDeleting] = createSignal(false);
  const [deleteError, setDeleteError] = createSignal<string>();

  const nameId = createUniqueId();
  const cronId = createUniqueId();
  const cronNoteId = createUniqueId();
  const triggerRepoId = createUniqueId();
  const labelId = createUniqueId();
  const promptId = createUniqueId();
  const promptNoteId = createUniqueId();
  const enabledId = createUniqueId();
  const errorId = createUniqueId();
  const kindGroup = createUniqueId();

  const refs: Partial<Record<BlamedField, HTMLElement>> = {};
  let errorBox: HTMLElement | undefined;

  const runners = (): readonly string[] => {
    const health = app.health();
    return health.status === "ready" ? health.health.runners : [];
  };
  const selectedRunner = (): string | undefined =>
    runners().includes(runner()) ? runner() : runners()[0];
  const repos = (): readonly Repo[] => {
    const registry = app.repos();
    return registry.status === "ready" ? registry.repos : [];
  };
  const workflowList = (): readonly WorkflowInfo[] => {
    const state = app.workflows();
    return state.status === "ready" ? state.workflows : [];
  };
  const pickedWorkflow = createMemo(() => chosenWorkflow(workflow(), workflowList()));
  /** The repo a trigger watches: the chosen one, else the first that can be used. */
  const watchedRepo = (): string =>
    triggerRepo() !== ""
      ? triggerRepo()
      : (repos().find((repo) => repo.available)?.id ?? repos()[0]?.id ?? "");

  // The server says what an expression means, once typing pauses.
  let previewTimer: ReturnType<typeof setTimeout> | undefined;
  let previewAsked = 0;
  createEffect(
    () => (kind() === "schedule" ? cron().trim() : ""),
    (expr) => {
      clearTimeout(previewTimer);
      previewAsked += 1;
      if (expr === "") {
        setPreview({ status: "idle" });
        return;
      }
      const mine = previewAsked;
      setPreview({ status: "loading" });
      previewTimer = setTimeout(() => {
        app.previewCron(expr).then(
          (answer) => {
            if (mine === previewAsked) setPreview({ status: "ready", preview: answer });
          },
          (error: unknown) => {
            if (mine === previewAsked)
              setPreview({ status: "error", message: errorMessage(error) });
          },
        );
      }, CRON_PREVIEW_DELAY_MS);
    },
  );
  onSettled(() => () => clearTimeout(previewTimer));

  const previewError = (): string | undefined => {
    const state = preview();
    return state.status === "error" ? state.message : undefined;
  };
  const previewAnswer = (): CronPreview | undefined => {
    const state = preview();
    return state.status === "ready" ? state.preview : undefined;
  };

  function trigger(): AutomationTrigger {
    switch (kind()) {
      case "schedule":
        return { kind: "schedule", cron: cron().trim() };
      case "issue":
        return {
          kind: "issue",
          repo_id: watchedRepo(),
          label: label().trim() === "" ? null : label().trim(),
        };
      case "pull_request":
        return { kind: "pull_request", repo_id: watchedRepo() };
    }
  }

  function request(): AutomationRequest {
    const id = pickedWorkflow()?.id;
    return {
      name: name().trim(),
      enabled: enabled(),
      trigger: trigger(),
      session: {
        prompt: prompt(),
        workflow: id === undefined || id === QUICK_WORKFLOW ? null : id,
        repos: choices().map((choice) => ({ repo_id: choice.repoId, mode: choice.mode })),
        // Never chosen: the server's default runner, whichever it is then.
        runner: runner() === "" ? null : (selectedRunner() ?? null),
        model: model().trim() === "" ? null : model().trim(),
        permission_mode: mode(),
      },
    };
  }

  async function save(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setSaved(false);
    setFailure(undefined);
    try {
      const result = await app.saveAutomation(stored?.id, request());
      setSaved(true);
      props.onSaved(result, stored === undefined);
    } catch (reason) {
      const message = errorMessage(reason);
      const invalid = reason instanceof HttpError && reason.status === 400;
      const field = invalid ? blameField(message, kind()) : undefined;
      setFailure({ message, field });
      // The message is in the page by the next frame; then the field it is about takes focus.
      queueMicrotask(() => {
        const target = field === undefined ? undefined : refs[field];
        (target ?? errorBox)?.focus();
      });
    } finally {
      setBusy(false);
    }
  }

  async function remove(): Promise<void> {
    if (!stored || deleting()) return;
    setDeleting(true);
    setDeleteError(undefined);
    try {
      await app.deleteAutomation(stored.id);
      navigate("/automations");
    } catch (reason) {
      setDeleteError(errorMessage(reason));
    } finally {
      setDeleting(false);
    }
  }

  const blamed = (field: BlamedField): boolean => failure()?.field === field;
  /** The server's message, shown at the field it is about, or by Save when it is about none. */
  const FieldError = (where: { field?: BlamedField }) => (
    <Show when={failure()?.field === where.field ? failure()?.message : undefined}>
      {(message) => (
        <p
          id={errorId}
          ref={(element) => {
            errorBox = element;
          }}
          tabindex="-1"
          role="alert"
          class="rounded-md border border-line bg-danger-soft px-3 py-2 text-sm wrap-break-word"
        >
          {message()}
        </p>
      )}
    </Show>
  );
  const describe = (field: BlamedField, own?: string): string | undefined =>
    [own, blamed(field) ? errorId : undefined].filter(Boolean).join(" ") || undefined;

  return (
    <form
      aria-label={stored ? `Edit ${stored.name}` : "New automation"}
      class="flex flex-col gap-5"
      novalidate
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
      onInput={() => setSaved(false)}
    >
      <div class="flex flex-col gap-1.5">
        <label for={nameId} class="text-sm font-medium">
          Name
        </label>
        <input
          id={nameId}
          ref={(element) => {
            refs.name = element;
          }}
          type="text"
          class="field h-11 px-3"
          maxlength={80}
          autocomplete="off"
          value={name()}
          aria-invalid={blamed("name") ? "true" : undefined}
          aria-describedby={describe("name")}
          onInput={(event) => setName(event.currentTarget.value)}
        />
        <FieldError field="name" />
      </div>

      <label class="flex min-h-11 cursor-pointer items-center gap-3">
        <input
          id={enabledId}
          type="checkbox"
          class="size-5 accent-accent"
          checked={enabled()}
          onChange={(event) => setEnabled(event.currentTarget.checked)}
        />
        <span>
          <span class="block font-medium">Enabled</span>
          <span class="block text-xs text-muted">
            When off, it never fires by itself; Run now still works.
          </span>
        </span>
      </label>

      <fieldset class="flex flex-col gap-2">
        <legend class="mb-1.5 text-sm font-medium">Trigger</legend>
        <div class="flex flex-wrap gap-2">
          <For each={triggerKinds}>
            {(one) => (
              <label class="choice items-center gap-2">
                <input
                  type="radio"
                  name={kindGroup}
                  value={one.kind}
                  class="size-5 accent-accent"
                  checked={kind() === one.kind}
                  onChange={() => setKind(one.kind)}
                />
                {one.label}
              </label>
            )}
          </For>
        </div>

        <Show when={kind() === "schedule"}>
          <div class="flex flex-col gap-1.5">
            <label for={cronId} class="text-sm font-medium">
              Cron expression
            </label>
            <input
              id={cronId}
              ref={(element) => {
                refs.cron = element;
              }}
              type="text"
              class="field h-11 px-3 font-mono"
              autocomplete="off"
              autocapitalize="off"
              spellcheck={false}
              value={cron()}
              aria-invalid={blamed("cron") ? "true" : undefined}
              aria-describedby={describe("cron", cronNoteId)}
              onInput={(event) => setCron(event.currentTarget.value)}
            />
            <FieldError field="cron" />
            <div role="group" aria-label="Cron presets" class="flex flex-wrap gap-2">
              <For each={cronPresets}>
                {(preset) => (
                  <button
                    type="button"
                    class="btn px-3 text-sm"
                    aria-pressed={cron().trim() === preset.expr ? "true" : "false"}
                    onClick={() => setCron(preset.expr)}
                  >
                    {preset.label}
                  </button>
                )}
              </For>
            </div>
            <div id={cronNoteId} aria-live="polite" class="text-sm" data-part="cron-preview">
              <Switch>
                <Match when={preview().status === "loading"}>
                  <p class="text-muted">Checking the expression…</p>
                </Match>
                <Match when={previewError()}>
                  {(message) => (
                    <p class={fieldError}>The expression could not be checked: {message()}</p>
                  )}
                </Match>
                <Match when={previewAnswer()}>
                  {(answer) => {
                    return (
                      <Show
                        when={answer().text}
                        fallback={
                          <p class={fieldError}>
                            {answer().problem ?? "This is not a valid cron expression."}
                          </p>
                        }
                      >
                        {(text) => (
                          <div class="flex flex-col gap-1">
                            <p class="font-medium">{text()}</p>
                            <Show when={answer().next.length > 0}>
                              <p class="text-xs text-muted">Next times, in your local time:</p>
                              <ul class="text-xs">
                                <For each={answer().next}>
                                  {(ts) => (
                                    <li>
                                      <time datetime={new Date(ts).toISOString()}>
                                        {weekdayTime(ts)}
                                      </time>
                                    </li>
                                  )}
                                </For>
                              </ul>
                            </Show>
                          </div>
                        )}
                      </Show>
                    );
                  }}
                </Match>
              </Switch>
            </div>
          </div>
        </Show>

        <Show when={kind() !== "schedule"}>
          <div class="flex flex-col gap-1.5">
            <label for={triggerRepoId} class="text-sm font-medium">
              Repo
            </label>
            <select
              id={triggerRepoId}
              ref={(element) => {
                refs["trigger-repo"] = element;
              }}
              class="field h-11 px-2"
              aria-invalid={blamed("trigger-repo") ? "true" : undefined}
              aria-describedby={describe("trigger-repo")}
              onChange={(event) => setTriggerRepo(event.currentTarget.value)}
            >
              <Show when={repos().length === 0}>
                <option value="">No repo is registered</option>
              </Show>
              <For each={repos()} keyed={(repo) => repo.id}>
                {(repo) => (
                  <option value={repo().id} selected={repo().id === watchedRepo()}>
                    {repo().name}
                  </option>
                )}
              </For>
            </select>
            <FieldError field="trigger-repo" />
            <p class="text-xs text-muted">
              {kind() === "issue"
                ? "Fires once for each new open issue of this repo. Issues that exist when it is switched on are not started on."
                : "Fires once for each new open pull request of this repo. Pull requests that exist when it is switched on are not started on."}
            </p>
          </div>
          <Show when={kind() === "issue"}>
            <div class="flex flex-col gap-1.5">
              <label for={labelId} class="text-sm font-medium">
                Label <span class="font-normal text-muted">(optional)</span>
              </label>
              <input
                id={labelId}
                type="text"
                class="field h-11 px-3"
                autocomplete="off"
                autocapitalize="off"
                spellcheck={false}
                placeholder="any label"
                value={label()}
                onInput={(event) => setLabel(event.currentTarget.value)}
              />
            </div>
          </Show>
        </Show>
      </fieldset>

      <section aria-label="Session to start" class="flex flex-col gap-3">
        <h2 class="font-semibold">Session to start</h2>
        <div class="flex flex-col gap-1.5">
          <label for={promptId} class="text-sm font-medium">
            Prompt
          </label>
          <textarea
            id={promptId}
            ref={(element) => {
              refs.prompt = element;
            }}
            class="field min-h-28 resize-y px-3 py-2"
            rows="5"
            value={prompt()}
            aria-invalid={blamed("prompt") ? "true" : undefined}
            aria-describedby={describe("prompt", promptNoteId)}
            onInput={(event) => setPrompt(event.currentTarget.value)}
          />
          <FieldError field="prompt" />
          <p id={promptNoteId} class="text-xs text-muted" data-part="placeholders">
            {placeholderNote(kind())}
          </p>
        </div>

        <RepoPicker
          registry={app.repos()}
          sets={app.repoSets()}
          choices={choices()}
          onChange={setChoices}
          onRegister={(body) => app.registerRepo(body)}
          onRemove={(id) => app.removeRepo(id)}
          onClone={(id) => app.cloneRepo(id)}
          onRetry={() => app.loadRepos()}
        />
        <FieldError field="repos" />

        <WorkflowField
          workflows={workflowList()}
          picked={pickedWorkflow()}
          onChange={setWorkflow}
          ref={(element) => {
            refs.workflow = element;
          }}
        />
        <FieldError field="workflow" />

        <PermissionField
          modes={automationModes}
          value={mode()}
          onChange={setMode}
          note="An automated session runs unattended, so Ask is not offered."
          ref={(element) => {
            refs.permission = element;
          }}
        />
        <FieldError field="permission" />

        <RunnerOptions
          runners={runners()}
          runner={selectedRunner()}
          model={model()}
          onRunner={setRunner}
          onModel={setModel}
        />
      </section>

      <FieldError />

      <div class="sticky bottom-0 flex flex-wrap items-center justify-between gap-3 border-t border-line bg-bg py-3">
        <Show when={stored}>
          <button
            type="button"
            class="btn text-tone-danger"
            aria-expanded={confirmDelete() ? "true" : "false"}
            onClick={() => setConfirmDelete((open) => !open)}
          >
            Delete
          </button>
        </Show>
        <span class="ml-auto flex items-center gap-3">
          <Show when={saved()}>
            <span role="status" class="text-sm text-tone-success">
              Saved
            </span>
          </Show>
          <button type="submit" class="btn btn-primary" disabled={busy()}>
            {busy() ? "Saving…" : stored ? "Save" : "Create automation"}
          </button>
        </span>
      </div>

      <Show when={stored && confirmDelete()}>
        <div
          role="group"
          aria-label="Delete this automation"
          class="flex flex-col gap-2 rounded-md border border-line bg-sunken p-3 text-sm"
        >
          <p>
            Delete <span class="font-medium">{name()}</span>? It stops firing. The sessions it
            started are kept.
          </p>
          <Show when={deleteError()}>
            {(message) => (
              <p role="alert" class={fieldError}>
                {message()}
              </p>
            )}
          </Show>
          <div class="flex justify-end gap-2">
            <button type="button" class="btn" onClick={() => setConfirmDelete(false)}>
              Keep
            </button>
            <button type="button" class="btn" disabled={deleting()} onClick={() => void remove()}>
              {deleting() ? "Deleting…" : "Delete automation"}
            </button>
          </div>
        </div>
      </Show>
    </form>
  );
}

/** "Run now": at once for a schedule; for a trigger, after asking which issue or pull request. */
function RunNow(props: { automation: Automation; onRan: (run: AutomationRun) => void }) {
  const app = useContext(AppContext);
  const [asking, setAsking] = createSignal(false);
  const [number, setNumber] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const [result, setResult] = createSignal<AutomationRun>();
  const numberId = createUniqueId();
  const numberLabel = (): string | undefined => runNumberLabel(props.automation.trigger);

  async function run(): Promise<void> {
    if (busy()) return;
    let wanted: number | undefined;
    if (numberLabel() !== undefined) {
      wanted = Number(number().trim().replace(/^#/, ""));
      if (!Number.isInteger(wanted) || wanted <= 0) {
        setError(`Give the ${numberLabel()?.toLowerCase()}, a whole number such as 12.`);
        return;
      }
    }
    setBusy(true);
    setError(undefined);
    setResult(undefined);
    try {
      const ran = await app.runAutomation(props.automation.id, wanted);
      setResult(ran);
      setAsking(false);
      setNumber("");
      props.onRan(ran);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section aria-label="Run now" class="flex flex-col gap-2">
      <Show
        when={asking() && numberLabel()}
        fallback={
          <div>
            <button
              type="button"
              class="btn"
              disabled={busy()}
              onClick={() => {
                setResult(undefined);
                setError(undefined);
                if (numberLabel() === undefined) void run();
                else setAsking(true);
              }}
            >
              {busy() ? "Starting…" : "Run now"}
            </button>
          </div>
        }
      >
        {(label) => (
          <form
            aria-label="Run now"
            class="flex flex-wrap items-end gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              void run();
            }}
          >
            <div class="flex flex-col gap-1">
              <label for={numberId} class="text-sm font-medium">
                {label()}
              </label>
              <input
                id={numberId}
                ref={(element) => queueMicrotask(() => element.focus())}
                type="text"
                inputmode="numeric"
                class="field h-11 w-32 px-3 font-mono"
                autocomplete="off"
                value={number()}
                onInput={(event) => setNumber(event.currentTarget.value)}
              />
            </div>
            <button type="submit" class="btn btn-primary" disabled={busy()}>
              {busy() ? "Starting…" : "Run"}
            </button>
            <button type="button" class="btn" onClick={() => setAsking(false)}>
              Cancel
            </button>
          </form>
        )}
      </Show>
      <Show when={error()}>
        {(message) => (
          <p role="alert" class={fieldError}>
            {message()}
          </p>
        )}
      </Show>
      <Show when={result()}>
        {(ran) => (
          <p role="status" class="flex flex-wrap items-center gap-x-2 text-sm">
            <span>{outcomeText(ran())}.</span>
            <Show when={ran().session_id}>
              {(id) => (
                <a
                  href={sessionPath(id())}
                  class="inline-flex min-h-11 items-center text-accent underline"
                >
                  Open the started session
                </a>
              )}
            </Show>
          </p>
        )}
      </Show>
    </section>
  );
}

/** The last fires, newest first: when, what fired it, what came of it. */
function RunHistory(props: {
  runs: readonly AutomationRun[] | undefined;
  error?: string;
  now: number;
}) {
  return (
    <section class="flex flex-col gap-2">
      <h2 class="font-semibold">Run history</h2>
      <Switch>
        <Match when={props.error}>
          {(message) => (
            <p role="alert" class={fieldError}>
              The run history could not be loaded: {message()}
            </p>
          )}
        </Match>
        <Match when={props.runs === undefined}>
          <p class="text-sm text-muted">Loading the run history…</p>
        </Match>
        <Match when={props.runs?.length === 0}>
          <p class="text-sm text-muted">It has not run yet.</p>
        </Match>
        <Match when={props.runs}>
          {(runs) => (
            <ul class="divide-y divide-line" data-part="runs">
              <For each={runs()} keyed={(run) => run.id}>
                {(run) => (
                  <li class="flex flex-col gap-0.5 py-2 text-sm">
                    <p class="flex flex-wrap items-center gap-x-2">
                      <time
                        datetime={new Date(run().at).toISOString()}
                        title={absoluteTime(run().at)}
                        class="text-muted"
                      >
                        {relativeTime(run().at, props.now)}
                      </time>
                      <span aria-hidden="true" class="text-muted">
                        ·
                      </span>
                      <span>{causeText(run().cause)}</span>
                    </p>
                    <p
                      class={[
                        "wrap-break-word",
                        run().outcome === "failed" ? "text-tone-danger" : undefined,
                      ]}
                    >
                      {outcomeText(run())}
                    </p>
                    <Show when={run().session_id}>
                      {(id) => (
                        <a
                          href={sessionPath(id())}
                          class="inline-flex min-h-11 items-center gap-2 self-start text-accent underline"
                        >
                          Open the session
                          <Show when={run().session_status}>
                            {(status) => <SessionStatusBadge status={status()} />}
                          </Show>
                        </a>
                      )}
                    </Show>
                  </li>
                )}
              </For>
            </ul>
          )}
        </Match>
      </Switch>
    </section>
  );
}

type Load =
  | { status: "loading" }
  | { status: "ready"; automation: Automation | undefined }
  | { status: "error"; message: string };

/** `/automations/new` and `/automations/:id`: the editor, and for one that exists, Run now and its history. */
export default function AutomationPage() {
  const app = useContext(AppContext);
  const params = useParams<{ id?: string }>();
  const navigate = useNavigate();
  const [load, setLoad] = createSignal<Load>({ status: "loading" });
  const [runs, setRuns] = createSignal<readonly AutomationRun[]>();
  const [runsError, setRunsError] = createSignal<string>();
  // The automation as last saved or read; the editor keeps its own drafts from the first read.
  const [latest, setLatest] = createSignal<Automation>();
  let asked = 0;

  onSettled(() => {
    app.loadRepos();
    app.loadWorkflows();
  });

  function readRuns(id: string): void {
    app.automationRuns(id).then(
      (loaded) => {
        setRunsError(undefined);
        setRuns(loaded);
      },
      (error: unknown) => setRunsError(errorMessage(error)),
    );
  }

  createEffect(
    () => params.id,
    (id) => {
      asked += 1;
      const mine = asked;
      setRuns(undefined);
      setLatest(undefined);
      if (id === undefined) {
        setLoad({ status: "ready", automation: undefined });
        return;
      }
      setLoad({ status: "loading" });
      app.getAutomation(id).then(
        (automation) => {
          if (mine !== asked) return;
          setLatest(automation);
          setLoad({ status: "ready", automation });
        },
        (error: unknown) => {
          if (mine === asked) setLoad({ status: "error", message: errorMessage(error) });
        },
      );
      readRuns(id);
    },
  );

  const ready = () => {
    const current = load();
    return current.status === "ready" ? current : undefined;
  };
  const failure = (): string | undefined => {
    const current = load();
    return current.status === "error" ? current.message : undefined;
  };
  const title = (): string => latest()?.name ?? (params.id ? "Automation" : "New automation");

  return (
    <section class="min-h-0 flex-1 overflow-y-auto overscroll-contain">
      <div class="mx-auto flex w-full max-w-2xl flex-col gap-5 px-3 py-3 lg:px-4 lg:py-8">
        <header class="flex items-center gap-2">
          <a href="/automations" aria-label="Back to automations" class="btn btn-ghost px-0">
            <span aria-hidden="true">←</span>
          </a>
          <h1 class="min-w-0 flex-1 truncate text-xl font-semibold">{title()}</h1>
        </header>
        <Switch>
          <Match when={failure()}>
            {(message) => (
              <p role="alert" class="text-sm wrap-break-word">
                The automation could not be loaded: {message()}
              </p>
            )}
          </Match>
          <Match when={ready()}>
            {(loaded) => (
              <>
                <Show when={latest()}>
                  {(automation) => (
                    <>
                      <p class="text-sm">{automation().trigger_text}</p>
                      <Show when={automation().disabled_reason}>
                        {(reason) => (
                          <p class="text-sm wrap-break-word text-tone-danger">
                            Switched off by Calin: {reason()}
                          </p>
                        )}
                      </Show>
                      <RunNow automation={automation()} onRan={() => readRuns(automation().id)} />
                    </>
                  )}
                </Show>
                {/* Keyed on the read: another automation starts the drafts over, a save does not. */}
                <Show when={loaded()} keyed>
                  {(current) => (
                    <AutomationEditor
                      stored={current.automation}
                      onSaved={(automation, created) => {
                        if (created) navigate(`/automations/${encodeURIComponent(automation.id)}`);
                        else setLatest(automation);
                      }}
                    />
                  )}
                </Show>
                <Show when={latest()}>
                  <RunHistory runs={runs()} error={runsError()} now={app.now()} />
                </Show>
              </>
            )}
          </Match>
          <Match when={true}>
            <p class="text-sm text-muted">Loading the automation…</p>
          </Match>
        </Switch>
      </div>
    </section>
  );
}
