// Synthetic session graph, produced as an ordered replay: one node per step,
// together with the edges that become visible at that step.

export type NodeKind = "user" | "agent" | "sub" | "repo";
export type EdgeKind = "spawned" | "message" | "changed";

export interface GNode {
  id: string;
  kind: NodeKind;
  label: string;
  parent?: string;
  depth: number;
  w: number;
  h: number;
}

export interface GEdge {
  id: string;
  kind: EdgeKind;
  from: string;
  to: string;
}

export interface Step {
  node: GNode;
  edges: GEdge[];
}

const SIZE: Record<NodeKind, [number, number]> = {
  user: [72, 36],
  agent: [132, 40],
  sub: [104, 30],
  repo: [120, 36],
};

function mulberry32(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const ROLES = ["plan", "impl", "review", "test", "fix", "docs", "bench", "lint", "migrate", "api"];
const REPOS = ["calin", "web", "proto", "infra", "docs", "sdk", "cli", "design"];

/** `nNodes` nodes and about `nEdges` distinct edges (exact when enough distinct pairs exist). */
export function generate(nNodes: number, nEdges: number, seed = 1): Step[] {
  const rnd = mulberry32(seed);
  const pick = <T>(xs: T[]): T => xs[Math.floor(rnd() * xs.length)];
  const steps: Step[] = [];
  const seen = new Set<string>();
  let edgeSeq = 0;
  const edge = (kind: EdgeKind, from: string, to: string): GEdge | null => {
    const key = `${kind}:${from}>${to}`;
    if (from === to || seen.has(key)) return null;
    seen.add(key);
    return { id: `e${edgeSeq++}`, kind, from, to };
  };
  const node = (id: string, kind: NodeKind, label: string, depth: number, parent?: string): GNode => ({
    id, kind, label, depth, parent, w: SIZE[kind][0], h: SIZE[kind][1],
  });

  const nRepos = Math.max(3, Math.min(8, Math.round(3 + nNodes / 40)));
  const user = node("user", "user", "you", 0);
  steps.push({ node: user, edges: [] });
  const lead = node("a0", "agent", "lead", 0, "user");
  steps.push({ node: lead, edges: [edge("spawned", "user", "a0")!] });
  const repos: GNode[] = [];
  for (let i = 0; i < nRepos; i++) {
    const r = node(`r${i}`, "repo", REPOS[i], 0);
    repos.push(r);
    steps.push({ node: r, edges: [] });
  }

  const agents: GNode[] = [lead]; // everything that can send messages
  const spawners: GNode[] = [lead]; // full agents shallow enough to spawn
  const children = new Map<string, GNode[]>([["a0", []]]);
  const remaining = nNodes - steps.length;
  const extraTotal = Math.max(0, nEdges - (1 + remaining));
  let extra = 0;

  for (let i = 0; i < remaining; i++) {
    // Spawns come in bursts: usually from a recently created spawner, sometimes from anyone.
    // Deeper agents delegate less often, which keeps the tree from being all leaves.
    const recent = spawners.slice(-6);
    let parent = lead;
    for (let t = 0; i >= 6 && t < 20; t++) {
      const candidate = rnd() < 0.55 ? pick(recent) : pick(spawners);
      if (rnd() < (candidate.depth >= 2 ? 0.3 : 1)) {
        parent = candidate;
        break;
      }
    }
    const depth = parent.depth + 1;
    const kind: NodeKind = depth >= 3 || rnd() < 0.25 ? "sub" : "agent";
    const id = `a${i + 1}`;
    const n = node(id, kind, `${pick(ROLES)}-${i + 1}`, depth, parent.id);
    const edges: GEdge[] = [edge("spawned", parent.id, id)!];
    agents.push(n);
    children.get(parent.id)!.push(n);
    children.set(id, []);
    if (kind === "agent" && depth <= 2) spawners.push(n);

    const due = Math.round((extraTotal * (i + 1)) / remaining);
    for (let tries = 0; extra < due && tries < 200; tries++) {
      const a = rnd() < 0.6 ? pick(agents.slice(-12)) : pick(agents);
      let e: GEdge | null = null;
      const r = rnd();
      if (r < 0.35) {
        // Skewed: the first repositories take most of the changes.
        e = edge("changed", a.id, repos[Math.floor(rnd() * rnd() * repos.length)].id);
      } else if (a.parent && a.parent !== "user") {
        const sibs = children.get(a.parent)!.filter((s) => s.id !== a.id);
        if (r < 0.6) e = edge("message", a.id, a.parent);
        else if (r < 0.87 && sibs.length) e = edge("message", a.id, pick(sibs).id);
        else e = edge("message", a.parent, a.id);
      }
      if (e) {
        edges.push(e);
        extra++;
      }
    }
    steps.push({ node: n, edges });
  }
  return steps;
}

/** The graph after the first `count` steps. */
export function prefix(steps: Step[], count: number): { nodes: GNode[]; edges: GEdge[] } {
  const part = steps.slice(0, count);
  return { nodes: part.map((s) => s.node), edges: part.flatMap((s) => s.edges) };
}
