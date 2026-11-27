// ELK, either behind elkjs' own web worker (the design's proposal) or on the main thread.

import ElkApi from "elkjs/lib/elk-api.js";
import workerUrl from "elkjs/lib/elk-worker.min.js?url";
import type { ElkLike } from "./elkopts.ts";

export function createElk(where: "worker" | "main"): Promise<ElkLike> {
  if (where === "worker") return Promise.resolve(new ElkApi({ workerUrl }) as unknown as ElkLike);
  // The bundled build runs the same GWT code in a fake worker on the calling thread.
  return import("elkjs/lib/elk.bundled.js").then((m) => new m.default() as unknown as ElkLike);
}
