import { createUniqueId, For, Show } from "solid-js";
import type { PermissionMode } from "../gen/PermissionMode";
import type { WorkflowInfo } from "../gen/WorkflowInfo";
import { permissionModeLook } from "../lib/status";
import { dollars, isUsable, QUICK_WORKFLOW, shapeSummary } from "../lib/workflow";

// The controls that say how a session runs, shared by the new-session form and
// the automation editor so both offer the same choices in the same words.

/** True when there is a workflow besides the built-in one: only then is the choice worth a control. */
export const offersWorkflows = (workflows: readonly WorkflowInfo[]): boolean =>
  workflows.some((one) => one.id !== QUICK_WORKFLOW);

/** The workflow select with what the chosen one does; renders nothing without a choice to make. */
export function WorkflowField(props: {
  workflows: readonly WorkflowInfo[];
  picked: WorkflowInfo | undefined;
  disabled?: boolean;
  onChange: (id: string) => void;
  /** Lets a form focus the select, as when the server refused the workflow. */
  ref?: (element: HTMLSelectElement) => void;
}) {
  const workflowId = createUniqueId();
  const workflowNoteId = createUniqueId();
  return (
    <Show when={offersWorkflows(props.workflows)}>
      <div class="flex flex-col gap-1.5">
        <label for={workflowId} class="text-sm font-medium">
          Workflow
        </label>
        <select
          id={workflowId}
          ref={(element) => props.ref?.(element)}
          class="field h-11 px-2"
          aria-describedby={workflowNoteId}
          disabled={props.disabled === true}
          onChange={(event) => props.onChange(event.currentTarget.value)}
        >
          <For each={props.workflows} keyed={(one) => one.id}>
            {(one) => (
              <option
                value={one().id}
                disabled={!isUsable(one())}
                selected={one().id === props.picked?.id}
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
          <Show when={props.picked}>
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
  );
}

/** One radio per permission mode, each with what it means. `note` says why some are missing. */
export function PermissionField(props: {
  modes: readonly PermissionMode[];
  value: PermissionMode;
  onChange: (mode: PermissionMode) => void;
  note?: string;
  /** Lets a form focus the first radio, as when the server refused the mode. */
  ref?: (element: HTMLInputElement) => void;
}) {
  const modeGroup = createUniqueId();
  const noteId = createUniqueId();
  return (
    <fieldset
      class="flex flex-col gap-1.5"
      aria-describedby={props.note === undefined ? undefined : noteId}
    >
      <legend class="mb-1.5 text-sm font-medium">Permissions</legend>
      <Show when={props.note}>
        {(note) => (
          <p id={noteId} class="text-xs text-muted">
            {note()}
          </p>
        )}
      </Show>
      <For each={props.modes}>
        {(mode, index) => (
          <label class="choice items-start gap-3 py-2">
            <input
              type="radio"
              name={modeGroup}
              value={mode}
              ref={(element) => {
                if (index() === 0) props.ref?.(element);
              }}
              class="mt-0.5 size-5 shrink-0 accent-accent"
              checked={props.value === mode}
              onChange={() => props.onChange(mode)}
            />
            <span class="min-w-0">
              <span class="block font-medium">{permissionModeLook(mode).label}</span>
              <span class="block text-xs text-muted">{permissionModeLook(mode).hint}</span>
            </span>
          </label>
        )}
      </For>
    </fieldset>
  );
}

/** The runner and the model, folded behind "Options" with what is chosen in its summary. */
export function RunnerOptions(props: {
  runners: readonly string[];
  /** The runner in use: the chosen one, or the server's default. */
  runner: string | undefined;
  model: string;
  disabled?: boolean;
  onRunner: (runner: string) => void;
  onModel: (model: string) => void;
}) {
  const runnerId = createUniqueId();
  const modelId = createUniqueId();
  return (
    <details class="group">
      <summary class="flex min-h-11 items-center gap-1.5 text-sm">
        <span aria-hidden="true" class="text-muted transition-transform group-open:rotate-90">
          ▸
        </span>
        <span class="font-medium">Options</span>
        <span class="truncate font-mono text-xs text-muted group-open:hidden">
          {props.runner}
          {props.model.trim() === "" ? "" : ` · ${props.model.trim()}`}
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
          onChange={(event) => props.onRunner(event.currentTarget.value)}
        >
          <For each={props.runners}>
            {(name) => (
              <option value={name} selected={name === props.runner}>
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
          value={props.model}
          onInput={(event) => props.onModel(event.currentTarget.value)}
        />
      </div>
    </details>
  );
}
