// The SVG renderer the design proposes: Solid owns the structure (one element group per node,
// one path per edge). What changes every frame goes either through signals (`upd=solid`) or
// straight to the DOM (`upd=dom`); the query flags switch the optimisations on one at a time.

import { For, onSettled, Show } from "solid-js";
import { BG, CARD, drawPulses, EDGE_COLOR, NODE_STROKE, STATUS } from "./canvas.ts";
import { pathD } from "./geom.ts";
import { LOD_BELOW, PULSE_MS, tr, type Dirty, type Scene } from "./scene.ts";

const CSS = `
.e { fill: none; stroke-width: 1.5; }
.e.spawned { stroke: ${EDGE_COLOR.spawned}; marker-end: url(#arrow-spawned); }
.e.message { stroke: ${EDGE_COLOR.message}; marker-end: url(#arrow-message); }
.e.changed { stroke: ${EDGE_COLOR.changed}; marker-end: url(#arrow-changed); }
.n rect { fill: ${CARD}; stroke-width: 1; }
.n.user rect { stroke: ${NODE_STROKE.user}; } .n.agent rect { stroke: ${NODE_STROKE.agent}; }
.n.sub rect { stroke: ${NODE_STROKE.sub}; } .n.repo rect { stroke: ${NODE_STROKE.repo}; }
.n text { fill: #e8e8e3; font: 12px system-ui, sans-serif; }
.lod .n text, .lod .n circle { display: none; }
.lod .e { marker-end: none; }
.pulse { fill: #fff; }
.pulse.css { offset-rotate: 0deg; animation: pulse ${PULSE_MS}ms linear infinite; }
@keyframes pulse { from { offset-distance: 0%; } to { offset-distance: 100%; } }
`;

export function SvgView(props: { scene: Scene }) {
  const scene = props.scene;
  const o = scene.opts;
  const solid = o.upd === "solid";
  let view!: SVGGElement;
  let overlay: HTMLCanvasElement | undefined;
  let octx: CanvasRenderingContext2D | null = null;
  const dpr = window.devicePixelRatio || 1;
  let lod = false;
  let cull = { x0: 0, y0: 0, x1: 0, y1: 0, k: 0 };

  const applyView = () => {
    const { x, y, k } = scene.view;
    if (o.pan === "css") view.style.transform = `translate(${x}px,${y}px) scale(${k})`;
    else if (solid) scene.setViewT(`translate(${x},${y}) scale(${k})`);
    else view.setAttribute("transform", `translate(${x},${y}) scale(${k})`);
    const wantLod = o.lod && k < LOD_BELOW;
    if (wantLod !== lod) view.classList.toggle("lod", (lod = wantLod));
  };

  // Culling hides elements (display:none) instead of removing them: no DOM churn while panning.
  // The kept rectangle is the viewport grown by half its size, recomputed when the view leaves it.
  const updateCull = (force: boolean) => {
    const { x, y, k } = scene.view;
    const vx0 = -x / k, vy0 = -y / k, vx1 = (scene.vw - x) / k, vy1 = (scene.vh - y) / k;
    if (!force && k === cull.k && vx0 >= cull.x0 && vy0 >= cull.y0 && vx1 <= cull.x1 && vy1 <= cull.y1) return;
    const mx = (vx1 - vx0) / 2, my = (vy1 - vy0) / 2;
    cull = { x0: vx0 - mx, y0: vy0 - my, x1: vx1 + mx, y1: vy1 + my, k };
    for (const n of scene.nodes) {
      const vis = n.tx < cull.x1 && n.tx + n.g.w > cull.x0 && n.ty < cull.y1 && n.ty + n.g.h > cull.y0;
      if (vis !== n.vis || force) {
        n.vis = vis;
        if (n.el) n.el.style.display = vis ? "" : "none";
      }
    }
    for (const e of scene.edges) {
      const p = e.to;
      let ax = Infinity, ay = Infinity, bx = -Infinity, by = -Infinity;
      for (let i = 0; i < p.length; i += 2) {
        if (p[i] < ax) ax = p[i];
        if (p[i] > bx) bx = p[i];
        if (p[i + 1] < ay) ay = p[i + 1];
        if (p[i + 1] > by) by = p[i + 1];
      }
      const vis = ax < cull.x1 && bx > cull.x0 && ay < cull.y1 && by > cull.y0;
      if (vis !== e.vis || force) {
        e.vis = vis;
        if (e.el) e.el.style.display = vis ? "" : "none";
      }
    }
  };

  const frame = (d: Dirty) => {
    if (d.view || d.structure) {
      applyView();
      if (o.cull) updateCull(d.structure);
    }
    if (d.geom) {
      for (const n of scene.nodes) {
        if (!n.moving) continue;
        if (solid) n.setPos(tr(n.x, n.y));
        else n.el?.setAttribute("transform", tr(n.x, n.y));
      }
      for (const e of scene.edges) {
        if (!e.moving) continue;
        if (solid) e.setD(pathD(e.pts));
        else e.el?.setAttribute("d", pathD(e.pts));
      }
    }
    if (o.pulse === "svg" && d.pulses) {
      for (const p of scene.pulses) {
        if (solid) p.setT(tr(p.x, p.y));
        else p.el?.setAttribute("transform", tr(p.x, p.y));
      }
    } else if (o.pulse === "css" && d.structure) {
      for (const p of scene.pulses) if (p.el) p.el.style.offsetPath = `path("${pathD(p.edge.to)}")`;
    } else if (o.pulse === "canvas" && octx && overlay && (d.pulses || d.view || d.structure)) {
      octx.setTransform(1, 0, 0, 1, 0, 0);
      octx.clearRect(0, 0, overlay.width, overlay.height);
      if (scene.pulses.length) drawPulses(octx, scene, dpr);
    }
  };

  onSettled(() => {
    if (overlay) {
      overlay.width = Math.round(scene.vw * dpr);
      overlay.height = Math.round(scene.vh * dpr);
      octx = overlay.getContext("2d");
    }
    scene.renderer = { frame };
    scene.dirty.view = scene.dirty.structure = true;
  });

  return (
    <>
      <svg width="100%" height="100%" style={`position:absolute;inset:0;display:block;background:${BG}`}>
        <style>{CSS}</style>
        <defs>
          <For each={["spawned", "message", "changed"] as const}>
            {(kind) => (
              <marker id={`arrow-${kind}`} viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7"
                markerUnits="userSpaceOnUse" orient="auto">
                <path d="M0,0.5L8,4L0,7.5z" fill={EDGE_COLOR[kind]} />
              </marker>
            )}
          </For>
        </defs>
        <g
          ref={(el) => (view = el)}
          transform={o.pan === "attr" && solid ? scene.viewT() : undefined}
          style={o.pan === "css" ? "will-change:transform;transform-origin:0 0" : undefined}
        >
          <g>
            <For each={scene.edgesSig()} keyed={(e) => e.g.id}>
              {(e) => <path ref={(el) => (e().el = el)} class={`e ${e().g.kind}`} d={e().d()} />}
            </For>
          </g>
          <g>
            <For each={scene.nodesSig()} keyed={(n) => n.g.id}>
              {(n) => (
                <g ref={(el) => (n().el = el)} class={`n ${n().g.kind}`} transform={n().pos()}>
                  <rect width={n().g.w} height={n().g.h} rx="8" />
                  <circle cx="12" cy={n().g.h / 2} r="4" fill={STATUS[n().status]} />
                  <text x="22" y={n().g.h / 2 + 4}>{n().g.label}</text>
                </g>
              )}
            </For>
          </g>
          <Show when={o.pulse !== "canvas"}>
            <g>
              <For each={scene.pulsesSig()} keyed={(p) => p.phase}>
                {(p) => (
                  <circle
                    ref={(el) => (p().el = el)}
                    class={o.pulse === "css" ? "pulse css" : "pulse"}
                    r="4"
                    transform={o.pulse === "css" ? undefined : p().t()}
                    style={o.pulse === "css" ? `animation-delay:${-Math.round(p().phase * PULSE_MS)}ms` : undefined}
                  />
                )}
              </For>
            </g>
          </Show>
        </g>
      </svg>
      <Show when={o.pulse === "canvas"}>
        <canvas ref={(el) => (overlay = el)} style="position:absolute;inset:0;width:100%;height:100%;pointer-events:none" />
      </Show>
    </>
  );
}
