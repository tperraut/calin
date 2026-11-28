import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { WorkflowRun } from "../../gen/WorkflowRun";
import { foldNode, startRun } from "../../lib/workflow";
import { canvasView, describeCanvasMoves, drag, nextFrame, press } from "../../test/canvas";
import { find, mount, unmountAll } from "../../test/render";
import { specGraph } from "../../test/workflows";
import {
  type Orientation,
  WorkflowHeader,
  WorkflowNodeDetails,
  WorkflowText,
  WorkflowView,
} from "./WorkflowView";
import { runOverlay } from "./workflowModel";

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

describe("WorkflowView: the drawing", () => {
  const finished = (on: WorkflowRun, id: string, port: string): WorkflowRun =>
    foldNode(
      foldNode(on, {
        type: "workflow_node",
        node_id: id,
        phase: "started",
        port: null,
        outcome: null,
      }),
      {
        type: "workflow_node",
        node_id: id,
        phase: "finished",
        port,
        outcome: null,
      },
    );

  function draw(drawn: WorkflowRun) {
    return mount(() => (
      <WorkflowView run={drawn} orientation="down" selection={undefined} onSelect={() => {}} />
    ));
  }

  it("rings a node's card for the focus and puts its corner word at the right edge", () => {
    const host = draw(run);
    const card = find(host, 'g.wnode[data-node="start"]');
    const width = Number(find(card, "rect.wshape").getAttribute("width"));
    const height = Number(find(card, "rect.wshape").getAttribute("height"));
    expect(width).toBeGreaterThan(0);
    expect(find(card, "rect.wfocus").getAttribute("width")).toBe(String(width + 10));
    expect(find(card, "rect.wfocus").getAttribute("height")).toBe(String(height + 10));
    expect(find(card, "text.wcorner").getAttribute("x")).toBe(String(width - 10));
  });

  it("draws taken edges bold with their count past one, and the others faint", () => {
    const twice = finished(finished(run, "start", "next"), "start", "next");
    const host = draw(twice);
    const markers = [...host.querySelectorAll("marker")].map((one) => [
      one.id.split("-").at(-1),
      one.getAttribute("markerWidth"),
      one.getAttribute("markerHeight"),
      one.querySelector("path")?.getAttribute("style"),
    ]);
    expect(markers).toEqual([
      ["taken", "9", "9", "fill: var(--fg);"],
      ["idle", "7", "7", "fill: var(--muted);"],
    ]);
    const taken = find(host, 'g.wedge[data-edge="0"]');
    expect(taken.hasAttribute("data-taken")).toBe(true);
    expect(find(taken, "path.wline").getAttribute("marker-end")).toMatch(/-taken\)$/);
    expect(find(taken, "g.wcount").textContent).toBe("×2");
    const idle = find(host, 'g.wedge[data-edge="1"]');
    expect(idle.hasAttribute("data-taken")).toBe(false);
    expect(find(idle, "path.wline").getAttribute("marker-end")).toMatch(/-idle\)$/);
    expect(idle.querySelector("g.wcount")).toBeNull();
  });

  it("shows the count of an edge only past one", () => {
    const host = draw(finished(run, "start", "next"));
    const once = find(host, 'g.wedge[data-edge="0"]');
    expect(once.querySelector("g.wcount")).toBeNull();
    expect(find(once, "path.wline").getAttribute("marker-end")).toMatch(/-taken\)$/);
  });

  it("draws each state of the legend the way the drawing does", () => {
    const host = draw(run);
    [...host.querySelectorAll("button")].find((one) => one.textContent === "Legend")?.click();
    flush();
    const shapes = [...host.querySelectorAll('ul[aria-label="Legend"] li g')].map((one) => [
      one.getAttribute("class"),
      one.getAttribute("data-state"),
      one.hasAttribute("data-failed"),
      one.hasAttribute("data-taken"),
      one.hasAttribute("data-failure"),
    ]);
    expect(shapes).toEqual([
      ["wnode", "active", false, false, false],
      ["wnode", "finished", false, false, false],
      ["wnode", "finished", true, false, false],
      ["wnode", "idle", false, false, false],
      ["wedge", null, false, true, false],
      ["wedge", null, false, false, false],
      ["wedge", null, false, false, true],
    ]);
  });
});

describe("WorkflowHeader", () => {
  const header = (cost: number, budget: number | null) => {
    const host = mount(() => (
      <WorkflowHeader run={{ ...run, cost_usd: cost, budget_usd: budget }} />
    ));
    return { text: host.textContent, bar: host.querySelector<HTMLElement>("span.block.h-full") };
  };

  it("says what was spent, and draws it against the budget, full and red at the budget", () => {
    expect(header(1.5, null)).toEqual({ text: expect.stringContaining("spent $1.50"), bar: null });
    const half = header(1, 2);
    expect(half.text).toContain("spent $1.00 of $2.00");
    expect(half.bar?.style.width).toBe("50%");
    expect(half.bar?.classList.contains("bg-accent")).toBe(true);
    const full = header(2, 2);
    expect(full.bar?.style.width).toBe("100%");
    expect(full.bar?.classList.contains("bg-tone-danger")).toBe(true);
    const over = header(3, 2);
    expect(over.bar?.style.width).toBe("100%");
    expect(over.bar?.classList.contains("bg-tone-danger")).toBe(true);
  });
});

describe("WorkflowText", () => {
  it("says the path taken, or that there is none yet", () => {
    expect(mount(() => <WorkflowText run={run} />).textContent).toContain(
      "Path taken: nothing yet.",
    );
    const begun = foldNode(run, {
      type: "workflow_node",
      node_id: "start",
      phase: "started",
      port: null,
      outcome: null,
    });
    expect(mount(() => <WorkflowText run={begun} />).textContent).toContain("Path taken: start.");
  });
});

describe("WorkflowNodeDetails", () => {
  const details = (drawn: WorkflowRun, id: string) => {
    const node = runOverlay(drawn).byId.get(id);
    if (!node) throw new Error(`no node ${id}`);
    return mount(() => (
      <WorkflowNodeDetails
        run={drawn}
        node={node}
        wide={true}
        onSelect={() => {}}
        onShowInTranscript={undefined}
      />
    ));
  };

  it("sets the outcome in mono, and lists no ports for a node that has none", () => {
    const tested = foldNode(run, {
      type: "workflow_node",
      node_id: "tests",
      phase: "finished",
      port: "pass",
      outcome: "cargo test: ok",
    });
    const host = details(tested, "tests");
    const outcome = [...host.querySelectorAll("dd")].find(
      (one) => one.textContent === "cargo test: ok",
    );
    expect(outcome?.classList.contains("font-mono")).toBe(true);
    expect(host.querySelector('ul[aria-label="Ports"]')).not.toBeNull();
    const label = [...host.querySelectorAll("dd")].find((one) => one.textContent === "repo checks");
    expect(label?.classList.contains("font-mono")).toBe(false);
    unmountAll();
    expect(details(run, "ok").querySelector('ul[aria-label="Ports"]')).toBeNull();
  });
});
