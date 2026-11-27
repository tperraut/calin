export type Refresher = {
  /** Something changed: refresh soon, once, however many times this is called meanwhile. */
  request(): void;
  /** Refresh at once (the panel opened, the reader asked). */
  now(): void;
  /** While not visible nothing is fetched; what was missed is fetched on the way back. */
  setVisible(visible: boolean): void;
  dispose(): void;
};

/** Debounces and serializes a refresh, and holds it while its screen is out of sight. */
export function createRefresher(run: () => Promise<void>, delayMs = 500): Refresher {
  let visible = false;
  let stale = false;
  let running = false;
  let again = false;
  let disposed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;

  function start(): void {
    if (disposed) return;
    if (running) {
      again = true;
      return;
    }
    running = true;
    void run().finally(() => {
      running = false;
      if (!again) return;
      again = false;
      if (visible) start();
      else stale = true;
    });
  }

  function cancel(): boolean {
    if (timer === undefined) return false;
    clearTimeout(timer);
    timer = undefined;
    return true;
  }

  return {
    request() {
      if (!visible) {
        stale = true;
        return;
      }
      cancel();
      timer = setTimeout(() => {
        timer = undefined;
        start();
      }, delayMs);
    },
    now() {
      cancel();
      if (visible) start();
      else stale = true;
    },
    setVisible(next) {
      visible = next;
      if (!next) {
        if (cancel()) stale = true;
      } else if (stale) {
        stale = false;
        start();
      }
    },
    dispose() {
      disposed = true;
      cancel();
    },
  };
}
