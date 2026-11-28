// The real xterm.js, under happy-dom: it draws with the DOM renderer, so what it
// shows and sends can be read back, though nothing is laid out.
import { afterEach, describe, expect, it } from "vite-plus/test";
import { until } from "../../test/render";
import type { TerminalView } from "./types";
import terminalModule from "./xterm";

type Opened = { parent: HTMLElement; view: TerminalView; input: string[] };

let opened: Opened[] = [];

function openView(): Opened {
  const parent = document.createElement("div");
  document.body.append(parent);
  const input: string[] = [];
  const view = terminalModule.createTerminalView(parent, {
    onInput: (data) => input.push(data),
    onBytes: () => {},
  });
  const one = { parent, view, input };
  opened.push(one);
  return one;
}

const background = (parent: HTMLElement): string | undefined =>
  parent.querySelector<HTMLElement>(".xterm-scrollable-element")?.style.backgroundColor;

const root = document.documentElement;

afterEach(() => {
  for (const { view, parent } of opened) {
    view.dispose();
    parent.remove();
  }
  opened = [];
  root.style.removeProperty("--surface");
  delete root.dataset.theme;
});

describe("terminal view", () => {
  it("paints with the page's colors, and again when the theme changes", async () => {
    root.style.setProperty("--surface", "#102030");
    const { parent } = openView();
    expect(background(parent)).toBe("#102030");

    root.style.setProperty("--surface", "#405060");
    root.dataset.theme = "dark";
    await until(() => background(parent) === "#405060");
  });

  it("sends a paste as input, bracketed once the program asks for it", async () => {
    const { view, input } = openView();
    view.paste("ls");
    expect(input).toEqual(["ls"]);

    // Application arrows in the same write: a sign that the write was read.
    view.write("\x1b[?2004h\x1b[?1h");
    await until(() => view.applicationCursorKeys());
    view.paste("pwd");
    expect(input).toEqual(["ls", "\x1b[200~pwd\x1b[201~"]);
  });

  it("tells when the running program switches the arrows to application mode", async () => {
    const { view } = openView();
    expect(view.applicationCursorKeys()).toBe(false);
    view.write(new TextEncoder().encode("\x1b[?1h"));
    await until(() => view.applicationCursorKeys());
    view.write("\x1b[?1l");
    await until(() => !view.applicationCursorKeys());
  });

  it("has no size to offer while its element is not laid out", () => {
    const { view } = openView();
    expect(view.fit()).toBeUndefined();
  });

  it("takes the focus into its keyboard field", () => {
    const { parent, view } = openView();
    view.focus();
    expect(document.activeElement).toBe(parent.querySelector("textarea"));
  });

  it("leaves its element empty once disposed", () => {
    const { parent, view } = openView();
    view.dispose();
    opened = [];
    expect(parent.querySelector(".xterm")).toBeNull();
  });
});
