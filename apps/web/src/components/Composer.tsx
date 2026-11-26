import { createSignal, createUniqueId, For, Show } from "solid-js";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import { errorMessage } from "../lib/errors";

/** True for the chord that submits a multi-line field: Ctrl+Enter, or Cmd+Enter on a Mac. */
export const isSubmitChord = (event: KeyboardEvent): boolean =>
  event.key === "Enter" && (event.ctrlKey || event.metaKey);

/**
 * The form that starts a session. `runners` comes from `GET /health`, default
 * first. `onCreate` resolves once the session exists and rejects with the reason.
 */
export function Composer(props: {
  runners: readonly string[];
  disabled?: boolean;
  onCreate: (request: CreateSessionRequest) => Promise<void>;
}) {
  const [prompt, setPrompt] = createSignal("");
  const [runner, setRunner] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const promptId = createUniqueId();
  const runnerId = createUniqueId();
  const errorId = createUniqueId();

  // Until the user picks one, the server's default (the first) is selected.
  const selected = (): string | undefined => runner() || props.runners[0];
  const blocked = (): boolean => props.disabled === true || busy();

  async function submit(): Promise<void> {
    const text = prompt().trim();
    if (text === "" || blocked()) return;
    const request: CreateSessionRequest = { prompt: text };
    const chosen = selected();
    if (chosen !== undefined) request.runner = chosen;
    setBusy(true);
    setError(undefined);
    try {
      await props.onCreate(request);
      setPrompt("");
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
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
      <div class="flex items-center gap-2">
        <label for={runnerId} class="text-sm text-muted">
          Runner
        </label>
        <select
          id={runnerId}
          class="field h-11 w-auto min-w-0 flex-1 px-2 font-mono lg:max-w-48"
          disabled={props.disabled === true || props.runners.length === 0}
          onChange={(event) => setRunner(event.currentTarget.value)}
        >
          <For each={props.runners}>
            {(name) => (
              <option value={name} selected={name === selected()}>
                {name}
              </option>
            )}
          </For>
        </select>
        <button type="submit" class="btn btn-primary ml-auto" disabled={blocked()}>
          {busy() ? "Starting…" : "Start"}
        </button>
      </div>
      <p class="text-xs text-muted">Ctrl+Enter (⌘+Enter on a Mac) starts the session.</p>
    </form>
  );
}
