// Stands in for `panels/terminal/xterm.ts` and for the terminal socket in tests.
import type {
  OpenTerminal,
  TerminalBytes,
  TerminalHandlers,
  TerminalRequest,
} from "../api/terminal";
import type {
  TerminalModule,
  TerminalSize,
  TerminalView,
  TerminalViewOptions,
} from "../panels/terminal/types";

export type FakeTerminalView = {
  options: TerminalViewOptions;
  element: HTMLElement;
  /** Everything written to the screen, bytes decoded. */
  screen: string;
  /** What `fit()` answers; a test changes it before triggering a resize. */
  size: TerminalSize | undefined;
  pasted: string[];
  applicationCursorKeys: boolean;
  focused: number;
  disposed: boolean;
};

export const views: FakeTerminalView[] = [];

export function resetFakeTerminals(): void {
  views.length = 0;
}

function createTerminalView(parent: HTMLElement, options: TerminalViewOptions): TerminalView {
  const element = document.createElement("pre");
  element.dataset.terminal = "";
  parent.append(element);
  const decoder = new TextDecoder();
  const view: FakeTerminalView = {
    options,
    element,
    screen: "",
    size: { cols: 100, rows: 30 },
    pasted: [],
    applicationCursorKeys: false,
    focused: 0,
    disposed: false,
  };
  views.push(view);
  return {
    write(data) {
      view.screen += typeof data === "string" ? data : decoder.decode(data);
      element.textContent = view.screen;
    },
    fit: () => view.size,
    focus() {
      view.focused += 1;
    },
    paste(text) {
      view.pasted.push(text);
      // xterm turns a paste into input.
      options.onInput(text);
    },
    applicationCursorKeys: () => view.applicationCursorKeys,
    dispose() {
      view.disposed = true;
      element.remove();
    },
  };
}

export const fakeTerminalModule: TerminalModule = { createTerminalView };

export type FakeShell = {
  request: TerminalRequest;
  handlers: TerminalHandlers;
  /** Input received, text and bytes alike, as text. */
  input: string;
  resizes: TerminalSize[];
  closed: boolean;
};

/** An `OpenTerminal` that connects nothing and records every shell asked for. */
export function fakeTerminals(): { open: OpenTerminal; shells: FakeShell[] } {
  const shells: FakeShell[] = [];
  const decoder = new TextDecoder();
  return {
    shells,
    open(request, handlers) {
      const shell: FakeShell = { request, handlers, input: "", resizes: [], closed: false };
      shells.push(shell);
      return {
        write(data: string | TerminalBytes) {
          shell.input += typeof data === "string" ? data : decoder.decode(data);
        },
        resize(cols, rows) {
          shell.resizes.push({ cols, rows });
        },
        close() {
          shell.closed = true;
        },
      };
    },
  };
}
