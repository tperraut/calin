// Light or dark: the reader's choice, or the system's when they leave it to the system.
// `public/theme.js` applies the stored choice before the first paint, so its key and
// values must stay in step with `prefs.ts` and `resolveTheme` (theme.test.ts checks).
import { createSignal, type Accessor } from "solid-js";
import { loadThemeChoice, saveThemeChoice } from "./prefs";

export type ThemeChoice = "system" | "light" | "dark";
export type Theme = "light" | "dark";

export const themeChoices: readonly { choice: ThemeChoice; label: string }[] = [
  { choice: "system", label: "System" },
  { choice: "light", label: "Light" },
  { choice: "dark", label: "Dark" },
];

export const DARK_QUERY = "(prefers-color-scheme: dark)";

export const resolveTheme = (choice: ThemeChoice, systemDark: boolean): Theme =>
  choice === "system" ? (systemDark ? "dark" : "light") : choice;

/** The theme the page shows: the attribute every stylesheet keys its dark tokens on. */
export const pageTheme = (root: HTMLElement = document.documentElement): Theme =>
  root.dataset.theme === "dark" ? "dark" : "light";

/**
 * Shows `theme`: the attribute the stylesheets key on, and the browser's own UI
 * (address bar, status bar of the installed app) in the page's background color.
 */
export function applyTheme(doc: Document, theme: Theme): void {
  const root = doc.documentElement;
  root.dataset.theme = theme;
  const background = getComputedStyle(root).getPropertyValue("--bg").trim();
  if (background === "") return;
  for (const meta of doc.querySelectorAll('meta[name="theme-color"]')) {
    meta.setAttribute("content", background);
  }
}

/**
 * Calls `listener` with the page's theme each time it changes, for what paints with
 * concrete colors (the terminal) or picks defaults by it (the editor). Returns the stop.
 */
export function watchTheme(listener: (theme: Theme) => void, root = document.documentElement) {
  const observer = new MutationObserver(() => listener(pageTheme(root)));
  observer.observe(root, { attributes: true, attributeFilter: ["data-theme"] });
  return () => observer.disconnect();
}

export type ThemeState = {
  choice: Accessor<ThemeChoice>;
  setChoice(choice: ThemeChoice): void;
};

/** Keeps the page's theme current with the stored choice and the system setting. */
export function createThemeState(doc: Document, system: MediaQueryList | undefined): ThemeState {
  // A plain variable beside the signal: a signal's new value may only show after the
  // next flush, and the page must change at once.
  let current = loadThemeChoice();
  const [choice, setChoiceSignal] = createSignal<ThemeChoice>(current);
  const apply = (): void => applyTheme(doc, resolveTheme(current, system?.matches ?? false));
  system?.addEventListener("change", apply);
  apply();
  return {
    choice,
    setChoice(next) {
      current = next;
      saveThemeChoice(next);
      setChoiceSignal(next);
      apply();
    },
  };
}

let shared: ThemeState | undefined;

/** This page's one theme state, started on first use (index.tsx starts it). */
export function themeState(): ThemeState {
  shared ??= createThemeState(
    document,
    typeof matchMedia === "function" ? matchMedia(DARK_QUERY) : undefined,
  );
  return shared;
}
