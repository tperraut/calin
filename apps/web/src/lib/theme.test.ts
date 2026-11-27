import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import bootScript from "../../public/theme.js?raw";
import { loadThemeChoice, saveThemeChoice, THEME_KEY } from "./prefs";
import {
  applyTheme,
  createThemeState,
  pageTheme,
  resolveTheme,
  watchTheme,
  type ThemeChoice,
} from "./theme";

/** A stand-in for `matchMedia(DARK_QUERY)` whose answer the test changes. */
function fakeSystem(dark: boolean) {
  const listeners = new Set<() => void>();
  const list = {
    matches: dark,
    addEventListener: (_type: string, listener: () => void) => listeners.add(listener),
    removeEventListener: (_type: string, listener: () => void) => listeners.delete(listener),
  };
  return {
    list: list as unknown as MediaQueryList,
    set(next: boolean) {
      list.matches = next;
      for (const listener of listeners) listener();
    },
  };
}

const root = (): HTMLElement => document.documentElement;

afterEach(() => {
  delete root().dataset.theme;
  root().style.removeProperty("--bg");
  for (const meta of document.querySelectorAll('meta[name="theme-color"]')) meta.remove();
});

describe("resolveTheme", () => {
  it("follows the system only when the choice is left to it", () => {
    expect(resolveTheme("system", false)).toBe("light");
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("dark", false)).toBe("dark");
  });
});

describe("the stored choice", () => {
  it("is the system's until the reader picks one, and survives a reload", () => {
    expect(loadThemeChoice()).toBe("system");
    saveThemeChoice("dark");
    expect(loadThemeChoice()).toBe("dark");
    expect(localStorage.getItem(THEME_KEY)).toBe('"dark"');
  });

  it("falls back to the system's for anything it does not know", () => {
    localStorage.setItem(THEME_KEY, '"sepia"');
    expect(loadThemeChoice()).toBe("system");
    localStorage.setItem(THEME_KEY, "not json");
    expect(loadThemeChoice()).toBe("system");
  });
});

describe("the theme state", () => {
  it("shows the system's theme by default and follows it when it changes", () => {
    const system = fakeSystem(true);
    const state = createThemeState(document, system.list);
    expect(state.choice()).toBe("system");
    expect(pageTheme()).toBe("dark");
    system.set(false);
    expect(pageTheme()).toBe("light");
  });

  it("applies a choice at once, stores it, and then ignores the system", () => {
    const system = fakeSystem(false);
    const state = createThemeState(document, system.list);
    state.setChoice("dark");
    expect(pageTheme()).toBe("dark");
    expect(loadThemeChoice()).toBe("dark");
    system.set(true);
    system.set(false);
    expect(pageTheme()).toBe("dark");
    state.setChoice("system");
    expect(pageTheme()).toBe("light");
  });

  it("starts from the stored choice", () => {
    saveThemeChoice("light");
    createThemeState(document, fakeSystem(true).list);
    expect(pageTheme()).toBe("light");
  });

  it("works where the system setting cannot be read", () => {
    const state = createThemeState(document, undefined);
    expect(pageTheme()).toBe("light");
    state.setChoice("dark");
    expect(pageTheme()).toBe("dark");
  });
});

describe("applyTheme", () => {
  it("colors the browser's chrome with the page's background", () => {
    const light = document.createElement("meta");
    light.name = "theme-color";
    light.media = "(prefers-color-scheme: light)";
    document.head.append(light);
    root().style.setProperty("--bg", "#141513");
    applyTheme(document, "dark");
    expect(root().dataset.theme).toBe("dark");
    expect(light.content).toBe("#141513");
  });
});

describe("watchTheme", () => {
  it("tells a listener each time the page's theme changes, until stopped", async () => {
    const seen = vi.fn();
    const stop = watchTheme(seen);
    root().dataset.theme = "dark";
    await Promise.resolve();
    expect(seen).toHaveBeenLastCalledWith("dark");
    stop();
    root().dataset.theme = "light";
    await Promise.resolve();
    expect(seen).toHaveBeenCalledTimes(1);
  });
});

describe("public/theme.js, which runs before the first paint", () => {
  const stored: (ThemeChoice | null)[] = [null, "system", "light", "dark"];
  for (const choice of stored) {
    for (const systemDark of [false, true]) {
      it(`agrees with resolveTheme for ${choice ?? "nothing stored"}, system ${systemDark ? "dark" : "light"}`, () => {
        if (choice !== null) saveThemeChoice(choice);
        vi.stubGlobal("matchMedia", () => ({ matches: systemDark }));
        try {
          // The shipped file, run as the browser runs it: a classic script.
          // oxlint-disable-next-line typescript/no-implied-eval
          new Function(bootScript)();
        } finally {
          vi.unstubAllGlobals();
        }
        expect(pageTheme()).toBe(resolveTheme(choice ?? "system", systemDark));
      });
    }
  }
});
