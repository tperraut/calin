// The non-ELK alternative: a tree layout over the "spawned" edges, written so that adding a
// node changes as little as possible. Children keep their arrival order, every depth has a
// fixed column, and a new leaf only pushes down what is below it by one slot.

import { LAYER_GAP, NODE_GAP, type LayoutOut, type Pos } from "./elkopts.ts";
import type { GNode } from "./gen.ts";

export function treeLayout(nodes: GNode[], align: "top" | "center"): LayoutOut {
  const kids = new Map<string, GNode[]>();
  const roots: GNode[] = [];
  const repos: GNode[] = [];
  for (const n of nodes) {
    if (n.kind === "repo") repos.push(n);
    else if (n.parent) {
      const list = kids.get(n.parent);
      if (list) list.push(n);
      else kids.set(n.parent, [n]);
    } else roots.push(n);
  }
  // Column widths per depth, from the widest node that lives there.
  const widths: number[] = [];
  const level = new Map<string, number>();
  const walk = (n: GNode, depth: number) => {
    level.set(n.id, depth);
    widths[depth] = Math.max(widths[depth] ?? 0, n.w);
    for (const k of kids.get(n.id) ?? []) walk(k, depth + 1);
  };
  for (const r of roots) walk(r, 0);
  const xs: number[] = [];
  let x = 0;
  for (let d = 0; d <= widths.length; d++) {
    xs[d] = x;
    x += (widths[d] ?? 0) + LAYER_GAP;
  }
  const pos = new Map<string, Pos>();
  let h = 0;
  const place = (n: GNode, top: number): number => {
    const own = n.h + NODE_GAP;
    let y = top;
    for (const k of kids.get(n.id) ?? []) y += place(k, y);
    const span = Math.max(y - top, own);
    pos.set(n.id, { x: xs[level.get(n.id)!], y: align === "top" ? top : top + (span - own) / 2 });
    return span;
  };
  for (const r of roots) h += place(r, h);
  // Repositories get their own column to the right of the deepest agents.
  const repoX = xs[widths.length];
  let ry = 0, rw = 0;
  for (const r of repos) {
    pos.set(r.id, { x: repoX, y: ry });
    ry += r.h + NODE_GAP;
    rw = Math.max(rw, r.w);
  }
  return { pos, routes: new Map(), w: repoX + rw, h: Math.max(h, ry) };
}
