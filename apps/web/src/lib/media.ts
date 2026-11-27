import { createSignal, onSettled, type Accessor } from "solid-js";

/** Tailwind's `lg` breakpoint (`--breakpoint-lg` in index.css): the sessions list fits beside a session. */
export const WIDE_QUERY = "(min-width: 64rem)";

/** Whether `query` matches, kept current. For layout that CSS alone cannot express. */
export function createMediaQuery(query: string): Accessor<boolean> {
  const list = typeof matchMedia === "function" ? matchMedia(query) : undefined;
  const [matches, setMatches] = createSignal(list?.matches ?? false);
  onSettled(() => {
    if (!list) return;
    const update = (): void => {
      setMatches(list.matches);
    };
    list.addEventListener("change", update);
    return () => list.removeEventListener("change", update);
  });
  return matches;
}

/**
 * Width of an element in pixels, kept current; 0 until it is laid out. For
 * panels whose layout depends on the room their pane gives them, not on the window.
 */
export function createWidth(element: () => HTMLElement | undefined): Accessor<number> {
  const [width, setWidth] = createSignal(0);
  onSettled(() => {
    const target = element();
    if (!target || typeof ResizeObserver !== "function") return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries.at(-1);
      if (entry) setWidth(entry.contentRect.width);
    });
    observer.observe(target);
    return () => observer.disconnect();
  });
  return width;
}
