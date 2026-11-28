import { describe, expect, it } from "vite-plus/test";
import type { GraphEdge } from "../../gen/GraphEdge";
import { agentLook, sessionLook } from "../../lib/status";
import {
  agentNode,
  makeAgent,
  makeEdge,
  makeGraph,
  repoNode,
  userNode,
} from "../../test/orchestration";
import { layoutGraph } from "./layout";
import { describeGraph, edgeSentence, graphModel, type EdgeView } from "./model";

const model = () => graphModel(makeGraph(), "s1");

const edge = (id: string): EdgeView => {
  const found = model().edgeById.get(id);
  if (!found) throw new Error(`no edge ${id}`);
  return found;
};

describe("graphModel: the nodes", () => {
  it("names each node by its title and the role it plays", () => {
    expect(model().nodes.map((node) => node.name)).toEqual([
      "You",
      "lead",
      "explorer",
      "reviewer",
      "worker (API: add /health)",
      "worker (Web: status badge)",
      "tester",
      "api (Worktree)",
      "web (Worktree)",
      "docs (Read only)",
    ]);
  });

  it("gives a lead its session's status, any other agent its own, and a cost when it cost something", () => {
    const byId = model().nodeById;
    expect(byId.get("agent:a1")?.status).toEqual(sessionLook("running"));
    expect(byId.get("agent:a1")?.cost).toBe("$0.0421");
    expect(byId.get("agent:sub1")?.status).toEqual(agentLook("done"));
    expect(byId.get("agent:sub1")?.cost).toBeUndefined();
    expect(byId.get("repo:api")?.status).toBeUndefined();
    expect(byId.get("user")?.status).toBeUndefined();
  });

  it("marks the agents of the open session as its own", () => {
    const own = (sessionId: string) =>
      graphModel(makeGraph(), sessionId)
        .nodes.filter((node) => node.own)
        .map((node) => node.id);
    expect(own("s1")).toEqual(["agent:a1", "agent:sub1", "agent:sub2"]);
    expect(own("s3")).toEqual(["agent:a3", "agent:sub3"]);
  });

  it("falls back to the id for a blank label, and to 'Repo' for a repo without a mode", () => {
    const graph = makeGraph({
      nodes: [
        userNode(),
        agentNode(makeAgent("a1"), "user", { label: "  " }),
        repoNode("x", { repo_mode: null }),
      ],
      edges: [],
    });
    expect(graphModel(graph, "s1").nodes.map((node) => node.name)).toEqual([
      "You",
      "agent:a1 (lead)",
      "x (Repo)",
    ]);
  });

  it("knows a child's lead by its agent role when its session has no title", () => {
    const graph = makeGraph({
      nodes: [
        agentNode(makeAgent("a1"), "user"),
        agentNode(makeAgent("a2", { session_id: "s2", role: "coder" }), "agent:a1", {
          label: "worker",
        }),
      ],
      edges: [],
    });
    expect(graphModel(graph, "s1").nodeById.get("agent:a2")?.name).toBe("worker (coder)");
  });
});

describe("graphModel: the edges", () => {
  it("routes the first spawn or dispatch of a node along the tree and every other edge as a curve", () => {
    const routes = model().edges.map((one) => [one.id, one.route, one.lane]);
    expect(routes).toEqual([
      ["spawned:agent:a1:agent:sub1", "tree", 0],
      ["spawned:agent:a1:agent:sub2", "tree", 0],
      ["spawned:agent:a3:agent:sub3", "tree", 0],
      ["dispatched:agent:a1:agent:a2", "tree", 0],
      ["dispatched:agent:a1:agent:a3", "tree", 0],
      ["message:agent:a2:agent:a1", "curve", 0],
      ["message:agent:a1:agent:a2", "curve", 1],
      ["report:agent:a2:agent:a1", "curve", 2],
      ["changed:agent:a2:repo:api", "curve", 0],
      ["changed:agent:a1:repo:api", "curve", 0],
      ["changed:agent:sub3:repo:web", "curve", 0],
      ["link:agent:a1", "tree", 0],
    ]);
  });

  it("draws a second spawn of the same node as a curve, and drops edges to nodes it does not hold", () => {
    const whole = makeGraph();
    const graph = makeGraph({
      edges: [
        ...whole.edges,
        makeEdge("spawned", "agent:a1", "agent:sub1", { id: "again" }),
        makeEdge("message", "agent:a1", "agent:gone"),
      ],
    });
    const built = graphModel(graph, "s1");
    expect(built.edgeById.get("again")?.route).toBe("curve");
    expect(built.edgeById.has("message:agent:a1:agent:gone")).toBe(false);
  });

  it("links a node to its parent only when no edge carries it and the parent is in the graph", () => {
    const graph = makeGraph({
      nodes: [
        userNode(),
        agentNode(makeAgent("a1"), "user"),
        agentNode(makeAgent("lost"), "agent:nowhere"),
        repoNode("api"),
      ],
      edges: [],
    });
    expect(graphModel(graph, "s1").edges).toEqual([
      {
        id: "link:agent:a1",
        from: "user",
        to: "agent:a1",
        route: "tree",
        edge: undefined,
        kind: "link",
        count: 1,
        lane: 0,
      },
    ]);
  });
});

describe("edgeSentence", () => {
  it("says who did what to whom, and how often", () => {
    const sentence = (id: string) => edgeSentence(edge(id), model());
    expect(sentence("spawned:agent:a1:agent:sub1")).toBe("lead spawned explorer");
    expect(sentence("dispatched:agent:a1:agent:a2")).toBe(
      "lead dispatched worker (API: add /health)",
    );
    expect(sentence("message:agent:a2:agent:a1")).toBe(
      "worker (API: add /health) sent 2 notes to lead",
    );
    expect(sentence("message:agent:a1:agent:a2")).toBe(
      "lead sent 1 note to worker (API: add /health)",
    );
    expect(sentence("report:agent:a2:agent:a1")).toBe(
      "worker (API: add /health) sent 1 report to lead",
    );
    expect(sentence("changed:agent:a2:repo:api")).toBe(
      "worker (API: add /health) changed 4 files in api (Worktree)",
    );
    expect(sentence("changed:agent:a1:repo:api")).toBe("lead changed 1 file in api (Worktree)");
    expect(sentence("link:agent:a1")).toBe("You started lead");
  });

  it("names an unknown node by its id, and an unknown kind by its wire name", () => {
    const raw: GraphEdge = { ...makeEdge("report", "agent:x", "agent:y", { count: 3 }) };
    const odd: EdgeView = {
      ...raw,
      kind: "zap" as unknown as EdgeView["kind"],
      route: "curve",
      edge: raw,
      lane: 0,
    };
    expect(edgeSentence(odd, model())).toBe("agent:x to agent:y: zap, 3");
    expect(edgeSentence({ ...odd, kind: "report" }, model())).toBe(
      "agent:x sent 3 reports to agent:y",
    );
  });
});

describe("describeGraph", () => {
  it("tells the tree as nested items, then the repos, then every edge but the tree links", () => {
    const built = model();
    const text = describeGraph(built, layoutGraph(makeGraph().nodes, undefined, "indented"));
    const lead = "lead, Running, $0.0421, in the open session";
    expect(text.tree).toEqual([
      {
        id: "user",
        text: "You",
        children: [
          {
            id: "agent:a1",
            text: lead,
            children: [
              {
                id: "agent:sub1",
                text: "explorer, Done, in the open session",
                children: [],
              },
              {
                id: "agent:sub2",
                text: "reviewer, Running, in the open session",
                children: [],
              },
              { id: "agent:a2", text: "worker (API: add /health), Review", children: [] },
              {
                id: "agent:a3",
                text: "worker (Web: status badge), Running",
                children: [{ id: "agent:sub3", text: "tester, Running", children: [] }],
              },
            ],
          },
        ],
      },
    ]);
    expect(text.repos).toEqual([
      "Repo api (Worktree)",
      "Repo web (Worktree)",
      "Repo docs (Read only)",
    ]);
    expect(text.edges).toHaveLength(built.edges.length - 1);
    expect(text.edges[0]).toBe("lead spawned explorer");
  });

  it("leaves out of the tree a node the model does not hold", () => {
    const built = model();
    const layout = layoutGraph(makeGraph().nodes, undefined, "indented");
    const text = describeGraph(built, {
      ...layout,
      order: ["ghost", ...layout.order],
      children: new Map([...layout.children, ["user", ["ghost", "agent:a1"]]]),
    });
    expect(text.tree.map((item) => item.id)).toEqual(["user"]);
    expect(text.tree[0]?.children.map((item) => item.id)).toEqual(["agent:a1"]);
  });
});
