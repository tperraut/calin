// Development only (see stress.html): a synthetic graph at the spike's scale,
// with nodes arriving over time, and the scenarios whose frame times are measured.
import { render } from "@solidjs/web";
import { createMemo, createSignal, For, Show } from "solid-js";
import "../../../index.css";
import "../graph.css";
import { GraphView, type GraphSelection } from "../GraphView";
import type { LayoutMode } from "../layout";
import type { Scene } from "../scene";
import { graphAt, synthesize } from "./synthetic";

type Result = {
  scenario: string;
  frames: number;
  median: number;
  p95: number;
  max: number;
  over33: number;
};

const query = new URLSearchParams(location.search);
const nodeCount = Number(query.get("n") ?? 200);
const edgeCount = Number(query.get("e") ?? 300);
const mode: LayoutMode = query.get("mode") === "columns" ? "columns" : "indented";
const steps = synthesize(nodeCount, edgeCount);

const sleep = (ms: number): Promise<void> => new Promise((done) => setTimeout(done, ms));

function summarize(scenario: string, deltas: number[]): Result {
  const sorted = deltas.toSorted((a, b) => a - b);
  const at = (p: number): number =>
    sorted.length === 0 ? 0 : sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))];
  const round = (value: number): number => Math.round(value * 10) / 10;
  return {
    scenario,
    frames: sorted.length,
    median: round(at(0.5)),
    p95: round(at(0.95)),
    max: round(sorted.at(-1) ?? 0),
    over33: round((100 * sorted.filter((delta) => delta > 33).length) / Math.max(1, sorted.length)),
  };
}

/** Frame-to-frame times over `ms`, while `each` runs once per frame. */
function record(ms: number, each: (elapsed: number) => void): Promise<number[]> {
  return new Promise((done) => {
    const deltas: number[] = [];
    let first: number | undefined;
    let last: number | undefined;
    const step = (time: number): void => {
      first ??= time;
      if (last !== undefined) deltas.push(time - last);
      last = time;
      if (time - first >= ms) {
        done(deltas);
        return;
      }
      each(time - first);
      requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  });
}

function StressPage() {
  const [count, setCount] = createSignal(steps.length);
  const [selection, setSelection] = createSignal<GraphSelection>();
  const [results, setResults] = createSignal<Result[]>([]);
  const [busy, setBusy] = createSignal<string>();
  const graph = createMemo(() => graphAt(steps, count()));
  let scene: Scene | undefined;

  /** A pan back and forth, as fast as 1500 px/s, driven from the frame loop. */
  async function pan(far: boolean): Promise<number[]> {
    if (!scene) return [];
    // Always from the same place: the whole graph, or its dense middle at full size.
    scene.fit();
    if (!far) scene.zoomBy(1 / scene.view().k);
    await sleep(200);
    let last = 0;
    const reach = 480;
    return record(3500, (elapsed) => {
      const wanted = reach * Math.sin((elapsed / 1000) * Math.PI);
      scene?.panBy((wanted - last) * 0.4, wanted - last);
      last = wanted;
    });
  }

  const scenarios: Record<string, () => Promise<number[]>> = {
    "pan-close": () => pan(false),
    "pan-far": () => pan(true),
    async pulses() {
      const messages = graph().edges.filter((edge) => edge.kind === "message");
      scene?.fit();
      let sent = 0;
      // About twenty pulses on their way at any moment.
      return record(3500, (elapsed) => {
        while (sent < elapsed / 45 && messages.length > 0) {
          scene?.pulse(messages[sent % messages.length].id);
          sent += 1;
        }
      });
    },
    async grow() {
      // The last ten nodes arrive at two a second: each is a layout and a tween.
      setCount(steps.length - 10);
      await sleep(600);
      scene?.fit();
      await sleep(300);
      let arrived = 0;
      const deltas = await record(5500, (elapsed) => {
        const due = Math.min(10, Math.floor(elapsed / 500) + 1);
        if (due > arrived) {
          arrived = due;
          setCount(steps.length - 10 + arrived);
        }
      });
      setCount(steps.length);
      return deltas;
    },
  };

  async function run(name: string): Promise<Result> {
    setBusy(name);
    const deltas = await scenarios[name]();
    const result = summarize(name, deltas);
    setResults((all) => [...all.filter((one) => one.scenario !== name), result]);
    setBusy(undefined);
    return result;
  }

  /** Nodes arriving from the first one, to watch the layout hold still. */
  async function replay(): Promise<void> {
    setBusy("replay");
    for (let shown = 1; shown <= steps.length; shown += 1) {
      setCount(shown);
      await sleep(100);
    }
    setBusy(undefined);
  }

  // What the browser check drives.
  Object.assign(window, {
    calinStress: {
      run,
      scenarios: Object.keys(scenarios),
      nodes: () => graph().nodes.length,
      edges: () => graph().edges.length,
      elements: () => document.querySelectorAll("svg.graph-canvas *").length,
    },
  });

  return (
    <div class="orch flex h-dvh flex-col bg-bg text-fg">
      <header class="flex flex-wrap items-center gap-2 border-b border-line bg-surface p-2 text-sm">
        <strong>Graph stress</strong>
        <span class="text-muted">
          {graph().nodes.length} nodes · {graph().edges.length} edges · {mode}
        </span>
        <For each={Object.keys(scenarios)}>
          {(name) => (
            <button
              type="button"
              class="btn px-2.5"
              disabled={busy() !== undefined}
              onClick={() => void run(name)}
            >
              {name}
            </button>
          )}
        </For>
        <button
          type="button"
          class="btn px-2.5"
          disabled={busy() !== undefined}
          onClick={() => void replay()}
        >
          replay arrivals
        </button>
      </header>
      <Show when={results().length > 0}>
        <pre class="border-b border-line p-2 font-mono text-xs" data-part="results">
          {results()
            .map(
              (one) =>
                `${one.scenario}: median ${one.median} ms, p95 ${one.p95} ms, max ${one.max} ms, ${one.over33}% over 33 ms (${one.frames} frames)`,
            )
            .join("\n")}
        </pre>
      </Show>
      <div class="flex min-h-0 flex-1">
        <GraphView
          graph={graph()}
          openSessionId="s0"
          mode={mode}
          selection={selection()}
          onSelect={setSelection}
          onScene={(made) => (scene = made)}
        />
      </div>
    </div>
  );
}

const root = document.getElementById("root");
if (!root) throw new Error("stress.html has no #root element");
render(() => <StressPage />, root);
