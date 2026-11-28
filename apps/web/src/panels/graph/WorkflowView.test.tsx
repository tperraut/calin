import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vite-plus/test";
import { startRun } from "../../lib/workflow";
import { describeCanvasMoves, press } from "../../test/canvas";
import { find, mount, unmountAll } from "../../test/render";
import { specGraph } from "../../test/workflows";
import { WorkflowView } from "./WorkflowView";

const run = startRun({
  type: "workflow_started",
  workflow_id: "ship",
  name: "Ship it",
  graph: specGraph(),
  budget_usd: null,
});

afterEach(unmountAll);

function setup() {
  const [selection, setSelection] = createSignal<string>();
  const host = mount(() => (
    <WorkflowView
      run={run}
      orientation="down"
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
