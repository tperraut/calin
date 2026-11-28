import { createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { press } from "../test/canvas";
import { click, find, findByText, mount, unmountAll } from "../test/render";
import { PanelTabs, type Tab } from "./PanelTabs";

afterEach(unmountAll);

type Id = "one" | "two" | "three";
const tabs: Tab<Id>[] = [
  { id: "one", label: "One" },
  { id: "two", label: "Two", badge: "4" },
  { id: "three", label: "Three" },
];

function setup(orientation: "horizontal" | "vertical" | undefined, active?: Id) {
  const [shown, setShown] = createSignal<Id | undefined>(active);
  const onSelect = vi.fn((id: Id) => setShown(id));
  const host = mount(() => (
    <PanelTabs
      label="Panels"
      idBase="p"
      tabs={tabs}
      active={shown()}
      orientation={orientation}
      onSelect={onSelect}
    />
  ));
  const list = find(host, '[role="tablist"]');
  const tab = (label: string) => findByText<HTMLButtonElement>(list, '[role="tab"]', label);
  return { list, tab, onSelect };
}

describe("PanelTabs", () => {
  it("names its tabs and panels, with one tab stop on the first while none is shown", () => {
    const { list, tab } = setup(undefined);
    expect(list.getAttribute("aria-label")).toBe("Panels");
    expect(list.getAttribute("aria-orientation")).toBe("horizontal");
    const all = [...list.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
    expect(all.map((one) => one.id)).toEqual(["p-tab-one", "p-tab-two", "p-tab-three"]);
    expect(all.map((one) => one.tabIndex)).toEqual([0, -1, -1]);
    expect(all.map((one) => one.getAttribute("aria-selected"))).toEqual([
      "false",
      "false",
      "false",
    ]);
    expect(all.map((one) => one.getAttribute("aria-controls"))).toEqual([null, null, null]);
    expect(tab("Two").textContent).toBe("Two4");
    expect(find(tab("Two"), "span").textContent).toBe("4");
    expect(tab("One").querySelector("span")).toBeNull();
  });

  it("selects a tab on a click, which then holds the tab stop and controls its panel", async () => {
    const { tab, onSelect } = setup(undefined);
    await click(tab("Three"));
    expect(onSelect).toHaveBeenCalledWith("three");
    expect(tab("Three").getAttribute("aria-selected")).toBe("true");
    expect(tab("Three").getAttribute("aria-controls")).toBe("p-panel-three");
    expect(tab("Three").tabIndex).toBe(0);
    expect(tab("Three").classList.contains("border-accent")).toBe(true);
    expect(tab("One").tabIndex).toBe(-1);
    expect(tab("One").classList.contains("text-muted")).toBe(true);
  });

  it("moves focus with Up and Down when vertical, wrapping round, and leaves Left and Right alone", () => {
    const { list, tab, onSelect } = setup("vertical", "two");
    expect(list.getAttribute("aria-orientation")).toBe("vertical");
    expect(list.classList.contains("flex-col")).toBe(true);
    expect(tab("One").classList.contains("border-l-2")).toBe(true);
    tab("Two").focus();
    expect(press(tab("Two"), "ArrowDown")).toBe(true);
    expect(document.activeElement).toBe(tab("Three"));
    expect(press(tab("Three"), "ArrowDown")).toBe(true);
    expect(document.activeElement).toBe(tab("One"));
    expect(press(tab("One"), "ArrowUp")).toBe(true);
    expect(document.activeElement).toBe(tab("Three"));
    expect(press(tab("Three"), "ArrowRight")).toBe(false);
    expect(press(tab("Three"), "ArrowLeft")).toBe(false);
    expect(document.activeElement).toBe(tab("Three"));
    // Focus moves; the selection does not.
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("moves focus with Left, Right, Home and End when horizontal, and leaves other keys alone", () => {
    const { list, tab } = setup("horizontal", "one");
    expect(list.classList.contains("overflow-x-auto")).toBe(true);
    tab("One").focus();
    expect(press(tab("One"), "ArrowLeft")).toBe(true);
    expect(document.activeElement).toBe(tab("Three"));
    expect(press(tab("Three"), "ArrowRight")).toBe(true);
    expect(document.activeElement).toBe(tab("One"));
    expect(press(tab("One"), "End")).toBe(true);
    expect(document.activeElement).toBe(tab("Three"));
    expect(press(tab("Three"), "Home")).toBe(true);
    expect(document.activeElement).toBe(tab("One"));
    expect(press(tab("One"), "ArrowDown")).toBe(false);
    expect(press(tab("One"), "Enter")).toBe(false);
  });

  it("ignores keys while no tab has the focus", () => {
    const { list } = setup("horizontal", "one");
    expect(press(list, "ArrowRight")).toBe(false);
    expect(document.activeElement).toBe(document.body);
  });
});
