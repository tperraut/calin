import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { startRun } from "../../lib/workflow";
import { canvasView, describeCanvasMoves, drag, nextFrame, press } from "../../test/canvas";
import { find, mount, unmountAll } from "../../test/render";
import { specGraph } from "../../test/workflows";
import { type Orientation, WorkflowView } from "./WorkflowView";

const run = startRun({
  type: "workflow_started",
  workflow_id: "ship",
  name: "Ship it",
  graph: specGraph(),
  budget_usd: null,
});

afterEach(unmountAll);

function setup(initial: Orientation = "down") {
  const [selection, setSelection] = createSignal<string>();
  const [orientation, setOrientation] = createSignal(initial);
  const host = mount(() => (
    <WorkflowView
      run={run}
      orientation={orientation()}
      selection={selection()}
      onSelect={(id) => setSelection(id)}
    />
  ));
  return {
    host,
    canvas: find<SVGSVGElement>(host, 'svg[aria-label="Workflow graph"]'),
    node: (id: string) => find<SVGGElement>(host, `g.wnode[data-node="${id}"]`),
    selected: selection,
    sample: "tests",
    step: 1.3,
    setOrientation,
  };
}

describeCanvasMoves("WorkflowView", () => Promise.resolve(setup()));

describe("WorkflowView: the keyboard", () => {
  it("moves focus through the nodes in order with the arrows, Home and End", () => {
    const { canvas, node } = setup();
    const stop = () => canvas.querySelector('g.wnode[tabindex="0"]')?.getAttribute("data-node");
    expect(stop()).toBe("start");
    // Outside a node the arrows do nothing, and the browser keeps them.
    for (const key of ["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End"]) {
      expect(press(canvas, key)).toBe(false);
    }
    expect(press(node("start"), "ArrowDown")).toBe(true);
    expect(stop()).toBe("implement");
    press(node("implement"), "ArrowRight");
    expect(stop()).toBe("tests");
    press(node("tests"), "ArrowUp");
    expect(stop()).toBe("implement");
    press(node("implement"), "ArrowLeft");
    expect(stop()).toBe("start");
    press(node("start"), "ArrowLeft");
    expect(stop()).toBe("start");
    press(node("start"), "End");
    const last = stop() ?? "";
    expect(last).not.toBe("start");
    press(node(last), "ArrowRight");
    expect(stop()).toBe(last);
    press(node(last), "Home");
    expect(stop()).toBe("start");
  });

  it("opens and closes the legend", () => {
    const { host } = setup();
    const toggle = [...host.querySelectorAll("button")].find((one) => one.textContent === "Legend");
    expect(toggle?.getAttribute("aria-expanded")).toBe("false");
    expect(host.querySelector('ul[aria-label="Legend"]')).toBeNull();
    toggle?.click();
    flush();
    expect(toggle?.getAttribute("aria-expanded")).toBe("true");
    expect(host.querySelectorAll('ul[aria-label="Legend"] li')).toHaveLength(7);
    toggle?.click();
    flush();
    expect(host.querySelector('ul[aria-label="Legend"]')).toBeNull();
  });
});

describe("WorkflowView: the orientation", () => {
  it("shows the whole run again, as a fresh view would, when it turns", async () => {
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue(
      DOMRect.fromRect({ x: 0, y: 0, width: 400, height: 300 }),
    );
    const across = setup("across");
    await nextFrame();
    const fresh = canvasView(across.canvas);
    unmountAll();

    const { canvas, setOrientation } = setup("down");
    await nextFrame();
    drag(canvas, 1, [10, 10], [90, 70]);
    setOrientation("across");
    flush();
    await nextFrame();
    expect(canvas.getAttribute("data-orientation")).toBe("across");
    expect(canvasView(canvas)).toEqual(fresh);
  });
});
