import {
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
import type { JSX } from "@solidjs/web";
import { HttpError } from "../api/http";
import { AccessSettings, LoadFailure } from "../components/AccessSettings";
import { InstallHint, NotificationSettings } from "../components/NotificationSettings";
import type { RepoSet } from "../gen/RepoSet";
import type { Settings } from "../gen/Settings";
import { errorMessage } from "../lib/errors";
import { counted } from "../lib/format";
import { createRequest } from "../lib/request";
import { onFormSubmit } from "../lib/submit";
import { themeChoices, themeState } from "../lib/theme";
import { blameSettingsField, settingsFromForm, type SettingsBlame } from "../lib/settings";
import { problemLines, stepCount } from "../lib/workflow";
import { AppContext, type RepoRegistry, type WorkflowCatalog } from "../state/app";

const EXAMPLE = `# <home repo>/.calin/repos.yaml
repos:
  api: { url: git@github.com:you/api.git }
  web: { url: git@github.com:you/web.git }
sets:
  storefront: [api, web]
`;

const hint = "text-xs text-muted";
const fieldError = "text-sm wrap-break-word text-tone-danger";

type Failure = { message: string; blame: SettingsBlame | undefined };

/** The refusal shown at `field`; `undefined` for the one by the Save button. */
const errorAt = (
  failure: Failure | undefined,
  field: SettingsBlame["field"] | undefined,
): string | undefined => (failure && failure.blame?.field === field ? failure.message : undefined);

/** What a failed save says, and the field it is about. */
function failureOf(error: unknown, sent: Settings): Failure {
  const message = errorMessage(error);
  // Only a 400 is about what was typed (api.rs); anything else is about the request.
  const invalid = error instanceof HttpError && error.status === 400;
  return { message, blame: invalid ? blameSettingsField(message, sent) : undefined };
}

/** The scan root a refusal names, if any. */
const blamedRoot = (failure: Failure | undefined): string | undefined => failure?.blame?.scanRoot;

const withRoot = (roots: readonly string[], path: string): readonly string[] =>
  roots.includes(path) ? roots : [...roots, path];

/** The limit as the field shows it: empty for the server's default. */
const typedParallel = (value: number | null | undefined): string => value?.toString() ?? "";

const invalidIf = (error: string | undefined): "true" | undefined =>
  error === undefined ? undefined : "true";

/** The id of the alert a field points at, while it shows one. */
const describedIf = (error: string | undefined, id: string): string | undefined =>
  error === undefined ? undefined : id;

/** The refusal of one field, under it, named so the field can point at it. */
function FieldError(props: { id: string; message: string | undefined }) {
  return (
    <Show when={props.message}>
      {(message) => (
        <p id={props.id} role="alert" class={fieldError}>
          {message()}
        </p>
      )}
    </Show>
  );
}

/** A path on the server, typed in full, with its refusal and its hint. */
function PathField(props: {
  id: string;
  errorId: string;
  label: string;
  placeholder: string;
  value: string;
  error: string | undefined;
  onInput: (value: string) => void;
  children: JSX.Element;
}) {
  return (
    <div class="flex flex-col gap-1.5">
      <label for={props.id} class="text-sm font-medium">
        {props.label}
      </label>
      <input
        id={props.id}
        type="text"
        class="field h-11 px-3 font-mono"
        placeholder={props.placeholder}
        autocomplete="off"
        autocapitalize="off"
        spellcheck={false}
        value={props.value}
        aria-invalid={invalidIf(props.error)}
        aria-describedby={describedIf(props.error, props.errorId)}
        onInput={(event) => props.onInput(event.currentTarget.value)}
      />
      <FieldError id={props.errorId} message={props.error} />
      <p class={hint}>{props.children}</p>
    </div>
  );
}

/** The scan roots: the list, each with its Remove button, and the field that adds one. */
function ScanRoots(props: {
  roots: readonly string[];
  /** The root a refusal names, marked in the list. */
  blamed: string | undefined;
  newRoot: string;
  error: string | undefined;
  inputId: string;
  errorId: string;
  onNewRoot: (value: string) => void;
  onAdd: () => void;
  onRemove: (root: string) => void;
}) {
  return (
    <fieldset class="flex flex-col gap-1.5">
      <legend class="mb-1.5 text-sm font-medium">Scan roots</legend>
      <Show
        when={props.roots.length > 0}
        fallback={<p class="text-sm text-muted">No scan root yet.</p>}
      >
        <ul aria-label="Scan roots" class="divide-y divide-line rounded-md border border-line">
          <For each={props.roots}>
            {(root) => (
              <li class="flex items-center gap-2 pl-3">
                <span
                  class={[
                    "min-w-0 flex-1 truncate font-mono text-sm",
                    { "text-tone-danger": props.blamed === root },
                  ]}
                  title={root}
                >
                  {root}
                </span>
                <button
                  type="button"
                  class="btn btn-ghost px-2 text-sm"
                  aria-label={`Remove ${root}`}
                  onClick={() => props.onRemove(root)}
                >
                  Remove
                </button>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <div class="flex gap-2">
        <label for={props.inputId} class="sr-only">
          Add a scan root
        </label>
        <input
          id={props.inputId}
          type="text"
          class="field h-11 min-w-0 flex-1 px-3 font-mono"
          placeholder="/home/you/code"
          autocomplete="off"
          autocapitalize="off"
          spellcheck={false}
          value={props.newRoot}
          aria-invalid={invalidIf(props.error)}
          aria-describedby={describedIf(props.error, props.errorId)}
          onInput={(event) => props.onNewRoot(event.currentTarget.value)}
          onKeyDown={(event) => {
            // Enter adds the path; saving is the button's job.
            if (event.key !== "Enter" || props.newRoot.trim() === "") return;
            event.preventDefault();
            props.onAdd();
          }}
        />
        <button
          type="button"
          class="btn"
          disabled={props.newRoot.trim() === ""}
          onClick={() => props.onAdd()}
        >
          Add
        </button>
      </div>
      <FieldError id={props.errorId} message={props.error} />
      <p class={hint}>
        Folders searched for local clones of the repos the home repo declares, matched by their git
        URL.
      </p>
    </fieldset>
  );
}

function ParallelField(props: {
  id: string;
  errorId: string;
  value: string;
  error: string | undefined;
  onInput: (value: string) => void;
}) {
  const hintId = (): string => `${props.errorId}-hint`;
  return (
    <div class="flex flex-col gap-1.5">
      <label for={props.id} class="text-sm font-medium">
        Sessions running at once
      </label>
      <input
        id={props.id}
        type="number"
        inputmode="numeric"
        class="field h-11 w-32 px-3"
        placeholder="Default"
        value={props.value}
        aria-invalid={invalidIf(props.error)}
        aria-describedby={describedIf(props.error, `${props.errorId} ${hintId()}`) ?? hintId()}
        onInput={(event) => props.onInput(event.currentTarget.value)}
      />
      <FieldError id={props.errorId} message={props.error} />
      <p id={hintId()} class={hint}>
        How many sessions may prepare or run at the same time; the others wait, queued, for a free
        slot. Child sessions do not count. Empty for the server's default.
      </p>
    </div>
  );
}

/** Save, with the refusal about no field before it, or the word that it went through. */
function SaveRow(props: { error: string | undefined; saved: boolean; busy: boolean }) {
  return (
    <div class="flex flex-wrap items-center justify-end gap-3">
      <Show when={props.error}>
        {(message) => (
          <p role="alert" class={["min-w-0 flex-1 basis-48", fieldError]}>
            {message()}
          </p>
        )}
      </Show>
      <Show when={props.saved}>
        <p role="status" class="min-w-0 flex-1 text-sm text-tone-success">
          Saved.
        </p>
      </Show>
      <button type="submit" class="btn btn-primary" disabled={props.busy}>
        {props.busy ? "Saving…" : "Save"}
      </button>
    </div>
  );
}

/**
 * The three settings, edited as a draft of what the server stores. A refusal
 * is shown at the field it is about (see `blameSettingsField`), or by the
 * Save button when it is about none.
 */
function SettingsForm(props: { stored: Settings; onSave: (next: Settings) => Promise<Settings> }) {
  // Drafts follow the stored value: a save, or a reload, starts them over from what the server has.
  const [homeRepo, setHomeRepo] = createSignal(() => props.stored.home_repo ?? "");
  const [scanRoots, setScanRoots] = createSignal<readonly string[]>(() => props.stored.scan_roots);
  const [cloneRoot, setCloneRoot] = createSignal(() => props.stored.clone_root ?? "");
  const [maxParallel, setMaxParallel] = createSignal(() =>
    typedParallel(props.stored.max_parallel),
  );
  const [newRoot, setNewRoot] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [saved, setSaved] = createSignal(false);
  const [failure, setFailure] = createSignal<Failure>();
  const homeId = createUniqueId();
  const rootId = createUniqueId();
  const cloneId = createUniqueId();
  const parallelId = createUniqueId();
  const errorId = createUniqueId();

  const errorOf = (field: SettingsBlame["field"] | undefined): string | undefined =>
    errorAt(failure(), field);

  function edited(): void {
    setSaved(false);
  }

  function addRoot(): void {
    const path = newRoot().trim();
    if (path === "") return;
    setScanRoots(withRoot(scanRoots(), path));
    setNewRoot("");
    edited();
  }

  async function save(): Promise<void> {
    if (busy()) return;
    // A path typed but not added yet is meant to be saved too.
    const next = settingsFromForm({
      homeRepo: homeRepo(),
      scanRoots: [...scanRoots(), newRoot()],
      cloneRoot: cloneRoot(),
      maxParallel: maxParallel(),
    });
    setBusy(true);
    setFailure(undefined);
    setSaved(false);
    try {
      await props.onSave(next);
      setNewRoot("");
      setSaved(true);
    } catch (error) {
      setFailure(failureOf(error, next));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      aria-label="Settings"
      // The server says what it accepts (api.rs); the browser's own checks would hide its words.
      novalidate
      class="flex flex-col gap-5"
      onSubmit={onFormSubmit(save)}
    >
      <PathField
        id={homeId}
        errorId={`${errorId}-home`}
        label="Home repo"
        placeholder="/home/you/code/home"
        value={homeRepo()}
        error={errorOf("home_repo")}
        onInput={(value) => {
          setHomeRepo(value);
          edited();
        }}
      >
        Path, on the server, of the git repo every session is launched from. Empty for none.
      </PathField>

      <ScanRoots
        roots={scanRoots()}
        blamed={blamedRoot(failure())}
        newRoot={newRoot()}
        error={errorOf("scan_roots")}
        inputId={rootId}
        errorId={`${errorId}-scan`}
        onNewRoot={(value) => {
          setNewRoot(value);
          edited();
        }}
        onAdd={addRoot}
        onRemove={(root) => {
          setScanRoots(scanRoots().filter((one) => one !== root));
          edited();
        }}
      />

      <PathField
        id={cloneId}
        errorId={`${errorId}-clone`}
        label="Clone root"
        placeholder="Server default: <data dir>/repos"
        value={cloneRoot()}
        error={errorOf("clone_root")}
        onInput={(value) => {
          setCloneRoot(value);
          edited();
        }}
      >
        Where a declared repo with no local clone is cloned to. Empty for the server's default.
      </PathField>

      <ParallelField
        id={parallelId}
        errorId={`${errorId}-parallel`}
        value={maxParallel()}
        error={errorOf("max_parallel")}
        onInput={(value) => {
          setMaxParallel(value);
          edited();
        }}
      />

      <SaveRow error={errorOf(undefined)} saved={saved()} busy={busy()} />
    </form>
  );
}

/** What the home repo declares, as the repo list and the sets report it. Read-only. */
function HomeRepoSummary(props: {
  homeRepo: string;
  registry: RepoRegistry;
  sets: readonly RepoSet[];
  onRescan: () => Promise<void>;
  onRetry: () => void;
}) {
  const rescan = createRequest(() => props.onRescan());
  const repos = () => (props.registry.status === "ready" ? props.registry.repos : undefined);
  const loadError = () => (props.registry.status === "error" ? props.registry : undefined);
  const totals = createMemo(() => {
    const declared = (repos() ?? []).filter((repo) => repo.declared);
    const found = declared.filter((repo) => repo.available).length;
    return { declared: declared.length, found, missing: declared.length - found };
  });
  const problem = (): string | null => repos()?.find((repo) => repo.home)?.problem ?? null;

  return (
    <>
      <div class="flex items-center justify-between gap-2">
        <p class="min-w-0 truncate font-mono text-sm text-muted" title={props.homeRepo}>
          {props.homeRepo}
        </p>
        <button
          type="button"
          class="btn shrink-0"
          title="Read the home repo's list again and look for clones under the scan roots"
          disabled={rescan.busy()}
          onClick={() => void rescan.run()}
        >
          {rescan.busy() ? "Rescanning…" : "Rescan"}
        </button>
      </div>
      <Show when={rescan.error()}>
        {(message) => (
          <p role="alert" class={fieldError}>
            Could not rescan: {message()}
          </p>
        )}
      </Show>
      <Switch>
        <Match when={props.registry.status === "loading"}>
          <p class="text-sm text-muted">Reading the repo list…</p>
        </Match>
        <Match when={loadError()}>
          {(failure) => (
            <LoadFailure
              what="The repo list"
              message={failure().message}
              onRetry={() => props.onRetry()}
            />
          )}
        </Match>
        <Match when={true}>
          <Show when={problem()}>
            {(message) => (
              <p
                role="alert"
                class="rounded-md border border-tone-danger bg-danger-soft px-3 py-2 text-sm wrap-break-word"
              >
                {message()}
              </p>
            )}
          </Show>
          <dl class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 text-sm">
            <dt class="text-muted">Declared</dt>
            <dd>{counted(totals().declared, "repo", "repos")}</dd>
            <dt class="text-muted">Found locally</dt>
            <dd>{totals().found}</dd>
            <dt class="text-muted">Not cloned</dt>
            <dd class={{ "text-tone-attention": totals().missing > 0 }}>{totals().missing}</dd>
            <dt class="text-muted">Sets</dt>
            <dd>
              <Show when={props.sets.length > 0} fallback="none">
                <ul class="flex flex-col gap-0.5">
                  <For each={props.sets} keyed={(set) => set.name}>
                    {(set) => (
                      <li>
                        <span class="font-medium">{set().name}</span>{" "}
                        <span class="text-muted">
                          ({counted(set().repo_ids.length, "repo", "repos")})
                        </span>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </dd>
          </dl>
        </Match>
      </Switch>
    </>
  );
}

/** Every workflow the home repo offers, and what is wrong with those that cannot be used. */
function WorkflowList(props: { catalog: WorkflowCatalog; onRetry: () => void }) {
  return (
    <Switch>
      <Match when={props.catalog.status === "loading"}>
        <p class="text-sm text-muted">Reading the workflows…</p>
      </Match>
      <Match when={props.catalog.status === "error" ? props.catalog : undefined}>
        {(failure) => (
          <LoadFailure
            what="The workflows"
            message={failure().message}
            onRetry={() => props.onRetry()}
          />
        )}
      </Match>
      <Match when={props.catalog.status === "ready" ? props.catalog.workflows : undefined}>
        {(workflows) => (
          <>
            <ul aria-label="Workflows" class="divide-y divide-line rounded-md border border-line">
              <For each={workflows()} keyed={(one) => one.id}>
                {(one) => (
                  <li
                    data-workflow={one().id}
                    data-invalid={one().problem === null ? undefined : ""}
                    class="flex flex-col gap-1 px-3 py-2"
                  >
                    <div class="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5">
                      <span class="font-medium wrap-anywhere">{one().name}</span>
                      <span class="font-mono text-xs text-muted wrap-anywhere">{one().id}</span>
                      <span class="ml-auto text-xs text-muted">
                        <Show
                          when={one().graph}
                          fallback={<span class="font-medium text-tone-danger">Has errors</span>}
                        >
                          {(graph) => <span>{counted(stepCount(graph()), "step", "steps")}</span>}
                        </Show>
                      </span>
                    </div>
                    <Show when={one().builtin}>
                      <p class="text-xs text-muted">Built in</p>
                    </Show>
                    <Show when={one().problem !== null}>
                      <ul
                        aria-label={`Problems in ${one().id}`}
                        class="overflow-x-auto rounded bg-danger-soft px-2 py-1.5 font-mono text-xs"
                      >
                        <For each={problemLines(one())}>
                          {(line) => <li class="whitespace-pre-wrap wrap-anywhere">{line}</li>}
                        </For>
                      </ul>
                    </Show>
                  </li>
                )}
              </For>
            </ul>
            <p class={hint}>
              Workflows are the <span class="font-mono">.yaml</span> files in the home repo's{" "}
              <span class="font-mono">.calin/workflows</span> folder; the file name is the id.
            </p>
          </>
        )}
      </Match>
    </Switch>
  );
}

function HomeRepoExplained() {
  return (
    <>
      <p class="text-sm">
        A home repo is the git repo you start every session from. It lists your other repos by name
        in <span class="font-mono">.calin/repos.yaml</span>, optionally grouped into sets, so a
        session can attach the ones its task touches. Set its path above once the file exists; a
        minimal one looks like this:
      </p>
      <pre
        tabindex="0"
        aria-label="Example repos.yaml"
        class="overflow-x-auto rounded-md bg-sunken p-3 font-mono text-xs"
      >
        {EXAMPLE}
      </pre>
    </>
  );
}

/** `/settings`: where this machine keeps its repos, and what the home repo declares. */
/** Light, dark or the system's setting; kept in this browser, applied at once. */
function AppearanceSettings() {
  const theme = themeState();
  const group = createUniqueId();
  return (
    <fieldset class="flex flex-col gap-2">
      <legend class="mb-1.5 text-sm font-medium">Theme</legend>
      <div class="flex flex-wrap gap-2">
        <For each={themeChoices}>
          {(one) => (
            <label class="choice items-center gap-2">
              <input
                type="radio"
                name={group}
                value={one.choice}
                class="size-5 accent-accent"
                checked={theme.choice() === one.choice}
                onChange={() => theme.setChoice(one.choice)}
              />
              {one.label}
            </label>
          )}
        </For>
      </div>
      <p class={hint}>System follows this device's light or dark setting.</p>
    </fieldset>
  );
}

export default function SettingsPage() {
  const app = useContext(AppContext);
  const summaryId = createUniqueId();
  const workflowsId = createUniqueId();

  onSettled(() => {
    app.loadSettings();
    app.loadRepos();
    app.loadWorkflows();
  });

  const stored = () => {
    const state = app.settings();
    return state.status === "ready" ? state.settings : undefined;
  };
  const failed = () => {
    const state = app.settings();
    return state.status === "error" ? state : undefined;
  };

  return (
    <section class="min-h-0 flex-1 overflow-y-auto overscroll-contain">
      <div class="mx-auto flex w-full max-w-2xl flex-col gap-5 px-3 py-3 lg:px-4 lg:py-8">
        <header class="flex items-center gap-2">
          <a href="/" aria-label="Back to sessions" class="btn btn-ghost px-0 lg:hidden">
            <span aria-hidden="true">←</span>
          </a>
          <h1 class="text-xl font-semibold">Settings</h1>
        </header>
        <AppearanceSettings />
        <Switch>
          <Match when={failed()}>
            {(failure) => (
              <div role="alert" class="flex flex-col items-start gap-2 text-sm">
                <p>The settings could not be loaded: {failure().message}</p>
                <button type="button" class="btn" onClick={() => app.loadSettings()}>
                  Try again
                </button>
              </div>
            )}
          </Match>
          <Match when={stored()}>
            {(settings) => (
              <>
                <SettingsForm stored={settings()} onSave={(next) => app.saveSettings(next)} />
                <section
                  aria-labelledby={summaryId}
                  class="flex flex-col gap-3 border-t border-line pt-4"
                >
                  <h2 id={summaryId} class="text-sm font-medium">
                    Home repo catalog
                  </h2>
                  <Show when={settings().home_repo} fallback={<HomeRepoExplained />}>
                    {(homeRepo) => (
                      <HomeRepoSummary
                        homeRepo={homeRepo()}
                        registry={app.repos()}
                        sets={app.repoSets()}
                        onRescan={() => app.rescanRepos()}
                        onRetry={() => app.loadRepos()}
                      />
                    )}
                  </Show>
                </section>
                <section
                  aria-labelledby={workflowsId}
                  class="flex flex-col gap-3 border-t border-line pt-4"
                >
                  <h2 id={workflowsId} class="text-sm font-medium">
                    Workflows
                  </h2>
                  <WorkflowList catalog={app.workflows()} onRetry={() => app.loadWorkflows()} />
                </section>
              </>
            )}
          </Match>
          <Match when={true}>
            <p class="text-sm text-muted">Loading settings…</p>
          </Match>
        </Switch>
        <InstallHint />
        <NotificationSettings />
        {/* Local mode has nobody to sign in, and these routes answer 409 there (api.rs). */}
        <Show when={app.auth().status === "signed_in"}>
          <AccessSettings />
        </Show>
      </div>
    </section>
  );
}
