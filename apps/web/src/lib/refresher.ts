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
class DebouncedRefresher implements Refresher {
  private readonly run: () => Promise<void>;
  private readonly delayMs: number;
  private visible = false;
  private stale = false;
  private running = false;
  private again = false;
  private disposed = false;
  private timer: ReturnType<typeof setTimeout> | undefined;

  constructor(run: () => Promise<void>, delayMs: number) {
    this.run = run;
    this.delayMs = delayMs;
  }

  request(): void {
    if (!this.visible) {
      this.stale = true;
      return;
    }
    this.cancel();
    this.timer = setTimeout(() => this.fire(), this.delayMs);
  }

  now(): void {
    this.cancel();
    this.startOrHold();
  }

  setVisible(next: boolean): void {
    this.visible = next;
    if (!next) {
      if (this.cancel()) this.stale = true;
    } else if (this.stale) {
      this.stale = false;
      this.start();
    }
  }

  dispose(): void {
    this.disposed = true;
    this.cancel();
  }

  private fire(): void {
    this.timer = undefined;
    this.start();
  }

  private startOrHold(): void {
    if (this.visible) this.start();
    else this.stale = true;
  }

  private start(): void {
    if (this.disposed) return;
    if (this.running) {
      this.again = true;
      return;
    }
    this.running = true;
    void this.run().finally(() => this.finished());
  }

  private finished(): void {
    this.running = false;
    if (!this.again) return;
    this.again = false;
    this.startOrHold();
  }

  private cancel(): boolean {
    if (this.timer === undefined) return false;
    clearTimeout(this.timer);
    this.timer = undefined;
    return true;
  }
}

export function createRefresher(run: () => Promise<void>, delayMs = 500): Refresher {
  return new DebouncedRefresher(run, delayMs);
}
