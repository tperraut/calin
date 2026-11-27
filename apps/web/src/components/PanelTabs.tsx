import { For, Show } from "solid-js";

export type Tab<T extends string> = { id: T; label: string; badge?: string };

export const tabId = (base: string, id: string): string => `${base}-tab-${id}`;
export const tabPanelId = (base: string, id: string): string => `${base}-panel-${id}`;

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
    if (at === -1) return;
    const [back, forward] = vertical() ? ["ArrowUp", "ArrowDown"] : ["ArrowLeft", "ArrowRight"];
    let next: number;
    if (event.key === forward) next = (at + 1) % tabs.length;
    else if (event.key === back) next = (at - 1 + tabs.length) % tabs.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = tabs.length - 1;
    else return;
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
          <button
            type="button"
            role="tab"
            id={tabId(props.idBase, tab().id)}
            aria-selected={props.active === tab().id ? "true" : "false"}
            aria-controls={
              props.active === tab().id ? tabPanelId(props.idBase, tab().id) : undefined
            }
            tabindex={stop() === tab().id ? 0 : -1}
            class={[
              // Five tabs share a 360px phone: the padding is what gives.
              "flex min-h-11 min-w-11 items-center justify-center gap-1 px-1.5 text-sm font-medium -outline-offset-2 sm:gap-1.5 sm:px-3",
              // Sized by their labels, then sharing what is left: equal shares would cut the longest one.
              vertical() ? "border-l-2" : "flex-auto shrink-0 border-b-2 lg:flex-none",
              props.active === tab().id
                ? "border-accent text-fg"
                : "border-transparent text-muted hover:text-fg",
            ]}
            onClick={() => props.onSelect(tab().id)}
          >
            {tab().label}
            <Show when={tab().badge}>
              {(badge) => (
                <span class="rounded-full bg-sunken px-1.5 text-xs font-normal">{badge()}</span>
              )}
            </Show>
          </button>
        )}
      </For>
    </div>
  );
}
