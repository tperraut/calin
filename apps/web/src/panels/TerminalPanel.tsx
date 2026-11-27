import { createSignal, createUniqueId, For, onSettled, Show } from "solid-js";
import { createMediaQuery, WIDE_QUERY } from "../lib/media";
import { repoModeLabel } from "../lib/status";
import type { PanelProps } from "./registry";
import { TerminalTab, type ShellPhase } from "./terminal/TerminalTab";

/** One socket and one server process each: a handful is plenty for a session. */
const MAX_TERMINALS = 4;

type Shell = { id: number; title: string; sessionRepoId: string | undefined };

/**
 * Shells in the session's folders, on the server, one per tab. Nothing is
 * connected until this panel is first shown; from then on each shell lives
 * until its tab is closed or the session screen goes away.
 */
export default function TerminalPanel(props: PanelProps) {
  const wide = createMediaQuery(WIDE_QUERY);
  const coarse = createMediaQuery("(pointer: coarse)");

  const [shells, setShells] = createSignal<readonly Shell[]>([]);
  const [activeId, setActiveId] = createSignal<number>();
  const [choosing, setChoosing] = createSignal(false);
  const [chosenRepo, setChosenRepo] = createSignal<string>();
  const [closing, setClosing] = createSignal<number>();
  const repoFieldId = createUniqueId();

  // Plain mirrors: a setter shows a tick later, and two quick taps must not open a fifth shell.
  let list: readonly Shell[] = [];
  let started = 0;
  const live = new Set<number>();

  function start(sessionRepoId: string | undefined): void {
    if (list.length >= MAX_TERMINALS) return;
    started += 1;
    const name = props.store.repos().find((repo) => repo.id === sessionRepoId)?.name ?? "shell";
    const shell: Shell = { id: started, title: `${name} ${started}`, sessionRepoId };
    list = [...list, shell];
    setShells(list);
    setActiveId(shell.id);
    setChoosing(false);
  }

  /** With several repos the reader says where; otherwise there is nothing to ask. */
  function newTerminal(): void {
    const repos = props.store.repos();
    if (repos.length > 1) {
      setChosenRepo(repos[0].id);
      setChoosing(true);
    } else {
      start(repos.at(0)?.id);
    }
  }

  function close(id: number): void {
    const index = list.findIndex((shell) => shell.id === id);
    if (index === -1) return;
    live.delete(id);
    list = list.toSpliced(index, 1);
    setShells(list);
    setClosing(undefined);
    if (activeId() === id) setActiveId((list.at(index) ?? list.at(-1))?.id);
  }

  function requestClose(id: number): void {
    // A running shell is killed with its tab: that is worth a second tap.
    if (live.has(id)) {
      setActiveId(id);
      setClosing(id);
    } else close(id);
  }

  function onPhase(id: number, phase: ShellPhase): void {
    if (phase.kind === "open" || phase.kind === "connecting") live.add(id);
    else live.delete(id);
  }

  // Opening the panel is asking for a shell.
  onSettled(newTerminal);

  const full = (): boolean => shells().length >= MAX_TERMINALS;

  return (
    <div class="flex min-h-0 min-w-0 flex-1 flex-col">
      <div class="flex items-stretch border-b border-line bg-surface">
        <ul
          aria-label="Terminals"
          class="flex min-w-0 flex-1 overflow-x-auto [scrollbar-width:thin]"
        >
          <For each={shells()} keyed={(shell) => shell.id}>
            {(shell) => (
              <li
                class={[
                  "flex shrink-0 items-stretch border-r border-line",
                  activeId() === shell().id ? "bg-bg" : "text-muted",
                ]}
              >
                <button
                  type="button"
                  aria-current={activeId() === shell().id ? "true" : undefined}
                  class="flex min-h-11 items-center pr-1 pl-3 font-mono text-sm -outline-offset-2"
                  onClick={() => setActiveId(shell().id)}
                >
                  {shell().title}
                </button>
                <button
                  type="button"
                  class="flex min-h-11 min-w-11 items-center justify-center -outline-offset-2 hover:text-fg"
                  aria-label={`Close ${shell().title}`}
                  title="Close"
                  onClick={() => requestClose(shell().id)}
                >
                  <span aria-hidden="true">×</span>
                </button>
              </li>
            )}
          </For>
        </ul>
        <button
          type="button"
          class="btn shrink-0 rounded-none border-0 border-l border-line px-3 text-sm"
          disabled={full()}
          title={full() ? `Up to ${MAX_TERMINALS} terminals at once` : undefined}
          onClick={newTerminal}
        >
          New terminal
        </button>
      </div>
      <Show when={choosing()}>
        <form
          aria-label="New terminal"
          class="flex flex-wrap items-end gap-2 border-b border-line bg-attention-soft p-3"
          onSubmit={(event) => {
            event.preventDefault();
            start(chosenRepo());
          }}
        >
          <label for={repoFieldId} class="flex min-w-0 flex-1 basis-48 flex-col gap-1 text-sm">
            Start the shell in
            <select
              id={repoFieldId}
              class="field h-11 px-2"
              value={chosenRepo()}
              onChange={(event) => setChosenRepo(event.currentTarget.value)}
            >
              <For each={props.store.repos()} keyed={(repo) => repo.id}>
                {(repo) => (
                  <option value={repo().id}>
                    {repo().name} ({repo().primary ? "primary, " : ""}
                    {repoModeLabel(repo().mode).toLowerCase()})
                  </option>
                )}
              </For>
            </select>
          </label>
          <button type="button" class="btn" onClick={() => setChoosing(false)}>
            Cancel
          </button>
          <button type="submit" class="btn btn-primary">
            Start
          </button>
        </form>
      </Show>
      <Show when={closing()}>
        {(id) => (
          <div
            role="alertdialog"
            aria-label="Close the terminal"
            class="flex flex-wrap items-center gap-2 border-b border-line bg-attention-soft px-3 py-2 text-sm"
          >
            <p class="min-w-0 flex-1 basis-48">Closing this terminal kills its shell.</p>
            <button type="button" class="btn px-3 text-sm" onClick={() => close(id())}>
              Close terminal
            </button>
            <button
              type="button"
              class="btn btn-primary px-3 text-sm"
              onClick={() => setClosing(undefined)}
            >
              Keep it
            </button>
          </div>
        )}
      </Show>
      <Show when={shells().length === 0 && !choosing()}>
        <p class="p-6 text-center text-sm text-muted">
          No terminal is open. Start one with New terminal.
        </p>
      </Show>
      <For each={shells()} keyed={(shell) => shell.id}>
        {(shell) => (
          <TerminalTab
            workspace={props.store.workspace}
            sessionRepoId={shell().sessionRepoId}
            shown={props.active && activeId() === shell().id}
            compact={!wide() || coarse()}
            onPhase={(phase) => onPhase(shell().id, phase)}
          />
        )}
      </For>
    </div>
  );
}
