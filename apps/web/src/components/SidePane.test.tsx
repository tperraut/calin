import { createSignal, flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { loadSidePaneWidth, saveSidePaneWidth } from "../lib/prefs";
import type { SidePanelId } from "../panels/registry";
import { pointer, press } from "../test/canvas";
import { click, find, findByText, mount, unmountAll } from "../test/render";
import { SidePane } from "./SidePane";

afterEach(unmountAll);

/** The window the pane sits in: 1200 px wide, so the pane may grow to 1200 - 320 px. */
const WINDOW = 1200;

beforeEach(() => {
  // happy-dom lays nothing out: the sizes the pane measures are set here.
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(WINDOW);
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (
    this: HTMLElement,
  ) {
    return Number.parseFloat(this.style.width) || 0;
  });
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue(
    new DOMRect(0, 0, WINDOW, 800),
  );
});

const tabs = [
  { id: "changes", label: "Changes" },
  { id: "files", label: "Files" },
] as const;

function setup(initial: SidePanelId | undefined) {
  const [panel, setPanel] = createSignal<SidePanelId | undefined>(initial);
  const onSelect = vi.fn((id: SidePanelId) => setPanel(id));
  const onCollapse = vi.fn(() => setPanel(undefined));
  const host = mount(() => (
    <SidePane idBase="ws" tabs={tabs} panel={panel()} onSelect={onSelect} onCollapse={onCollapse}>
      <p>panel body</p>
    </SidePane>
  ));
  const pane = find(host, 'aside[aria-label="Workspace panels"]');
  const edge = () => find(host, '[role="separator"]');
  const now = () => Number(edge().getAttribute("aria-valuenow"));
  return { host, pane, edge, now, onSelect, onCollapse };
}

describe("SidePane", () => {
  it("is only a rail of vertical tabs while collapsed, with its panels kept but hidden", () => {
    const { host, pane } = setup(undefined);
    expect(host.querySelector('[role="separator"]')).toBeNull();
    expect(pane.classList.contains("border-l")).toBe(true);
    expect(pane.getAttribute("style")).toBeNull();
    expect(find(pane, '[role="tablist"]').getAttribute("aria-orientation")).toBe("vertical");
    expect(pane.querySelector('button[aria-label="Collapse the panel"]')).toBeNull();
    const body = find(pane, "#ws-panel-none");
    expect(body.hidden).toBe(true);
    expect(body.getAttribute("role")).toBeNull();
    expect(body.getAttribute("aria-labelledby")).toBeNull();
    expect(body.textContent).toBe("panel body");
  });

  it("opens on a tab, at the default width, with an edge that says how wide it is", async () => {
    const { pane, edge, onSelect } = setup(undefined);
    await click(findByText(pane, "button", "Files"));
    expect(onSelect).toHaveBeenCalledWith("files");

    expect(pane.classList.contains("border-l")).toBe(false);
    expect(pane.style.width).toBe("440px");
    expect(pane.style.maxWidth).toBe("calc(100% - 320px)");
    expect(find(pane, '[role="tablist"]').getAttribute("aria-orientation")).toBe("horizontal");
    const body = find(pane, '[role="tabpanel"]');
    expect(body.id).toBe("ws-panel-files");
    expect(body.getAttribute("aria-labelledby")).toBe("ws-tab-files");
    expect(body.hidden).toBe(false);

    expect(edge().getAttribute("aria-label")).toBe("Resize the panel");
    expect(edge().getAttribute("aria-orientation")).toBe("vertical");
    expect(edge().getAttribute("aria-valuemin")).toBe("280");
    expect(edge().getAttribute("aria-valuemax")).toBe(String(WINDOW - 320));
    expect(edge().getAttribute("aria-valuenow")).toBe("440");
    expect(edge().getAttribute("aria-valuetext")).toBe("440 pixels wide");
  });

  it("collapses from its button", async () => {
    const { host, pane, onCollapse } = setup("changes");
    await click(find(pane, 'button[aria-label="Collapse the panel"]'));
    expect(onCollapse).toHaveBeenCalledTimes(1);
    expect(host.querySelector('[role="separator"]')).toBeNull();
    expect(pane.querySelector('[role="tabpanel"]')).toBeNull();
  });

  it("opens at the width the reader left it at", () => {
    saveSidePaneWidth(600);
    const { pane, now } = setup("changes");
    expect(now()).toBe(600);
    expect(pane.style.width).toBe("600px");
  });

  it("resizes from the keyboard, within its limits, and remembers each step", () => {
    const { pane, edge, now } = setup("changes");
    // The edge is on the pane's left: Left widens the pane.
    expect(press(edge(), "ArrowLeft")).toBe(true);
    expect(now()).toBe(464);
    expect(pane.style.width).toBe("464px");
    expect(loadSidePaneWidth()).toBe(464);
    expect(press(edge(), "ArrowRight")).toBe(true);
    expect(now()).toBe(440);
    expect(press(edge(), "End")).toBe(true);
    expect(now()).toBe(WINDOW - 320);
    expect(press(edge(), "ArrowLeft")).toBe(true);
    expect(now()).toBe(WINDOW - 320);
    expect(press(edge(), "Home")).toBe(true);
    expect(now()).toBe(280);
    expect(press(edge(), "ArrowRight")).toBe(true);
    expect(now()).toBe(280);
    expect(loadSidePaneWidth()).toBe(280);

    expect(press(edge(), "a")).toBe(false);
    expect(press(edge(), "ArrowUp")).toBe(false);
    expect(now()).toBe(280);
  });

  it("follows a drag of its edge, and remembers the width once the pointer lets go", () => {
    const { pane, edge, now } = setup("changes");
    // Moving without pressing first does nothing.
    pointer(edge(), "pointermove", 1, [700, 10]);
    flush();
    expect(now()).toBe(440);

    pointer(edge(), "pointerdown", 1, [760, 10]);
    pointer(edge(), "pointermove", 1, [700, 10]);
    flush();
    expect(now()).toBe(WINDOW - 700);
    expect(pane.style.width).toBe(`${WINDOW - 700}px`);
    expect(loadSidePaneWidth()).toBeUndefined();
    pointer(edge(), "pointermove", 1, [100, 10]);
    flush();
    expect(now()).toBe(WINDOW - 320);
    pointer(edge(), "pointermove", 1, [1100, 10]);
    flush();
    expect(now()).toBe(280);
    pointer(edge(), "pointerup", 1, [1100, 10]);
    expect(loadSidePaneWidth()).toBe(280);

    // Let go already: further moves and a second release change nothing.
    pointer(edge(), "pointermove", 1, [700, 10]);
    pointer(edge(), "pointerup", 1, [700, 10]);
    flush();
    expect(now()).toBe(280);
  });

  it("keeps the width of a cancelled drag on screen without remembering it", () => {
    const { edge, now } = setup("changes");
    pointer(edge(), "pointerdown", 1, [760, 10]);
    pointer(edge(), "pointermove", 1, [600, 10]);
    pointer(edge(), "pointercancel", 1, [600, 10]);
    pointer(edge(), "pointermove", 1, [900, 10]);
    pointer(edge(), "pointerup", 1, [900, 10]);
    flush();
    expect(now()).toBe(WINDOW - 600);
    expect(loadSidePaneWidth()).toBeUndefined();
  });
});
