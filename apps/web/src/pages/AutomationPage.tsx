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
  type Accessor,
} from "solid-js";
import { SessionStatusBadge } from "../components/Badge";
import { chosenWorkflow, offeredRunner } from "../components/Composer";
import { RepoPicker } from "../components/RepoPicker";
import { PermissionField, RunnerOptions, WorkflowField } from "../components/SessionOptions";
import type { Automation } from "../gen/Automation";
import type { AutomationRequest } from "../gen/AutomationRequest";
import type { AutomationRun } from "../gen/AutomationRun";
import type { CronPreview } from "../gen/CronPreview";
import type { PermissionMode } from "../gen/PermissionMode";
import type { Repo } from "../gen/Repo";
import {
  automationDraft,
  automationModes,
  causeText,
  cronPresets,
  messageAt,
  outcomeText,
  placeholders,
  runNumberLabel,
  saveFailure,
  sessionOf,
  triggerKinds,
  triggerOf,
  watchedRepo,
  type BlamedField,
  type SaveFailure,
  type TriggerKind,
} from "../lib/automations";
import { errorMessage } from "../lib/errors";
import { absoluteTime, listInWords, relativeTime } from "../lib/format";
import type { RepoChoice } from "../lib/prefs";
import { sentWorkflow } from "../lib/workflow";
import { sessionPath } from "../panels/registry";
import {
  AppContext,
  catalogWorkflows,
  healthRunners,
  type AppState,
  type RepoRegistry,
} from "../state/app";

/** How long typing in the cron field must pause before the server is asked what it means. */
export const CRON_PREVIEW_DELAY_MS = 400;

const fieldError = "text-sm wrap-break-word text-tone-danger";

type Preview =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready"; preview: CronPreview }
  | { status: "error"; message: string };

const previewError = (state: Preview): string | undefined =>
  state.status === "error" ? state.message : undefined;
const previewAnswer = (state: Preview): CronPreview | undefined =>
  state.status === "ready" ? state.preview : undefined;

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

const watchNote = (kind: TriggerKind): string =>
  kind === "issue"
    ? "Fires once for each new open issue of this repo. Issues that exist when it is switched on are not started on."
    : "Fires once for each new open pull request of this repo. Pull requests that exist when it is switched on are not started on.";

const registeredRepos = (registry: RepoRegistry): readonly Repo[] =>
  registry.status === "ready" ? registry.repos : [];

/** The fields a refused save can focus, and the message itself for a refusal about none. */
type FocusTargets = Partial<Record<BlamedField | "message", HTMLElement>>;

const focusTarget = (
  field: BlamedField | undefined,
  targets: FocusTargets,
): HTMLElement | undefined => (field === undefined ? undefined : targets[field]) ?? targets.message;

/** A field's `aria-describedby`: its own note, then the refusal when it is about this field. */
const describedBy = (own: string | undefined, error: string | undefined): string | undefined =>
  [own, error].filter(Boolean).join(" ") || undefined;

/** The editor's drafts, what the server offers for them, and the last refusal; shared by its field groups. */
function createAutomationForm(app: AppState, stored: Automation | undefined) {
  const start = automationDraft(stored);
  const [name, setName] = createSignal(start.name);
  const [enabled, setEnabled] = createSignal(start.enabled);
  const [kind, setKind] = createSignal<TriggerKind>(start.trigger.kind);
  const [cron, setCron] = createSignal(start.trigger.cron);
  const [triggerRepo, setTriggerRepo] = createSignal(start.trigger.repo);
  const [label, setLabel] = createSignal(start.trigger.label);
  const [prompt, setPrompt] = createSignal(start.session.prompt);
  const [choices, setChoices] = createSignal<readonly RepoChoice[]>(start.session.choices);
  const [workflow, setWorkflow] = createSignal<string | undefined>(start.session.workflow);
  const [runner, setRunner] = createSignal(start.session.runner);
  const [model, setModel] = createSignal(start.session.model);
  const [mode, setMode] = createSignal<PermissionMode>(start.session.mode);
  const [failure, setFailure] = createSignal<SaveFailure>();
  const errorId = createUniqueId();
  const targets: FocusTargets = {};

  const runners = (): readonly string[] => healthRunners(app.health());
  const repos = (): readonly Repo[] => registeredRepos(app.repos());
  const workflows = () => catalogWorkflows(app.workflows());
  const picked = createMemo(() => chosenWorkflow(workflow(), workflows()));
  const selectedRunner = (): string | undefined => offeredRunner(runner(), runners());
  const blamed = (field: BlamedField): boolean => failure()?.field === field;

  const request = (): AutomationRequest => ({
    name: name().trim(),
    enabled: enabled(),
    trigger: triggerOf(
      { kind: kind(), cron: cron(), repo: triggerRepo(), label: label() },
      repos(),
    ),
    session: sessionOf(
      {
        prompt: prompt(),
        choices: choices(),
        workflow: workflow(),
        runner: runner(),
        model: model(),
        mode: mode(),
      },
      { workflow: sentWorkflow(picked()), runner: selectedRunner() },
    ),
  });

  return {
    name,
    setName,
    enabled,
    setEnabled,
    kind,
    setKind,
    cron,
    setCron,
    setTriggerRepo,
    label,
    setLabel,
    prompt,
    setPrompt,
    choices,
    setChoices,
    setWorkflow,
    setRunner,
    model,
    setModel,
    mode,
    setMode,
    failure,
    setFailure,
    errorId,
    targets,
    runners,
    repos,
    workflows,
    picked,
    selectedRunner,
    request,
    watchedRepo: () => watchedRepo(triggerRepo(), repos()),
    invalid: (field: BlamedField) => (blamed(field) ? "true" : undefined),
    describe: (field: BlamedField, own?: string) =>
      describedBy(own, blamed(field) ? errorId : undefined),
  };
}

type AutomationForm = ReturnType<typeof createAutomationForm>;

/** What the server says the expression means, asked once typing pauses; the last ask wins. */
function createCronPreview(app: AppState, expression: () => string): Accessor<Preview> {
  const [preview, setPreview] = createSignal<Preview>({ status: "idle" });
  let timer: ReturnType<typeof setTimeout> | undefined;
  let asked = 0;
  createEffect(expression, (expr) => {
    clearTimeout(timer);
    asked += 1;
    if (expr === "") {
      setPreview({ status: "idle" });
      return;
    }
    const mine = asked;
    const answer = (state: Preview) => {
      if (mine === asked) setPreview(state);
    };
    setPreview({ status: "loading" });
    timer = setTimeout(() => {
      app.previewCron(expr).then(
        (preview) => answer({ status: "ready", preview }),
        (error: unknown) => answer({ status: "error", message: errorMessage(error) }),
      );
    }, CRON_PREVIEW_DELAY_MS);
  });
  onSettled(() => () => clearTimeout(timer));
  return preview;
}

/** The server's message, shown at the field it is about, or by Save when it is about none. */
function FieldError(props: { form: AutomationForm; field?: BlamedField }) {
  return (
    <Show when={messageAt(props.form.failure(), props.field)}>
      {(message) => (
        <p
          id={props.form.errorId}
          ref={(element) => {
            props.form.targets.message = element;
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
}

function NameField(props: { form: AutomationForm }) {
  const nameId = createUniqueId();
  return (
    <div class="flex flex-col gap-1.5">
      <label for={nameId} class="text-sm font-medium">
        Name
      </label>
      <input
        id={nameId}
        ref={(element) => {
          props.form.targets.name = element;
        }}
        type="text"
        class="field h-11 px-3"
        maxlength={80}
        autocomplete="off"
        value={props.form.name()}
        aria-invalid={props.form.invalid("name")}
        aria-describedby={props.form.describe("name")}
        onInput={(event) => props.form.setName(event.currentTarget.value)}
      />
      <FieldError form={props.form} field="name" />
    </div>
  );
}

function EnabledField(props: { form: AutomationForm }) {
  const enabledId = createUniqueId();
  return (
    <label class="flex min-h-11 cursor-pointer items-center gap-3">
      <input
        id={enabledId}
        type="checkbox"
        class="size-5 accent-accent"
        checked={props.form.enabled()}
        onChange={(event) => props.form.setEnabled(event.currentTarget.checked)}
      />
      <span>
        <span class="block font-medium">Enabled</span>
        <span class="block text-xs text-muted">
          When off, it never fires by itself; Run now still works.
        </span>
      </span>
    </label>
  );
}

function TriggerFields(props: { form: AutomationForm; preview: Preview }) {
  const kindGroup = createUniqueId();
  return (
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
                checked={props.form.kind() === one.kind}
                onChange={() => props.form.setKind(one.kind)}
              />
              {one.label}
            </label>
          )}
        </For>
      </div>

      <Show when={props.form.kind() === "schedule"}>
        <ScheduleFields form={props.form} preview={props.preview} />
      </Show>

      <Show when={props.form.kind() !== "schedule"}>
        <WatchFields form={props.form} />
      </Show>
    </fieldset>
  );
}

function ScheduleFields(props: { form: AutomationForm; preview: Preview }) {
  const cronId = createUniqueId();
  const cronNoteId = createUniqueId();
  return (
    <div class="flex flex-col gap-1.5">
      <label for={cronId} class="text-sm font-medium">
        Cron expression
      </label>
      <input
        id={cronId}
        ref={(element) => {
          props.form.targets.cron = element;
        }}
        type="text"
        class="field h-11 px-3 font-mono"
        autocomplete="off"
        autocapitalize="off"
        spellcheck={false}
        value={props.form.cron()}
        aria-invalid={props.form.invalid("cron")}
        aria-describedby={props.form.describe("cron", cronNoteId)}
        onInput={(event) => props.form.setCron(event.currentTarget.value)}
      />
      <FieldError form={props.form} field="cron" />
      <div role="group" aria-label="Cron presets" class="flex flex-wrap gap-2">
        <For each={cronPresets}>
          {(preset) => (
            <button
              type="button"
              class="btn px-3 text-sm"
              aria-pressed={props.form.cron().trim() === preset.expr ? "true" : "false"}
              onClick={() => props.form.setCron(preset.expr)}
            >
              {preset.label}
            </button>
          )}
        </For>
      </div>
      <CronPreviewNote id={cronNoteId} preview={props.preview} />
    </div>
  );
}

function CronPreviewNote(props: { id: string; preview: Preview }) {
  return (
    <div id={props.id} aria-live="polite" class="text-sm" data-part="cron-preview">
      <Switch>
        <Match when={props.preview.status === "loading"}>
          <p class="text-muted">Checking the expression…</p>
        </Match>
        <Match when={previewError(props.preview)}>
          {(message) => <p class={fieldError}>The expression could not be checked: {message()}</p>}
        </Match>
        <Match when={previewAnswer(props.preview)}>
          {(answer) => <CronAnswer answer={answer()} />}
        </Match>
      </Switch>
    </div>
  );
}

function CronAnswer(props: { answer: CronPreview }) {
  return (
    <Show
      when={props.answer.text}
      fallback={
        <p class={fieldError}>{props.answer.problem ?? "This is not a valid cron expression."}</p>
      }
    >
      {(text) => (
        <div class="flex flex-col gap-1">
          <p class="font-medium">{text()}</p>
          <Show when={props.answer.next.length > 0}>
            <p class="text-xs text-muted">Next times, in your local time:</p>
            <ul class="text-xs">
              <For each={props.answer.next}>
                {(ts) => (
                  <li>
                    <time datetime={new Date(ts).toISOString()}>{weekdayTime(ts)}</time>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </div>
      )}
    </Show>
  );
}

/** The repo an issue or pull request trigger watches, and an issue trigger's label. */
function WatchFields(props: { form: AutomationForm }) {
  const triggerRepoId = createUniqueId();
  const labelId = createUniqueId();
  return (
    <>
      <div class="flex flex-col gap-1.5">
        <label for={triggerRepoId} class="text-sm font-medium">
          Repo
        </label>
        <select
          id={triggerRepoId}
          ref={(element) => {
            props.form.targets["trigger-repo"] = element;
          }}
          class="field h-11 px-2"
          aria-invalid={props.form.invalid("trigger-repo")}
          aria-describedby={props.form.describe("trigger-repo")}
          onChange={(event) => props.form.setTriggerRepo(event.currentTarget.value)}
        >
          <Show when={props.form.repos().length === 0}>
            <option value="">No repo is registered</option>
          </Show>
          <For each={props.form.repos()} keyed={(repo) => repo.id}>
            {(repo) => (
              <option value={repo().id} selected={repo().id === props.form.watchedRepo()}>
                {repo().name}
              </option>
            )}
          </For>
        </select>
        <FieldError form={props.form} field="trigger-repo" />
        <p class="text-xs text-muted">{watchNote(props.form.kind())}</p>
      </div>
      <Show when={props.form.kind() === "issue"}>
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
            value={props.form.label()}
            onInput={(event) => props.form.setLabel(event.currentTarget.value)}
          />
        </div>
      </Show>
    </>
  );
}

function SessionFields(props: { form: AutomationForm }) {
  const app = useContext(AppContext);
  const promptId = createUniqueId();
  const promptNoteId = createUniqueId();
  return (
    <section aria-label="Session to start" class="flex flex-col gap-3">
      <h2 class="font-semibold">Session to start</h2>
      <div class="flex flex-col gap-1.5">
        <label for={promptId} class="text-sm font-medium">
          Prompt
        </label>
        <textarea
          id={promptId}
          ref={(element) => {
            props.form.targets.prompt = element;
          }}
          class="field min-h-28 resize-y px-3 py-2"
          rows="5"
          value={props.form.prompt()}
          aria-invalid={props.form.invalid("prompt")}
          aria-describedby={props.form.describe("prompt", promptNoteId)}
          onInput={(event) => props.form.setPrompt(event.currentTarget.value)}
        />
        <FieldError form={props.form} field="prompt" />
        <p id={promptNoteId} class="text-xs text-muted" data-part="placeholders">
          {placeholderNote(props.form.kind())}
        </p>
      </div>

      <RepoPicker
        registry={app.repos()}
        sets={app.repoSets()}
        choices={props.form.choices()}
        onChange={(choices) => props.form.setChoices(choices)}
        onRegister={(body) => app.registerRepo(body)}
        onRemove={(id) => app.removeRepo(id)}
        onClone={(id) => app.cloneRepo(id)}
        onRetry={() => app.loadRepos()}
      />
      <FieldError form={props.form} field="repos" />

      <WorkflowField
        workflows={props.form.workflows()}
        picked={props.form.picked()}
        onChange={(id) => props.form.setWorkflow(id)}
        ref={(element) => {
          props.form.targets.workflow = element;
        }}
      />
      <FieldError form={props.form} field="workflow" />

      <PermissionField
        modes={automationModes}
        value={props.form.mode()}
        onChange={(mode) => props.form.setMode(mode)}
        note="An automated session runs unattended, so Ask is not offered."
        ref={(element) => {
          props.form.targets.permission = element;
        }}
      />
      <FieldError form={props.form} field="permission" />

      <RunnerOptions
        runners={props.form.runners()}
        runner={props.form.selectedRunner()}
        model={props.form.model()}
        onRunner={(runner) => props.form.setRunner(runner)}
        onModel={(model) => props.form.setModel(model)}
      />
    </section>
  );
}

const saveLabel = (busy: boolean, stored: boolean): string =>
  busy ? "Saving…" : stored ? "Save" : "Create automation";

/** The bar under the form: Delete for a saved automation, then Saved and the submit button. */
function FormActions(props: {
  stored: boolean;
  confirming: boolean;
  onDelete: () => void;
  saved: boolean;
  busy: boolean;
}) {
  return (
    <div class="sticky bottom-0 flex flex-wrap items-center justify-between gap-3 border-t border-line bg-bg py-3">
      <Show when={props.stored}>
        <button
          type="button"
          class="btn text-tone-danger"
          aria-expanded={props.confirming ? "true" : "false"}
          onClick={() => props.onDelete()}
        >
          Delete
        </button>
      </Show>
      <span class="ml-auto flex items-center gap-3">
        <Show when={props.saved}>
          <span role="status" class="text-sm text-tone-success">
            Saved
          </span>
        </Show>
        <button type="submit" class="btn btn-primary" disabled={props.busy}>
          {saveLabel(props.busy, props.stored)}
        </button>
      </span>
    </div>
  );
}

/** Deleting asks first; what went wrong stays shown until the next try. */
function createDeletion(app: AppState, stored: Automation | undefined, onDeleted: () => void) {
  const [confirming, setConfirming] = createSignal(false);
  const [deleting, setDeleting] = createSignal(false);
  const [error, setError] = createSignal<string>();

  async function remove(): Promise<void> {
    if (!stored || deleting()) return;
    setDeleting(true);
    setError(undefined);
    try {
      await app.deleteAutomation(stored.id);
      onDeleted();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setDeleting(false);
    }
  }

  return { confirming, setConfirming, deleting, error, remove };
}

function DeleteConfirm(props: { name: string; deletion: ReturnType<typeof createDeletion> }) {
  return (
    <div
      role="group"
      aria-label="Delete this automation"
      class="flex flex-col gap-2 rounded-md border border-line bg-sunken p-3 text-sm"
    >
      <p>
        Delete <span class="font-medium">{props.name}</span>? It stops firing. The sessions it
        started are kept.
      </p>
      <Show when={props.deletion.error()}>
        {(message) => (
          <p role="alert" class={fieldError}>
            {message()}
          </p>
        )}
      </Show>
      <div class="flex justify-end gap-2">
        <button type="button" class="btn" onClick={() => props.deletion.setConfirming(false)}>
          Keep
        </button>
        <button
          type="button"
          class="btn"
          disabled={props.deletion.deleting()}
          onClick={() => void props.deletion.remove()}
        >
          {props.deletion.deleting() ? "Deleting…" : "Delete automation"}
        </button>
      </div>
    </div>
  );
}

/** The new-or-edit form. `stored` is absent for a new automation. */
function AutomationEditor(props: {
  stored: Automation | undefined;
  onSaved: (automation: Automation, created: boolean) => void;
}) {
  const app = useContext(AppContext);
  const navigate = useNavigate();
  const stored = props.stored;
  const form = createAutomationForm(app, stored);
  const preview = createCronPreview(app, () =>
    form.kind() === "schedule" ? form.cron().trim() : "",
  );
  const deletion = createDeletion(app, stored, () => navigate("/automations"));
  const [busy, setBusy] = createSignal(false);
  const [saved, setSaved] = createSignal(false);

  async function save(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setSaved(false);
    form.setFailure(undefined);
    try {
      const result = await app.saveAutomation(stored?.id, form.request());
      setSaved(true);
      props.onSaved(result, stored === undefined);
    } catch (reason) {
      const failure = saveFailure(reason, form.kind());
      form.setFailure(failure);
      // The message is in the page by the next frame; then the field it is about takes focus.
      queueMicrotask(() => focusTarget(failure.field, form.targets)?.focus());
    } finally {
      setBusy(false);
    }
  }

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
      <NameField form={form} />

      <EnabledField form={form} />

      <TriggerFields form={form} preview={preview()} />

      <SessionFields form={form} />

      <FieldError form={form} />

      <FormActions
        stored={stored !== undefined}
        confirming={deletion.confirming()}
        onDelete={() => deletion.setConfirming((open) => !open)}
        saved={saved()}
        busy={busy()}
      />

      <Show when={stored && deletion.confirming()}>
        <DeleteConfirm name={form.name()} deletion={deletion} />
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
