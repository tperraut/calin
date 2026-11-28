import {
  createSignal,
  createUniqueId,
  For,
  onSettled,
  Show,
  type Accessor,
  type Setter,
} from "solid-js";
import type { SessionRepo } from "../gen/SessionRepo";
import { createMediaQuery, WIDE_QUERY } from "../lib/media";
import { repoModeLabel } from "../lib/status";
import type { PanelProps } from "./registry";
import { TerminalTab, type ShellPhase } from "./terminal/TerminalTab";

/** One socket and one server process each: a handful is plenty for a session. */
const MAX_TERMINALS = 4;

type Shell = { id: number; title: string; sessionRepoId: string | undefined };

const repoName = (repos: readonly SessionRepo[], sessionRepoId: string | undefined): string =>
  repos.find((repo) => repo.id === sessionRepoId)?.name ?? "shell";

/** A shell that is starting or running: closing its tab kills it. */
const isRunning = (phase: ShellPhase): boolean =>
  phase.kind === "open" || phase.kind === "connecting";

/** The open shells, which one shows, and which one waits for a second tap to close. */
class ShellTabs {
  readonly shells: Accessor<readonly Shell[]>;
  readonly activeId: Accessor<number | undefined>;
  readonly closing: Accessor<number | undefined>;
  private readonly setShells: Setter<readonly Shell[]>;
  private readonly setActiveId: Setter<number | undefined>;
  private readonly setClosing: Setter<number | undefined>;
  // Plain mirrors: a setter shows a tick later, and two quick taps must not open a fifth shell.
  private list: readonly Shell[] = [];
  private started = 0;
  private readonly live = new Set<number>();

  constructor() {
    [this.shells, this.setShells] = createSignal<readonly Shell[]>([]);
    [this.activeId, this.setActiveId] = createSignal<number>();
    [this.closing, this.setClosing] = createSignal<number>();
  }

  full(): boolean {
    return this.shells().length >= MAX_TERMINALS;
  }

  /** Opens a tab and shows it; `false` when there is no room for one more. */
  add(name: string, sessionRepoId: string | undefined): boolean {
    if (this.list.length >= MAX_TERMINALS) return false;
    this.started += 1;
    const shell: Shell = { id: this.started, title: `${name} ${this.started}`, sessionRepoId };
    this.list = [...this.list, shell];
    this.setShells(this.list);
    this.setActiveId(shell.id);
    return true;
  }

  show(id: number): void {
    this.setActiveId(id);
  }

  close(id: number): void {
    const index = this.list.findIndex((shell) => shell.id === id);
    if (index === -1) return;
    this.live.delete(id);
    this.list = this.list.toSpliced(index, 1);
    this.setShells(this.list);
    this.setClosing(undefined);
    // The next tab takes the place of the one that showed, or the one before when it was last.
    if (this.activeId() === id) this.setActiveId((this.list.at(index) ?? this.list.at(-1))?.id);
  }

  requestClose(id: number): void {
    // A running shell is killed with its tab: that is worth a second tap.
    if (this.live.has(id)) {
      this.setActiveId(id);
      this.setClosing(id);
    } else this.close(id);
  }

  keep(): void {
    this.setClosing(undefined);
  }

  phase(id: number, phase: ShellPhase): void {
    if (isRunning(phase)) this.live.add(id);
    else this.live.delete(id);
  }
}

function TabStrip(props: { tabs: ShellTabs; onNew: () => void }) {
  return (
    <div class="flex items-stretch border-b border-line bg-surface">
      <ul aria-label="Terminals" class="flex min-w-0 flex-1 overflow-x-auto [scrollbar-width:thin]">
        <For each={props.tabs.shells()} keyed={(shell) => shell.id}>
          {(shell) => (
            <li
              class={[
                "flex shrink-0 items-stretch border-r border-line",
                props.tabs.activeId() === shell().id ? "bg-bg" : "text-muted",
              ]}
            >
              <button
                type="button"
                aria-current={props.tabs.activeId() === shell().id ? "true" : undefined}
                class="flex min-h-11 items-center pr-1 pl-3 font-mono text-sm -outline-offset-2"
                onClick={() => props.tabs.show(shell().id)}
              >
                {shell().title}
              </button>
              <button
                type="button"
                class="flex min-h-11 min-w-11 items-center justify-center -outline-offset-2 hover:text-fg"
                aria-label={`Close ${shell().title}`}
                title="Close"
                onClick={() => props.tabs.requestClose(shell().id)}
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
        disabled={props.tabs.full()}
        title={props.tabs.full() ? `Up to ${MAX_TERMINALS} terminals at once` : undefined}
        onClick={() => props.onNew()}
      >
        New terminal
      </button>
    </div>
  );
}

function RepoChooser(props: {
  repos: readonly SessionRepo[];
  chosen: string | undefined;
  onChoose: (sessionRepoId: string) => void;
  onStart: () => void;
  onCancel: () => void;
}) {
  const repoFieldId = createUniqueId();
  return (
    <form
      aria-label="New terminal"
      class="flex flex-wrap items-end gap-2 border-b border-line bg-sunken p-3"
      onSubmit={(event) => {
        event.preventDefault();
        props.onStart();
      }}
    >
      <label for={repoFieldId} class="flex min-w-0 flex-1 basis-48 flex-col gap-1 text-sm">
        Start the shell in
        <select
          id={repoFieldId}
          class="field h-11 px-2"
          value={props.chosen}
          onChange={(event) => props.onChoose(event.currentTarget.value)}
        >
          <For each={props.repos} keyed={(repo) => repo.id}>
            {(repo) => (
              <option value={repo().id}>
                {repo().name} ({repo().primary ? "primary, " : ""}
                {repoModeLabel(repo().mode).toLowerCase()})
              </option>
            )}
          </For>
        </select>
      </label>
      <button type="button" class="btn" onClick={() => props.onCancel()}>
        Cancel
      </button>
      <button type="submit" class="btn btn-primary">
        Start
      </button>
    </form>
  );
}

function CloseQuestion(props: { onClose: () => void; onKeep: () => void }) {
  return (
    <div
      role="alertdialog"
      aria-label="Close the terminal"
      class="flex flex-wrap items-center gap-2 border-b border-line bg-attention-soft px-3 py-2 text-sm"
    >
      <p class="min-w-0 flex-1 basis-48">Closing this terminal kills its shell.</p>
      <button type="button" class="btn px-3 text-sm" onClick={() => props.onClose()}>
        Close terminal
      </button>
      <button type="button" class="btn btn-primary px-3 text-sm" onClick={() => props.onKeep()}>
        Keep it
      </button>
    </div>
  );
}

/**
 * Shells in the session's folders, on the server, one per tab. Nothing is
 * connected until this panel is first shown; from then on each shell lives
 * until its tab is closed or the session screen goes away.
 */
export default function TerminalPanel(props: PanelProps) {
  const wide = createMediaQuery(WIDE_QUERY);
  const coarse = createMediaQuery("(pointer: coarse)");
  const tabs = new ShellTabs();
  const [choosing, setChoosing] = createSignal(false);
  const [chosenRepo, setChosenRepo] = createSignal<string>();

  function start(sessionRepoId: string | undefined): void {
    if (tabs.add(repoName(props.store.repos(), sessionRepoId), sessionRepoId)) setChoosing(false);
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

  // Opening the panel is asking for a shell.
  onSettled(newTerminal);

  return (
    <div class="flex min-h-0 min-w-0 flex-1 flex-col">
      <TabStrip tabs={tabs} onNew={newTerminal} />
      <Show when={choosing()}>
        <RepoChooser
          repos={props.store.repos()}
          chosen={chosenRepo()}
          onChoose={setChosenRepo}
          onStart={() => start(chosenRepo())}
          onCancel={() => setChoosing(false)}
        />
      </Show>
      <Show when={tabs.closing()}>
        {(id) => <CloseQuestion onClose={() => tabs.close(id())} onKeep={() => tabs.keep()} />}
      </Show>
      <Show when={tabs.shells().length === 0 && !choosing()}>
        <p class="p-6 text-center text-sm text-muted">
          No terminal is open. Start one with New terminal.
        </p>
      </Show>
      <For each={tabs.shells()} keyed={(shell) => shell.id}>
        {(shell) => (
          <TerminalTab
            workspace={props.store.workspace}
            sessionRepoId={shell().sessionRepoId}
            shown={props.active && tabs.activeId() === shell().id}
            compact={!wide() || coarse()}
            onPhase={(phase) => tabs.phase(shell().id, phase)}
          />
        )}
      </For>
    </div>
  );
}
