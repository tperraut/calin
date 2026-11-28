import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { SessionRepo } from "../gen/SessionRepo";
import { makeSessionRepo } from "../test/fakes";
import {
  fakeTerminals,
  nextView,
  resetFakeTerminals,
  views,
  type FakeShell,
} from "../test/fakeTerminal";
import {
  choose,
  click,
  find,
  findByText,
  mount,
  PHONE,
  setViewportWidth,
  type,
  unmountAll,
  until,
} from "../test/render";
import { act, openWorkspace, type Workspace } from "../test/workspace";
import { loadTerminalModule } from "./terminal/load";
import TerminalPanel from "./TerminalPanel";

vi.mock("./terminal/xterm", async () => ({
  default: (await import("../test/fakeTerminal")).fakeTerminalModule,
}));
// Real by default; a test makes one load fail, as a chunk lost to the network would.
vi.mock("./terminal/load", async (importOriginal) => {
  const real = await importOriginal<typeof import("./terminal/load")>();
  return { loadTerminalModule: vi.fn(real.loadTerminalModule) };
});

const bytes = (text: string): Uint8Array => new TextEncoder().encode(text);

type Opened = Workspace & { host: HTMLElement; shells: FakeShell[] };

async function open(
  repos: SessionRepo[] = [makeSessionRepo("api", { primary: true })],
): Promise<Opened> {
  const terminals = fakeTerminals();
  const workspace = await openWorkspace({ repos, openTerminal: terminals.open });
  const host = mount(() => (
    <TerminalPanel store={workspace.store} active={workspace.active()} links={workspace.links} />
  ));
  await act();
  return { ...workspace, host, shells: terminals.shells };
}

const tabs = (host: HTMLElement): string[] =>
  [...host.querySelectorAll('ul[aria-label="Terminals"] > li > button:first-child')].map(
    (tab) => tab.textContent,
  );
const newTerminal = (host: HTMLElement): HTMLButtonElement =>
  findByText<HTMLButtonElement>(host, "button", "New terminal");
/** The extra key with this accessible name, in the tab on screen. */
const key = (host: HTMLElement, label: string): HTMLButtonElement => {
  for (const button of host.querySelectorAll<HTMLButtonElement>('[role="toolbar"] button')) {
    const name = button.getAttribute("aria-label") ?? button.textContent;
    if (name === label && button.closest("[hidden]") === null) return button;
  }
  throw new Error(`no extra key named ${label}`);
};

// Captured so a test can deliver a resize: happy-dom lays nothing out.
let observed: Array<() => void> = [];
class FakeResizeObserver {
  private readonly callback: () => void;
  constructor(callback: () => void) {
    this.callback = callback;
  }
  observe(): void {
    observed.push(this.callback);
  }
  unobserve(): void {}
  disconnect(): void {
    observed = observed.filter((one) => one !== this.callback);
  }
}

beforeAll(() => loadTerminalModule());
beforeEach(() => {
  resetFakeTerminals();
  observed = [];
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  unmountAll();
});

describe("TerminalPanel", () => {
  it("starts a shell in the session's one repo as soon as the panel opens, at the fitted size", async () => {
    const { host, shells } = await open();
    expect(shells).toHaveLength(1);
    expect(shells[0].request).toEqual({
      sessionId: "s1",
      sessionRepoId: "api",
      cols: 100,
      rows: 30,
    });
    expect(tabs(host)).toEqual(["api 1"]);
    expect(host.textContent).toContain("Starting a shell…");
    shells[0].handlers.onOpen?.();
    await act();
    expect(host.textContent).not.toContain("Starting a shell…");
  });

  it("starts the shell at 80 by 24 when the grid is not laid out yet", async () => {
    nextView.size = undefined;
    const { shells } = await open();
    expect(shells[0].request).toMatchObject({ cols: 80, rows: 24 });
  });

  it("says so when the terminal's code cannot be loaded, and opens no socket", async () => {
    vi.mocked(loadTerminalModule).mockRejectedValueOnce(new Error("chunk lost"));
    const { host, shells } = await open();
    expect(find(host, '[role="alert"]').textContent).toBe(
      "The terminal could not be loaded: chunk lost",
    );
    expect(shells).toHaveLength(0);
  });

  it("refits a tab that comes back on screen, since the pane may have changed meanwhile", async () => {
    const { host, shells } = await open();
    await click(newTerminal(host));
    await act();
    views[0].size = { cols: 90, rows: 20 };
    await click(findByText(host, 'ul[aria-label="Terminals"] button', "api 1"));
    await until(() => shells[0].resizes.length > 0);
    expect(shells[0].resizes).toEqual([{ cols: 90, rows: 20 }]);
    expect(shells[1].resizes).toEqual([]);
  });

  it("starts in the session's own folder when no repo is attached", async () => {
    const { host, shells } = await open([]);
    expect(shells[0].request).toEqual({
      sessionId: "s1",
      sessionRepoId: undefined,
      cols: 100,
      rows: 30,
    });
    expect(tabs(host)).toEqual(["shell 1"]);
  });

  it("asks which repo to start in when there are several, the primary by default", async () => {
    const { host, shells } = await open([
      makeSessionRepo("web", { primary: true }),
      makeSessionRepo("api", { mode: "live" }),
      makeSessionRepo("docs", { mode: "read" }),
    ]);
    // No socket until the reader says where.
    expect(shells).toHaveLength(0);
    const form = find<HTMLFormElement>(host, 'form[aria-label="New terminal"]');
    const select = find<HTMLSelectElement>(form, "select");
    expect([...select.options].map((option) => option.textContent)).toEqual([
      "web (primary, worktree)",
      "api (edit in place)",
      "docs (read only)",
    ]);
    expect(select.value).toBe("web");

    await click(findByText(form, "button", "Start"));
    await act();
    expect(shells.map((shell) => shell.request.sessionRepoId)).toEqual(["web"]);
    expect(host.querySelector('form[aria-label="New terminal"]')).toBeNull();

    await click(newTerminal(host));
    choose(find<HTMLSelectElement>(host, "select"), "api");
    await click(findByText(host, "button", "Start"));
    await act();
    expect(shells.map((shell) => shell.request.sessionRepoId)).toEqual(["web", "api"]);
    expect(tabs(host)).toEqual(["web 1", "api 2"]);

    await click(newTerminal(host));
    await click(findByText(host, "button", "Cancel"));
    expect(shells).toHaveLength(2);
  });

  it("carries bytes both ways", async () => {
    const { shells } = await open();
    const [shell] = shells;
    const [view] = views;
    shell.handlers.onOpen?.();
    shell.handlers.onData(bytes("api $ "));
    shell.handlers.onData(bytes("héllo\r\n"));
    expect(view.screen).toBe("api $ héllo\r\n");

    view.options.onInput("ls\r");
    view.options.onBytes(new Uint8Array([0x1b, 0x5b, 0x4d]));
    expect(shell.input).toBe("ls\r\x1b[M");
  });

  it("sends a resize, debounced, when the container changes size, and only when the grid did", async () => {
    const { shells } = await open();
    const [shell] = shells;
    const [view] = views;
    shell.handlers.onOpen?.();
    expect(observed).toHaveLength(1);
    vi.useFakeTimers();

    view.size = { cols: 120, rows: 40 };
    observed[0]();
    vi.advanceTimersByTime(60);
    view.size = { cols: 132, rows: 43 };
    observed[0]();
    vi.advanceTimersByTime(119);
    expect(shell.resizes).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(shell.resizes).toEqual([{ cols: 132, rows: 43 }]);

    // Same grid (the pane moved by a few pixels), or not laid out: nothing to say.
    observed[0]();
    vi.advanceTimersByTime(200);
    view.size = undefined;
    observed[0]();
    vi.advanceTimersByTime(200);
    expect(shell.resizes).toHaveLength(1);
  });

  it("shows the exit code where the shell was, and starts a new shell in place", async () => {
    const { host, shells } = await open();
    shells[0].handlers.onOpen?.();
    shells[0].handlers.onData(bytes("bye\r\n"));
    shells[0].handlers.onEnd({ kind: "exit", code: 3 });
    await act();
    const status = find(host, '[role="status"]');
    expect(status.textContent).toContain("The shell exited with code 3.");

    await click(findByText(status, "button", "Start a new shell"));
    await act();
    expect(shells).toHaveLength(2);
    expect(shells[1].request).toMatchObject({ sessionRepoId: "api", cols: 100, rows: 30 });
    // Same tab, same screen: what the old shell printed is still there.
    expect(tabs(host)).toEqual(["api 1"]);
    expect(views).toHaveLength(1);
    expect(views[0].screen).toContain("bye");
    expect(host.textContent).not.toContain("The shell exited");

    views[0].options.onInput("x");
    expect(shells[0].input).toBe("");
    expect(shells[1].input).toBe("x");
  });

  it("says the shell exited without a code when it has none", async () => {
    const { host, shells } = await open();
    shells[0].handlers.onEnd({ kind: "exit", code: null });
    await act();
    expect(find(host, '[role="status"] p').textContent).toBe("The shell exited.");
  });

  it("says the shell was lost when the socket drops, and that it cannot be resumed", async () => {
    const { host, shells } = await open();
    shells[0].handlers.onOpen?.();
    shells[0].handlers.onEnd({ kind: "lost", opened: true });
    await act();
    expect(find(host, '[role="status"]').textContent).toContain(
      "The shell was lost: the connection dropped, and a shell cannot be resumed.",
    );
    expect(findByText(host, "button", "Start a new shell")).toBeDefined();
  });

  it("shows the message of an error frame, and says when the socket never connected", async () => {
    const { host, shells } = await open();
    shells[0].handlers.onEnd({ kind: "error", message: "the shell could not be started: no pty" });
    await act();
    expect(find(host, '[role="status"]').textContent).toContain(
      "the shell could not be started: no pty",
    );

    await click(findByText(host, "button", "Start a new shell"));
    shells[1].handlers.onEnd({ kind: "lost", opened: false });
    await act();
    expect(find(host, '[role="status"]').textContent).toContain(
      "The terminal could not connect to the server.",
    );
  });

  it("opens up to four terminals, each its own shell, and shows one at a time", async () => {
    const { host, shells } = await open();
    for (let more = 0; more < 3; more += 1) await click(newTerminal(host));
    await act();
    expect(tabs(host)).toEqual(["api 1", "api 2", "api 3", "api 4"]);
    expect(shells).toHaveLength(4);
    expect(newTerminal(host).disabled).toBe(true);
    expect(newTerminal(host).title).toBe("Up to 4 terminals at once");

    const shown = (): number[] =>
      views.flatMap((view, index) => (view.element.closest("[hidden]") === null ? [index] : []));
    expect(shown()).toEqual([3]);
    await click(findByText(host, 'ul[aria-label="Terminals"] button', "api 2"));
    expect(shown()).toEqual([1]);
    // The others keep running behind it.
    shells[3].handlers.onData(bytes("still here"));
    expect(views[3].screen).toBe("still here");
    expect(views.every((view) => !view.disposed)).toBe(true);
  });

  it("asks before closing a terminal whose shell is running, then kills it", async () => {
    const { host, shells } = await open();
    await click(newTerminal(host));
    await act();
    shells[0].handlers.onOpen?.();
    shells[1].handlers.onEnd({ kind: "exit", code: 0 });
    await act();

    // An ended shell has nothing to lose.
    await click(find(host, 'button[aria-label="Close api 2"]'));
    expect(tabs(host)).toEqual(["api 1"]);
    expect(views[1].disposed).toBe(true);

    await click(find(host, 'button[aria-label="Close api 1"]'));
    const question = find(host, '[role="alertdialog"]');
    expect(question.textContent).toContain("Closing this terminal kills its shell.");
    await click(findByText(question, "button", "Keep it"));
    expect(shells[0].closed).toBe(false);

    await click(find(host, 'button[aria-label="Close api 1"]'));
    await click(findByText(host, "button", "Close terminal"));
    expect(shells[0].closed).toBe(true);
    expect(views[0].disposed).toBe(true);
    expect(tabs(host)).toEqual([]);
    expect(host.textContent).toContain("No terminal is open.");
    expect(newTerminal(host).disabled).toBe(false);
  });

  it("shows the next tab when the one on screen closes, or the one before when it was last", async () => {
    const { host, shells } = await open();
    for (let more = 0; more < 3; more += 1) await click(newTerminal(host));
    await act();
    for (const shell of shells) shell.handlers.onEnd({ kind: "exit", code: 0 });
    await act();
    const current = (): string | undefined =>
      host.querySelector('ul[aria-label="Terminals"] button[aria-current="true"]')?.textContent;

    await click(findByText(host, 'ul[aria-label="Terminals"] button', "api 2"));
    await click(find(host, 'button[aria-label="Close api 2"]'));
    expect(current()).toBe("api 3");
    await click(findByText(host, 'ul[aria-label="Terminals"] button', "api 4"));
    await click(find(host, 'button[aria-label="Close api 4"]'));
    expect(current()).toBe("api 3");
    // Another tab than the one on screen: the screen stays.
    await click(find(host, 'button[aria-label="Close api 1"]'));
    expect(current()).toBe("api 3");
    await click(find(host, 'button[aria-label="Close api 3"]'));
    expect(current()).toBeUndefined();
  });

  it("closes every socket when the panel goes away", async () => {
    const { host, shells } = await open();
    await click(newTerminal(host));
    await act();
    unmountAll();
    expect(shells.map((shell) => shell.closed)).toEqual([true, true]);
    expect(views.map((view) => view.disposed)).toEqual([true, true]);
  });

  describe("on a phone", () => {
    beforeEach(() => setViewportWidth(PHONE));

    it("offers the keys a phone keyboard lacks, and sends what they stand for", async () => {
      const { host, shells } = await open();
      const [shell] = shells;
      shell.handlers.onOpen?.();
      const row = find(host, '[role="toolbar"]');
      expect(row.getAttribute("aria-label")).toBe("Extra keys");
      expect([...row.querySelectorAll("button")].map((button) => button.textContent)).toEqual([
        "Esc",
        "Tab",
        "Ctrl",
        "←",
        "↑",
        "↓",
        "→",
        "|",
        "/",
        "-",
        "~",
        "Paste",
      ]);

      for (const label of [
        "Escape",
        "Tab",
        "Up arrow",
        "Left arrow",
        "Pipe",
        "Slash",
        "Dash",
        "Tilde",
      ]) {
        await click(key(host, label));
      }
      expect(shell.input).toBe("\x1b\t\x1b[A\x1b[D|/-~");

      // A full-screen program switches the arrows to application mode.
      views[0].applicationCursorKeys = true;
      await click(key(host, "Down arrow"));
      expect(shell.input.endsWith("\x1bOB")).toBe(true);
    });

    it("keeps the focus, and so the on-screen keyboard, on the terminal when a key is tapped", async () => {
      const { host } = await open();
      const press = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
      key(host, "Tab").dispatchEvent(press);
      expect(press.defaultPrevented).toBe(true);
    });

    it("applies Ctrl to the next key only, from the row or from the keyboard", async () => {
      const { host, shells } = await open();
      const [shell] = shells;
      shell.handlers.onOpen?.();
      const ctrl = key(host, "Control, for the next key");
      expect(ctrl.getAttribute("aria-pressed")).toBe("false");

      await click(ctrl);
      expect(ctrl.getAttribute("aria-pressed")).toBe("true");
      views[0].options.onInput("c");
      await act();
      expect(shell.input).toBe("\x03");
      expect(ctrl.getAttribute("aria-pressed")).toBe("false");
      views[0].options.onInput("c");
      expect(shell.input).toBe("\x03c");

      // Tapped twice, it is off again.
      await click(ctrl);
      await click(ctrl);
      views[0].options.onInput("d");
      expect(shell.input).toBe("\x03cd");

      await click(ctrl);
      await click(key(host, "Dash"));
      expect(shell.input).toBe("\x03cd\x1f");

      // Ctrl does nothing to a tilde: it goes out as typed, and Ctrl is spent.
      await click(ctrl);
      await click(key(host, "Tilde"));
      expect(shell.input).toBe("\x03cd\x1f~");
      expect(ctrl.getAttribute("aria-pressed")).toBe("false");
    });

    it("pastes from the clipboard, or through a field when the clipboard is out of reach", async () => {
      const { host, shells } = await open();
      const [shell] = shells;
      shell.handlers.onOpen?.();
      const readText = vi.fn().mockResolvedValue("echo pasted");
      vi.stubGlobal("navigator", { clipboard: { readText } });
      await click(key(host, "Paste"));
      await act();
      expect(views[0].pasted).toEqual(["echo pasted"]);
      expect(shell.input).toBe("echo pasted");

      // A plain-http address has no clipboard API; a refusal looks the same.
      readText.mockRejectedValue(new Error("denied"));
      await click(key(host, "Paste"));
      await act();
      const form = find<HTMLFormElement>(host, 'form[aria-label="Paste into the terminal"]');
      type(find<HTMLInputElement>(form, "input"), "ls -la");
      await click(findByText(form, "button", "Send"));
      expect(views[0].pasted).toEqual(["echo pasted", "ls -la"]);
      expect(host.querySelector('form[aria-label="Paste into the terminal"]')).toBeNull();
    });

    it("pastes nothing from an empty clipboard or an empty field, and gives the terminal the focus back", async () => {
      const { host, shells } = await open();
      shells[0].handlers.onOpen?.();
      const readText = vi.fn().mockResolvedValue("");
      vi.stubGlobal("navigator", { clipboard: { readText } });
      await click(key(host, "Paste"));
      await act();
      expect(views[0].pasted).toEqual([]);
      expect(views[0].focused).toBe(1);

      readText.mockRejectedValue(new Error("denied"));
      await click(key(host, "Paste"));
      await act();
      const form = find<HTMLFormElement>(host, 'form[aria-label="Paste into the terminal"]');
      expect(document.activeElement).toBe(find(form, "input"));
      await click(findByText(form, "button", "Send"));
      expect(views[0].pasted).toEqual([]);
      expect(views[0].focused).toBe(2);
      expect(host.querySelector('form[aria-label="Paste into the terminal"]')).toBeNull();

      await click(key(host, "Paste"));
      await act();
      type(find<HTMLInputElement>(host, 'input[aria-label="Text to paste"]'), "kept out");
      await click(findByText(host, "button", "Cancel"));
      expect(host.querySelector('form[aria-label="Paste into the terminal"]')).toBeNull();
      expect(views[0].pasted).toEqual([]);
      expect(shells[0].input).toBe("");
    });

    it("keeps 80 columns by letting the grid scroll sideways instead of shrinking the text", async () => {
      await open();
      const cell = views[0].element.parentElement;
      expect(cell?.style.minWidth).toContain("81ch");
      expect(cell?.style.fontSize).toBe("13px");
      expect(cell?.parentElement?.className).toContain("overflow-x-auto");
    });
  });

  it("has no extra keys and no minimum width with a keyboard and a wide screen", async () => {
    const { host } = await open();
    expect(host.querySelector('[role="toolbar"]')).toBeNull();
    expect(views[0].element.parentElement?.style.minWidth).toBe("");
  });
});
