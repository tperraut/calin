import { createSignal, type Accessor, type Setter } from "solid-js";

/** What a `RemoteValue` shows before its first answer, and again after a reset. */
export type Loading = { status: "loading" };
const LOADING: Loading = { status: "loading" };

/** Where a `RemoteValue` reads from, and the state it shows for each outcome. */
export type RemoteSource<T, S> = {
  read: () => Promise<T>;
  ready: (value: T) => S;
  failed: (error: unknown) => S;
};

/**
 * One server read that screens show: asked once at a time, and kept on screen
 * through a failed read once it was known. Settings, workflows and health share it.
 */
export class RemoteValue<T, S extends { status: string }> {
  readonly state: Accessor<Loading | S>;
  /** A screen asked for it, so reconnects keep it fresh. */
  wanted = false;
  private readonly set: Setter<Loading | S>;
  private readonly source: RemoteSource<T, S>;
  private isKnown = false;
  private inFlight = false;

  constructor(source: RemoteSource<T, S>) {
    this.source = source;
    [this.state, this.set] = createSignal<Loading | S>(LOADING);
  }

  /** Read at least once since the last reset. */
  get known(): boolean {
    return this.isKnown;
  }

  /** A screen wants it: reads it, and `refresh` keeps it fresh from then on. */
  want(): void {
    this.wanted = true;
    this.read();
  }

  read(): void {
    if (this.inFlight) return;
    this.inFlight = true;
    this.source.read().then(
      (value) => this.answered(value),
      (error: unknown) => this.failedWith(error),
    );
  }

  /** Reads again what a screen wanted: everything with `all`, otherwise only what never arrived. */
  refresh(all: boolean): void {
    if (this.wanted && (all || !this.isKnown)) this.read();
  }

  /** Takes a value the server gave by other means, as a save's answer. */
  take(value: T): void {
    this.isKnown = true;
    this.set(() => this.source.ready(value));
  }

  /** Shows `loading` again unless a value is known. */
  showLoading(): void {
    if (!this.isKnown) this.set(LOADING);
  }

  /** Forgets the value and that it was wanted, as when the sign-in ended. */
  reset(): void {
    this.isKnown = false;
    this.wanted = false;
    this.set(LOADING);
  }

  private answered(value: T): void {
    this.inFlight = false;
    this.take(value);
  }

  private failedWith(error: unknown): void {
    this.inFlight = false;
    // A value that was read once stays on screen through an outage.
    if (!this.isKnown) this.set(() => this.source.failed(error));
  }
}
