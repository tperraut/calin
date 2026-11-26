/** Defers `run`; injectable so state can be tested without a frame clock. */
export type Schedule = (run: () => void) => void;

/** Next frame while the page is visible; a timer otherwise, since hidden pages get no frames. */
export const nextFrame: Schedule = (run) => {
  if (typeof requestAnimationFrame === "function" && document.visibilityState === "visible") {
    requestAnimationFrame(run);
  } else {
    setTimeout(run, 50);
  }
};

/**
 * Returns a trigger that runs `run` once per scheduled slot however often it
 * is called. A replay of thousands of events then costs one render, not one each.
 */
export function coalesce(run: () => void, schedule: Schedule = nextFrame): () => void {
  let pending = false;
  return () => {
    if (pending) return;
    pending = true;
    schedule(() => {
      pending = false;
      run();
    });
  };
}
