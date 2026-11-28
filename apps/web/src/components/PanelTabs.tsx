import { For, Show } from "solid-js";

export type Tab<T extends string> = { id: T; label: string; badge?: string };

export const tabId = (base: string, id: string): string => `${base}-tab-${id}`;
export const tabPanelId = (base: string, id: string): string => `${base}-panel-${id}`;

/** The tab a key moves the focus to, from the one at `at` of `count`; undefined leaves the key alone. */
function rovingIndex(
  key: string,
  at: number,
  count: number,
  vertical: boolean,
): number | undefined {
  const [back, forward] = vertical ? ["ArrowUp", "ArrowDown"] : ["ArrowLeft", "ArrowRight"];
  return new Map([
    [forward, (at + 1) % count],
    [back, (at - 1 + count) % count],
    ["Home", 0],
    ["End", count - 1],
  ]).get(key);
}

/**
 * A tablist with roving focus: one tab stop, arrows move between tabs, Enter or
 * Space selects the focused one. Selecting is left to the reader because a tab
 * here changes the address and may fetch a panel.
 */
export function PanelTabs<T extends string>(props: {
  label: string;
  /** Shared with the panels, which are named `tabPanelId(idBase, id)`. */
  idBase: string;
  tabs: readonly Tab<T>[];
  /** `undefined` when no panel is showing (the side pane is collapsed). */
  active: T | undefined;
  orientation?: "horizontal" | "vertical";
  onSelect: (id: T) => void;
}) {
  const vertical = (): boolean => props.orientation === "vertical";
  // The one tab stop: the selected tab, or the first when none is.
  const stop = (): T | undefined => props.active ?? props.tabs.at(0)?.id;

  function onKeyDown(event: KeyboardEvent & { currentTarget: HTMLElement }): void {
    const tabs = [...event.currentTarget.querySelectorAll<HTMLElement>('[role="tab"]')];
    const at = tabs.findIndex((tab) => tab === document.activeElement);
    const next = at === -1 ? undefined : rovingIndex(event.key, at, tabs.length, vertical());
    if (next === undefined) return;
    event.preventDefault();
    tabs[next].focus();
  }

  return (
    <div
      role="tablist"
      aria-label={props.label}
      aria-orientation={vertical() ? "vertical" : "horizontal"}
      class={["flex min-w-0", vertical() ? "flex-col" : "flex-1 overflow-x-auto"]}
      onKeyDown={onKeyDown}
    >
      <For each={props.tabs} keyed={(tab) => tab.id}>
        {(tab) => (
          <TabButton
            idBase={props.idBase}
            tab={tab()}
            active={props.active === tab().id}
            stop={stop() === tab().id}
            vertical={vertical()}
            onSelect={() => props.onSelect(tab().id)}
          />
        )}
      </For>
    </div>
  );
}

function TabButton<T extends string>(props: {
  idBase: string;
  tab: Tab<T>;
  active: boolean;
  /** The tablist's one tab stop. */
  stop: boolean;
  vertical: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      role="tab"
      id={tabId(props.idBase, props.tab.id)}
      aria-selected={props.active ? "true" : "false"}
      aria-controls={props.active ? tabPanelId(props.idBase, props.tab.id) : undefined}
      tabindex={props.stop ? 0 : -1}
      class={[
        // Five tabs share a 360px phone: the padding is what gives.
        "flex min-h-11 min-w-11 items-center justify-center gap-1 px-1.5 text-sm font-medium -outline-offset-2 sm:gap-1.5 sm:px-3",
        // Sized by their labels, then sharing what is left: equal shares would cut the longest one.
        props.vertical ? "border-l-2" : "flex-auto shrink-0 border-b-2 lg:flex-none",
        props.active ? "border-accent text-fg" : "border-transparent text-muted hover:text-fg",
      ]}
      onClick={() => props.onSelect()}
    >
      {props.tab.label}
      <Show when={props.tab.badge}>
        {(badge) => (
          <span class="rounded-full bg-sunken px-1.5 text-xs font-normal">{badge()}</span>
        )}
      </Show>
    </button>
  );
}
