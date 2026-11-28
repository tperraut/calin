import { createSignal } from "solid-js";
import { afterEach } from "vite-plus/test";
import { describeCanvasMoves } from "../../test/canvas";
import { makeGraph } from "../../test/orchestration";
import { find, mount, unmountAll } from "../../test/render";
import { GraphView, type GraphSelection } from "./GraphView";

afterEach(unmountAll);

describeCanvasMoves("GraphView", () => {
  const [selection, setSelection] = createSignal<GraphSelection>();
  const host = mount(() => (
    <GraphView
      graph={makeGraph()}
      openSessionId="s1"
      mode="columns"
      selection={selection()}
      onSelect={setSelection}
    />
  ));
  return Promise.resolve({
    host,
    canvas: find<SVGSVGElement>(host, 'svg[aria-label="Session graph"]'),
    node: (id: string) => find<SVGGElement>(host, `g.gnode[data-node="${id}"]`),
    selected: () => selection()?.id,
    sample: "agent:a1",
    step: 1.3,
  });
});
