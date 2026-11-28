import {
  createEffect,
  createSignal,
  For,
  onSettled,
  Show,
  type Accessor,
  type Setter,
} from "solid-js";
import type { TerminalClient, TerminalEnd, TerminalHandlers } from "../../api/terminal";
import { errorMessage } from "../../lib/errors";
import type { SessionWorkspace } from "../../state/sessionStore";
import { controlCode, keysAfterCtrl, keysBeforeCtrl, type ExtraKey } from "./keys";
import { loadTerminalModule } from "./load";
import {
  DEFAULT_SIZE,
  TERMINAL_FONT,
  TERMINAL_FONT_SIZE,
  type TerminalModule,
  type TerminalSize,
  type TerminalView,
} from "./types";

const RESIZE_DELAY_MS = 120;

export type ShellPhase =
  | { kind: "connecting" }
  | { kind: "open" }
  | { kind: "ended"; end: TerminalEnd }
  | { kind: "failed"; message: string };

function endText(end: TerminalEnd): string {
  switch (end.kind) {
    case "exit":
      return end.code === null ? "The shell exited." : `The shell exited with code ${end.code}.`;
    case "error":
      return end.message;
    default:
      return end.opened
        ? "The shell was lost: the connection dropped, and a shell cannot be resumed."
        : "The terminal could not connect to the server.";
  }
}

/** Calls `refit` once `element` has stopped changing size for a moment; returns the stop. */
function watchResize(element: HTMLElement | undefined, refit: () => void): () => void {
  if (!element || typeof ResizeObserver !== "function") return () => {};
  let timer: ReturnType<typeof setTimeout> | undefined;
  const observer = new ResizeObserver(() => {
    clearTimeout(timer);
    timer = setTimeout(refit, RESIZE_DELAY_MS);
  });
  observer.observe(element);
  return () => {
    clearTimeout(timer);
    observer.disconnect();
  };
}

type OpenShell = (size: TerminalSize, handlers: TerminalHandlers) => TerminalClient;

/** One tab's xterm view, bound to its shell's socket, with the sticky Ctrl between the two. */
class ShellBinding {
  readonly phase: Accessor<ShellPhase>;
  readonly ctrl: Accessor<boolean>;
  private readonly setPhase: Setter<ShellPhase>;
  private readonly setCtrl: Setter<boolean>;
  private readonly open: OpenShell;
  private readonly onPhase: (phase: ShellPhase) => void;
  private view: TerminalView | undefined;
  private client: TerminalClient | undefined;
  private size: TerminalSize = DEFAULT_SIZE;
  // Read by the input path on every key, so it is kept beside its signal.
  private ctrlArmed = false;
  private disposed = false;
  private unwatch = (): void => {};

  constructor(open: OpenShell, onPhase: (phase: ShellPhase) => void) {
    this.open = open;
    this.onPhase = onPhase;
    [this.phase, this.setPhase] = createSignal<ShellPhase>({ kind: "connecting" });
    [this.ctrl, this.setCtrl] = createSignal(false);
  }

  ended(): TerminalEnd | undefined {
    const current = this.phase();
    return current.kind === "ended" ? current.end : undefined;
  }

  failure(): string | undefined {
    const current = this.phase();
    return current.kind === "failed" ? current.message : undefined;
  }

  /** Loads xterm into `host` and starts the shell at the size it fits; refits as `scroller` resizes. */
  load(host: HTMLElement | undefined, scroller: HTMLElement | undefined): void {
    loadTerminalModule().then(
      (module) => this.loaded(module, host, scroller),
      (error: unknown) => this.loadFailed(error),
    );
  }

  /** Closes the socket, which kills the shell, and drops the view. */
  dispose(): void {
    this.disposed = true;
    this.unwatch();
    this.client?.close();
    this.client = undefined;
    this.view?.dispose();
    this.view = undefined;
  }

  toggleCtrl(): void {
    this.armCtrl(!this.ctrlArmed);
  }

  /** Everything typed goes through here: the sticky Ctrl applies to the next key, whichever keyboard it comes from. */
  send(data: string): void {
    let out = data;
    if (this.ctrlArmed) {
      this.armCtrl(false);
      out = controlCode(data) ?? data;
    }
    this.client?.write(out);
  }

  press(key: ExtraKey): void {
    this.send(key.sequence(this.view?.applicationCursorKeys() ?? false));
  }

  refit(): void {
    const fitted = this.view?.fit();
    if (!fitted || (fitted.cols === this.size.cols && fitted.rows === this.size.rows)) return;
    this.size = fitted;
    this.client?.resize(fitted.cols, fitted.rows);
  }

  restart(): void {
    this.view?.write("\r\n\x1b[2m--- new shell ---\x1b[0m\r\n");
    this.connect();
    this.view?.focus();
  }

  /** Pastes the clipboard; `false` when it is out of reach (a plain-http address, a refusal). */
  async pasteClipboard(): Promise<boolean> {
    try {
      this.paste(await navigator.clipboard.readText());
      return true;
    } catch {
      return false;
    }
  }

  paste(text: string): void {
    if (text !== "") this.view?.paste(text);
    this.view?.focus();
  }

  private loaded(
    module: TerminalModule,
    host: HTMLElement | undefined,
    scroller: HTMLElement | undefined,
  ): void {
    if (this.disposed || !host) return;
    this.view = module.createTerminalView(host, {
      onInput: (data) => this.send(data),
      onBytes: (bytes) => this.client?.write(bytes),
    });
    this.size = this.view.fit() ?? this.size;
    this.connect();
    this.unwatch = watchResize(scroller, () => this.refit());
  }

  private loadFailed(error: unknown): void {
    if (!this.disposed) this.report({ kind: "failed", message: errorMessage(error) });
  }

  private report(next: ShellPhase): void {
    this.setPhase(next);
    this.onPhase(next);
  }

  private armCtrl(armed: boolean): void {
    this.ctrlArmed = armed;
    this.setCtrl(armed);
  }

  private connect(): void {
    this.client?.close();
    this.report({ kind: "connecting" });
    const opened = this.open(this.size, {
      onOpen: () => this.report({ kind: "open" }),
      onData: (bytes) => this.view?.write(bytes),
      onEnd: (end) => this.closed(opened, end),
    });
    this.client = opened;
  }

  private closed(client: TerminalClient, end: TerminalEnd): void {
    if (this.client === client) this.client = undefined;
    this.report({ kind: "ended", end });
  }
}

// A tap on a key must not take the focus, and with it the on-screen keyboard, from the terminal.
const keepFocus = (event: MouseEvent): void => event.preventDefault();
const keyClass =
  "flex min-h-11 min-w-11 shrink-0 items-center justify-center rounded border border-line bg-surface px-2 font-mono text-sm -outline-offset-2";

function ExtraKeyButton(props: { extra: ExtraKey; onPress: (key: ExtraKey) => void }) {
  return (
    <button
      type="button"
      class={keyClass}
      aria-label={props.extra.label}
      onMouseDown={keepFocus}
      onClick={() => props.onPress(props.extra)}
    >
      <span aria-hidden="true">{props.extra.cap}</span>
    </button>
  );
}

function ExtraKeys(props: {
  ctrl: boolean;
  onPress: (key: ExtraKey) => void;
  onCtrl: () => void;
  onPaste: () => void;
}) {
  return (
    <div
      role="toolbar"
      aria-label="Extra keys"
      class="flex gap-1 overflow-x-auto border-t border-line bg-bg p-1 [scrollbar-width:thin]"
    >
      <For each={keysBeforeCtrl}>
        {(key) => <ExtraKeyButton extra={key} onPress={props.onPress} />}
      </For>
      <button
        type="button"
        class={[keyClass, { "border-accent bg-accent-soft": props.ctrl }]}
        aria-label="Control, for the next key"
        aria-pressed={props.ctrl ? "true" : "false"}
        onMouseDown={keepFocus}
        onClick={() => props.onCtrl()}
      >
        <span aria-hidden="true">Ctrl</span>
      </button>
      <For each={keysAfterCtrl}>
        {(key) => <ExtraKeyButton extra={key} onPress={props.onPress} />}
      </For>
      <button
        type="button"
        class={keyClass}
        onMouseDown={keepFocus}
        onClick={() => props.onPaste()}
      >
        Paste
      </button>
    </div>
  );
}

function PasteForm(props: {
  text: string;
  onText: (text: string) => void;
  onSend: () => void;
  onCancel: () => void;
  fieldRef: (field: HTMLInputElement) => void;
}) {
  return (
    <form
      aria-label="Paste into the terminal"
      class="flex items-center gap-2 border-t border-line bg-bg p-2"
      onSubmit={(event) => {
        event.preventDefault();
        props.onSend();
      }}
    >
      <input
        ref={(element) => props.fieldRef(element)}
        type="text"
        class="field h-11 min-w-0 flex-1 px-3 font-mono"
        aria-label="Text to paste"
        placeholder="Paste here"
        autocomplete="off"
        autocapitalize="off"
        spellcheck={false}
        value={props.text}
        onInput={(event) => props.onText(event.currentTarget.value)}
      />
      <button type="submit" class="btn btn-primary px-3">
        Send
      </button>
      <button type="button" class="btn px-3" onClick={() => props.onCancel()}>
        Cancel
      </button>
    </form>
  );
}

/**
 * One shell: an xterm.js view bound to one terminal socket. It stays mounted
 * while its tab exists, hidden behind the others, so the shell keeps running
 * and its screen is there on the way back. Removing it closes the socket,
 * which kills the shell.
 */
export function TerminalTab(props: {
  workspace: SessionWorkspace;
  /** The repo the shell starts in; the session's working directory when absent. */
  sessionRepoId: string | undefined;
  /** This tab is the one on screen. */
  shown: boolean;
  /** A phone: the row of extra keys shows, and the grid keeps 80 columns, scrolling sideways. */
  compact: boolean;
  onPhase: (phase: ShellPhase) => void;
}) {
  const shell = new ShellBinding(
    (size, handlers) =>
      props.workspace.openTerminal(
        { sessionRepoId: props.sessionRepoId, cols: size.cols, rows: size.rows },
        handlers,
      ),
    (phase) => props.onPhase(phase),
  );
  const [pasting, setPasting] = createSignal(false);
  const [pasteText, setPasteText] = createSignal("");
  let scroller: HTMLDivElement | undefined;
  let host: HTMLDivElement | undefined;
  let pasteField: HTMLInputElement | undefined;

  onSettled(() => {
    shell.load(host, scroller);
    return () => shell.dispose();
  });

  // Back on screen: the pane may have changed size while this tab was hidden.
  createEffect(
    () => props.shown,
    (shown) => {
      if (!shown) return;
      const frame = requestAnimationFrame(() => shell.refit());
      return () => cancelAnimationFrame(frame);
    },
    { defer: true },
  );

  async function paste(): Promise<void> {
    if (await shell.pasteClipboard()) return;
    // No clipboard: a field the reader pastes into instead.
    setPasting(true);
    queueMicrotask(() => pasteField?.focus());
  }

  function sendPasted(): void {
    shell.paste(pasteText());
    setPasteText("");
    setPasting(false);
  }

  return (
    <div hidden={!props.shown} class="flex min-h-0 min-w-0 flex-1 flex-col">
      <Show when={shell.ended()}>
        {(end) => (
          <div
            role="status"
            class="flex flex-wrap items-center gap-2 border-b border-line bg-sunken px-3 py-2 text-sm"
          >
            <p class="min-w-0 flex-1 basis-48">{endText(end())}</p>
            <button type="button" class="btn px-3 text-sm" onClick={() => shell.restart()}>
              Start a new shell
            </button>
          </div>
        )}
      </Show>
      <Show when={shell.failure()}>
        {(message) => (
          <p role="alert" class="border-b border-line bg-danger-soft px-3 py-2 text-sm">
            The terminal could not be loaded: {message()}
          </p>
        )}
      </Show>
      {/* Over the terminal, not above it: a line that comes and goes would resize the grid each time. */}
      <div class="relative flex min-h-0 min-w-0 flex-1 flex-col">
        <Show when={shell.phase().kind === "connecting"}>
          <p
            role="status"
            class="absolute top-1 right-3 z-10 rounded border border-line bg-bg px-2 py-0.5 text-xs text-muted"
          >
            Starting a shell…
          </p>
        </Show>
        <div
          ref={(element) => (scroller = element)}
          // Sideways scrolling is for the phone's fixed 80 columns only: elsewhere the grid is
          // fitted, and a scrollbar flashing in before the first fit would cost a row.
          class={[
            "min-h-0 min-w-0 flex-1 overflow-y-hidden bg-surface",
            props.compact ? "overflow-x-auto" : "overflow-x-hidden",
          ]}
        >
          <div
            ref={(element) => (host = element)}
            class="h-full"
            // In the terminal's own font, so `ch` is one cell: a phone keeps 80
            // columns and scrolls sideways instead of shrinking the text.
            style={{
              "font-family": TERMINAL_FONT,
              "font-size": `${TERMINAL_FONT_SIZE}px`,
              "min-width": props.compact ? "calc(81ch + 24px)" : undefined,
            }}
          />
        </div>
      </div>
      <Show when={pasting()}>
        <PasteForm
          text={pasteText()}
          onText={setPasteText}
          onSend={sendPasted}
          onCancel={() => setPasting(false)}
          fieldRef={(field) => (pasteField = field)}
        />
      </Show>
      <Show when={props.compact}>
        <ExtraKeys
          ctrl={shell.ctrl()}
          onPress={(key) => shell.press(key)}
          onCtrl={() => shell.toggleCtrl()}
          onPaste={() => void paste()}
        />
      </Show>
    </div>
  );
}
