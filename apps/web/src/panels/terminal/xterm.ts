// The one module that touches xterm.js; only the Terminal panel's chunk imports it.
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal, type ITheme } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import "./terminal.css";
import {
  TERMINAL_FONT,
  TERMINAL_FONT_SIZE,
  type TerminalModule,
  type TerminalView,
  type TerminalViewOptions,
} from "./types";

// xterm paints with concrete colors, so the tokens are read from the stylesheet
// (index.css, terminal.css) and read again when the system theme changes.
function theme(): ITheme {
  const style = getComputedStyle(document.documentElement);
  const token = (name: string): string => style.getPropertyValue(name).trim();
  return {
    background: token("--surface"),
    foreground: token("--fg"),
    cursor: token("--fg"),
    cursorAccent: token("--surface"),
    selectionBackground: token("--term-selection"),
    black: token("--term-black"),
    red: token("--term-red"),
    green: token("--term-green"),
    yellow: token("--term-yellow"),
    blue: token("--term-blue"),
    magenta: token("--term-magenta"),
    cyan: token("--term-cyan"),
    white: token("--term-white"),
    brightBlack: token("--term-bright-black"),
    brightRed: token("--term-bright-red"),
    brightGreen: token("--term-bright-green"),
    brightYellow: token("--term-bright-yellow"),
    brightBlue: token("--term-bright-blue"),
    brightMagenta: token("--term-bright-magenta"),
    brightCyan: token("--term-bright-cyan"),
    brightWhite: token("--term-bright-white"),
  };
}

function createTerminalView(parent: HTMLElement, options: TerminalViewOptions): TerminalView {
  const terminal = new Terminal({
    fontFamily: TERMINAL_FONT,
    fontSize: TERMINAL_FONT_SIZE,
    cursorBlink: true,
    scrollback: 5000,
    theme: theme(),
  });
  const fit = new FitAddon();
  terminal.loadAddon(fit);
  terminal.loadAddon(new WebLinksAddon());
  terminal.open(parent);

  const subscriptions = [
    terminal.onData((data) => options.onInput(data)),
    // A "binary string": one char per byte.
    terminal.onBinary((data) => options.onBytes(Uint8Array.from(data, (c) => c.charCodeAt(0)))),
  ];

  const scheme = matchMedia("(prefers-color-scheme: dark)");
  const retheme = (): void => {
    terminal.options.theme = theme();
  };
  scheme.addEventListener("change", retheme);

  return {
    write(data) {
      terminal.write(data);
    },
    fit() {
      const proposed = fit.proposeDimensions();
      if (!proposed || !Number.isFinite(proposed.cols) || !Number.isFinite(proposed.rows)) {
        return undefined;
      }
      fit.fit();
      return { cols: terminal.cols, rows: terminal.rows };
    },
    focus() {
      terminal.focus();
    },
    paste(text) {
      terminal.paste(text);
    },
    applicationCursorKeys: () => terminal.modes.applicationCursorKeysMode,
    dispose() {
      scheme.removeEventListener("change", retheme);
      for (const subscription of subscriptions) subscription.dispose();
      terminal.dispose();
    },
  };
}

const terminalModule: TerminalModule = { createTerminalView };
export default terminalModule;
