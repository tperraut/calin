/* @refresh reload */
import { render } from "@solidjs/web";
import { onSettled, Show } from "solid-js";
import { createCanvasRenderer } from "./canvas.ts";
import { parseOpts, Scene } from "./scene.ts";
import { SvgView } from "./SvgView.tsx";

const opts = parseOpts(new URLSearchParams(location.search));
const scene = new Scene(opts);
// scripts/measure.mjs drives the page through this.
(window as unknown as { __scene: Scene }).__scene = scene;

function App() {
  let host!: HTMLDivElement;
  let replay = 0;
  onSettled(() => {
    scene.attachInput(host);
    if (opts.renderer === "canvas") {
      scene.renderer = createCanvasRenderer(host, scene);
      scene.dirty.view = true;
    }
    void scene.boot();
  });
  const button = "background:#2a2c28;color:inherit;border:1px solid #4b4f47;border-radius:6px;padding:4px 8px;font:inherit";
  return (
    <div ref={(el) => (host = el)} style="position:fixed;inset:0;touch-action:none;overflow:hidden">
      <Show when={opts.renderer === "svg"}>
        <SvgView scene={scene} />
      </Show>
      <div style="position:absolute;left:8px;right:8px;bottom:8px;display:flex;gap:6px;align-items:center;flex-wrap:wrap;pointer-events:none">
        <span style="background:#000a;padding:3px 6px;border-radius:6px">
          {opts.renderer} · {opts.cfg} · {scene.hud()}
        </span>
        <span style="display:flex;gap:6px;pointer-events:auto">
          <button style={button} onClick={() => void scene.setCount(scene.shown + 1, true)}>+1</button>
          <button
            style={button}
            onClick={() => {
              clearInterval(replay);
              void scene.setCount(8, false);
              replay = window.setInterval(() => {
                if (scene.shown >= scene.steps.length) clearInterval(replay);
                else void scene.setCount(scene.shown + 1, true);
              }, 500);
            }}
          >
            replay
          </button>
          <button style={button} onClick={() => scene.setPulses(scene.pulses.length === 0)}>pulses</button>
          <button style={button} onClick={() => scene.fit()}>fit</button>
        </span>
      </div>
    </div>
  );
}

render(() => <App />, document.getElementById("root")!);
