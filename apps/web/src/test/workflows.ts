// Workflow builders shared by the test files. Never imported by the app.
import type { WorkflowGraph } from "../gen/WorkflowGraph";
import type { WorkflowInfo } from "../gen/WorkflowInfo";
import type { WorkflowNodeInfo } from "../gen/WorkflowNodeInfo";

const FAILURE = new Set(["failed", "fail", "exhausted", "reject"]);

/** A node with its ports, failure flags per the spec (section 3). */
export function node(id: string, kind: string, label: string, ports: string[]): WorkflowNodeInfo {
  return { id, kind, label, ports: ports.map((name) => ({ name, failure: FAILURE.has(name) })) };
}

/** The example of docs/specs/2026-11-27-workflows.md, section 2, in file order. */
export function specGraph(): WorkflowGraph {
  return {
    nodes: [
      node("start", "start", "", ["next"]),
      node("implement", "agent", "{{task}}", ["done", "failed"]),
      node("tests", "check", "repo checks", ["pass", "fail"]),
      node("retry", "loop", "max 3", ["repeat", "exhausted"]),
      node("fix", "agent", "The checks failed", ["done", "failed"]),
      node("review", "agent", "Review the changes on this branch.", [
        "approve",
        "changes",
        "failed",
      ]),
      node("gate", "gate", "Open pull requests?", ["approve", "reject"]),
      node("ship", "ship", "", ["done", "failed"]),
      node("ok", "end", "", []),
      node("gave-up", "end", "", []),
    ],
    edges: [
      { from: "start", port: "next", to: "implement" },
      { from: "implement", port: "done", to: "tests" },
      { from: "tests", port: "pass", to: "review" },
      { from: "tests", port: "fail", to: "retry" },
      { from: "retry", port: "repeat", to: "fix" },
      { from: "retry", port: "exhausted", to: "gave-up" },
      { from: "fix", port: "done", to: "tests" },
      { from: "review", port: "approve", to: "gate" },
      { from: "review", port: "changes", to: "retry" },
      { from: "gate", port: "approve", to: "ship" },
      { from: "ship", port: "done", to: "ok" },
    ],
  };
}

/** A workflow as `GET /workflows` lists it; valid with the spec's graph unless patched. */
export function workflowInfo(id: string, patch: Partial<WorkflowInfo> = {}): WorkflowInfo {
  return {
    id,
    name: `Workflow ${id}`,
    description: null,
    builtin: false,
    budget_usd: null,
    graph: specGraph(),
    problem: null,
    ...patch,
  };
}
