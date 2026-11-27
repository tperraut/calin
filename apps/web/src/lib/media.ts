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
