import { createEffect, createSignal, For, onSettled, Show } from "solid-js";
import type { TerminalClient, TerminalEnd } from "../../api/terminal";
import { errorMessage } from "../../lib/errors";
import type { SessionWorkspace } from "../../state/sessionStore";
import { controlCode, keysAfterCtrl, keysBeforeCtrl, type ExtraKey } from "./keys";
import { loadTerminalModule } from "./load";
import {
  DEFAULT_SIZE,
  TERMINAL_FONT,
  TERMINAL_FONT_SIZE,
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
  const [phase, setPhase] = createSignal<ShellPhase>({ kind: "connecting" });
  const [ctrl, setCtrl] = createSignal(false);
  const [pasting, setPasting] = createSignal(false);
  const [pasteText, setPasteText] = createSignal("");
  let scroller: HTMLDivElement | undefined;
  let host: HTMLDivElement | undefined;
  let pasteField: HTMLInputElement | undefined;

  let view: TerminalView | undefined;
  let client: TerminalClient | undefined;
  let size: TerminalSize = DEFAULT_SIZE;
  // Read by the input path on every key, so it is kept beside its signal.
  let ctrlArmed = false;

  function report(next: ShellPhase): void {
    setPhase(next);
    props.onPhase(next);
  }

  function armCtrl(armed: boolean): void {
    ctrlArmed = armed;
    setCtrl(armed);
  }

  /** Everything typed goes through here: the sticky Ctrl applies to the next key, whichever keyboard it comes from. */
  function send(data: string): void {
    let out = data;
    if (ctrlArmed) {
      armCtrl(false);
      out = controlCode(data) ?? data;
    }
    client?.write(out);
  }

  function connect(): void {
    client?.close();
    report({ kind: "connecting" });
    const opened = props.workspace.openTerminal(
      { sessionRepoId: props.sessionRepoId, cols: size.cols, rows: size.rows },
      {
        onOpen: () => report({ kind: "open" }),
        onData: (bytes) => view?.write(bytes),
        onEnd(end) {
          if (client === opened) client = undefined;
          report({ kind: "ended", end });
        },
      },
    );
    client = opened;
  }

  function refit(): void {
    const fitted = view?.fit();
    if (!fitted || (fitted.cols === size.cols && fitted.rows === size.rows)) return;
    size = fitted;
    client?.resize(fitted.cols, fitted.rows);
  }

  onSettled(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let observer: ResizeObserver | undefined;

    loadTerminalModule().then(
      (module) => {
        if (disposed || !host) return;
        view = module.createTerminalView(host, {
          onInput: send,
          onBytes: (bytes) => client?.write(bytes),
        });
        size = view.fit() ?? size;
        connect();
        if (scroller && typeof ResizeObserver === "function") {
          observer = new ResizeObserver(() => {
            clearTimeout(timer);
            timer = setTimeout(refit, RESIZE_DELAY_MS);
          });
          observer.observe(scroller);
        }
      },
      (error: unknown) => {
        if (!disposed) report({ kind: "failed", message: errorMessage(error) });
      },
    );

    return () => {
      disposed = true;
      clearTimeout(timer);
      observer?.disconnect();
      client?.close();
      client = undefined;
      view?.dispose();
      view = undefined;
    };
  });

  // Back on screen: the pane may have changed size while this tab was hidden.
  createEffect(
    () => props.shown,
    (shown) => {
      if (!shown) return;
      const frame = requestAnimationFrame(refit);
      return () => cancelAnimationFrame(frame);
    },
    { defer: true },
  );

  function restart(): void {
    view?.write("\r\n\x1b[2m--- new shell ---\x1b[0m\r\n");
    connect();
    view?.focus();
  }

  function press(key: ExtraKey): void {
    send(key.sequence(view?.applicationCursorKeys() ?? false));
  }

  async function paste(): Promise<void> {
    try {
      const text = await navigator.clipboard.readText();
      if (text !== "") view?.paste(text);
      view?.focus();
    } catch {
      // No clipboard access (a plain-http address, a refusal): a field the reader pastes into instead.
      setPasting(true);
      queueMicrotask(() => pasteField?.focus());
    }
  }

  const ended = () => {
    const current = phase();
    return current.kind === "ended" ? current.end : undefined;
  };
  const failed = () => {
    const current = phase();
    return current.kind === "failed" ? current.message : undefined;
  };

  // A tap on a key must not take the focus, and with it the on-screen keyboard, from the terminal.
  const keepFocus = (event: MouseEvent): void => event.preventDefault();
  const keyClass =
    "flex min-h-11 min-w-11 shrink-0 items-center justify-center rounded border border-line bg-surface px-2 font-mono text-sm -outline-offset-2";

  return (
    <div hidden={!props.shown} class="flex min-h-0 min-w-0 flex-1 flex-col">
      <Show when={ended()}>
        {(end) => (
          <div
            role="status"
            class="flex flex-wrap items-center gap-2 border-b border-line bg-sunken px-3 py-2 text-sm"
          >
            <p class="min-w-0 flex-1 basis-48">{endText(end())}</p>
            <button type="button" class="btn px-3 text-sm" onClick={restart}>
              Start a new shell
            </button>
          </div>
        )}
      </Show>
      <Show when={failed()}>
        {(message) => (
          <p role="alert" class="border-b border-line bg-danger-soft px-3 py-2 text-sm">
            The terminal could not be loaded: {message()}
          </p>
        )}
      </Show>
      {/* Over the terminal, not above it: a line that comes and goes would resize the grid each time. */}
      <div class="relative flex min-h-0 min-w-0 flex-1 flex-col">
        <Show when={phase().kind === "connecting"}>
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
        <form
          aria-label="Paste into the terminal"
          class="flex items-center gap-2 border-t border-line bg-bg p-2"
          onSubmit={(event) => {
            event.preventDefault();
            if (pasteText() !== "") view?.paste(pasteText());
            setPasteText("");
            setPasting(false);
            view?.focus();
          }}
        >
          <input
            ref={(element) => (pasteField = element)}
            type="text"
            class="field h-11 min-w-0 flex-1 px-3 font-mono"
            aria-label="Text to paste"
            placeholder="Paste here"
            autocomplete="off"
            autocapitalize="off"
            spellcheck={false}
            value={pasteText()}
            onInput={(event) => setPasteText(event.currentTarget.value)}
          />
          <button type="submit" class="btn btn-primary px-3">
            Send
          </button>
          <button type="button" class="btn px-3" onClick={() => setPasting(false)}>
            Cancel
          </button>
        </form>
      </Show>
      <Show when={props.compact}>
        <div
          role="toolbar"
          aria-label="Extra keys"
          class="flex gap-1 overflow-x-auto border-t border-line bg-bg p-1 [scrollbar-width:thin]"
        >
          <For each={keysBeforeCtrl}>
            {(key) => (
              <button
                type="button"
                class={keyClass}
                aria-label={key.label}
                onMouseDown={keepFocus}
                onClick={() => press(key)}
              >
                <span aria-hidden="true">{key.cap}</span>
              </button>
            )}
          </For>
          <button
            type="button"
            class={[keyClass, { "border-accent bg-accent-soft": ctrl() }]}
            aria-label="Control, for the next key"
            aria-pressed={ctrl() ? "true" : "false"}
            onMouseDown={keepFocus}
            onClick={() => armCtrl(!ctrlArmed)}
          >
            <span aria-hidden="true">Ctrl</span>
          </button>
          <For each={keysAfterCtrl}>
            {(key) => (
              <button
                type="button"
                class={keyClass}
                aria-label={key.label}
                onMouseDown={keepFocus}
                onClick={() => press(key)}
              >
                <span aria-hidden="true">{key.cap}</span>
              </button>
            )}
          </For>
          <button
            type="button"
            class={keyClass}
            onMouseDown={keepFocus}
            onClick={() => void paste()}
          >
            Paste
          </button>
        </div>
      </Show>
    </div>
  );
}
