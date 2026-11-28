import { createSignal, type Accessor, type Setter } from "solid-js";
import type { Graph } from "../../gen/Graph";
import type { Timeline } from "../../gen/Timeline";
import { errorMessage } from "../../lib/errors";
import { createRefresher, type Refresher } from "../../lib/refresher";

export type OrchestrationView = "graph" | "timeline";

export type OrchestrationData = {
  /** The last graph read; it stays on screen while another one is on its way. */
  graph: Accessor<Graph | undefined>;
  /** The server's reason when the last read of the graph failed. */
  graphError: Accessor<string | undefined>;
  timeline: Accessor<Timeline | undefined>;
  timelineError: Accessor<string | undefined>;
  /** True while a read is in flight. */
  refreshing: Accessor<boolean>;
  /** True while the graph follows the present; false once a past moment is shown. */
  live: Accessor<boolean>;
  /** The moment shown when not live, in Unix milliseconds. */
  at: Accessor<number | undefined>;
  playing: Accessor<boolean>;
  /** Back to the present (and stays there), or frozen at the moment now on screen. */
  setLive(live: boolean): void;
  /** Shows the graph as it was at `at`; several calls in a row cost one read. */
  scrub(at: number): void;
  /** Replays from the moment on screen (from the start, when live) to the present. */
  play(): void;
  pause(): void;
  /** Something happened in the tree: read again soon, once. */
  changed(): void;
  /** Reads at once, after a failure. */
  retry(): void;
  /** Nothing is read while the panel is hidden or the tab is in the background. */
  setVisible(visible: boolean): void;
  /** Which of the two is on screen; the timeline is only read once it has been. */
  setView(view: OrchestrationView): void;
  dispose(): void;
};

export type OrchestrationDeps = {
  graph(at?: number): Promise<Graph>;
  timeline(): Promise<Timeline>;
  /** How long changes are gathered before one read. */
  delayMs?: number;
};

/** Events arrive in bursts: one read per burst. */
export const REFETCH_DELAY_MS = 400;
/** The slider moves many times a second; the graph follows when it rests. */
export const SCRUB_DELAY_MS = 150;
/** One step of a replay. */
export const PLAY_TICK_MS = 500;
/** A replay of the whole history takes about this long. */
export const PLAY_TOTAL_MS = 20_000;
/** Bounds on the history one step covers: a short session still shows steps, a long one skips no hours. */
export const PLAY_MIN_STEP_MS = 250;
export const PLAY_MAX_STEP_MS = 10 * 60_000;

/** How much history one tick of the replay covers, for a history `span` milliseconds long. */
export function playStep(span: number): number {
  const steps = PLAY_TOTAL_MS / PLAY_TICK_MS;
  return Math.min(PLAY_MAX_STEP_MS, Math.max(PLAY_MIN_STEP_MS, span / steps));
}

/**
 * The data of the Graph panel: the graph (live, or at a past moment) and the
 * timeline, read through the session store. It debounces what the events ask
 * for, holds reads while out of sight, and never drops the data on screen for
 * a read in flight or a failed one.
 */
export function createOrchestrationData(deps: OrchestrationDeps): OrchestrationData {
  return new Orchestration(deps);
}

// A class rather than a closure of functions: each method then stands, and is measured, on its own.
// `private`, not `#`: the complexity report does not see `#` methods.
class Orchestration implements OrchestrationData {
  readonly graph: Accessor<Graph | undefined>;
  readonly graphError: Accessor<string | undefined>;
  readonly timeline: Accessor<Timeline | undefined>;
  readonly timelineError: Accessor<string | undefined>;
  readonly live: Accessor<boolean>;
  readonly at: Accessor<number | undefined>;
  readonly playing: Accessor<boolean>;
  private readonly setGraph: Setter<Graph | undefined>;
  private readonly setGraphError: Setter<string | undefined>;
  private readonly setTimeline: Setter<Timeline | undefined>;
  private readonly setTimelineError: Setter<string | undefined>;
  private readonly inFlight: Accessor<number>;
  private readonly setInFlight: Setter<number>;
  private readonly setLiveSignal: Setter<boolean>;
  private readonly setAtSignal: Setter<number | undefined>;
  private readonly setPlayingSignal: Setter<boolean>;
  private readonly deps: OrchestrationDeps;
  private readonly graphRefresher: Refresher;
  private readonly timelineRefresher: Refresher;

  // Plain mirrors: timers and promise callbacks decide on these, not on signals a tick behind.
  private isLive = true;
  private moment: number | undefined;
  private isPlaying = false;
  private range: { from: number; to: number } | undefined;
  private visible = false;
  private view: OrchestrationView = "graph";
  private timelineWanted = false;
  private disposed = false;
  // Only the answer to the newest question is shown.
  private asked = 0;
  private reading = false;
  private scrubTimer: ReturnType<typeof setTimeout> | undefined;
  private playTimer: ReturnType<typeof setInterval> | undefined;

  constructor(deps: OrchestrationDeps) {
    this.deps = deps;
    [this.graph, this.setGraph] = createSignal<Graph>();
    [this.graphError, this.setGraphError] = createSignal<string>();
    [this.timeline, this.setTimeline] = createSignal<Timeline>();
    [this.timelineError, this.setTimelineError] = createSignal<string>();
    [this.inFlight, this.setInFlight] = createSignal(0);
    [this.live, this.setLiveSignal] = createSignal(true);
    [this.at, this.setAtSignal] = createSignal<number>();
    [this.playing, this.setPlayingSignal] = createSignal(false);
    const delay = deps.delayMs ?? REFETCH_DELAY_MS;
    // A past moment does not change with new events: only the live graph is read again.
    this.graphRefresher = createRefresher(
      () => (this.isLive ? this.readGraph(undefined) : Promise.resolve()),
      delay,
    );
    this.timelineRefresher = createRefresher(() => this.readTimeline(), delay);
    // Marked stale while hidden: the first read happens when the panel is first seen.
    this.graphRefresher.now();
  }

  refreshing(): boolean {
    return this.inFlight() > 0;
  }

  setLive(next: boolean): void {
    if (next) {
      this.goLive();
    } else if (this.isLive) {
      this.stopPlaying();
      this.freeze(this.range?.to ?? Date.now());
    }
  }

  scrub(next: number): void {
    this.stopPlaying();
    this.freeze(next);
    clearTimeout(this.scrubTimer);
    this.scrubTimer = setTimeout(() => this.scrubRested(next), SCRUB_DELAY_MS);
  }

  play(): void {
    const range = this.range;
    if (this.isPlaying || !range) return;
    clearTimeout(this.scrubTimer);
    const start = this.replayStart(range);
    this.freeze(start);
    this.isPlaying = true;
    this.setPlayingSignal(true);
    void this.readGraph(start);
    this.playTimer = setInterval(() => this.tick(), PLAY_TICK_MS);
  }

  pause(): void {
    this.stopPlaying();
  }

  changed(): void {
    this.graphRefresher.request();
    if (this.timelineWanted) this.timelineRefresher.request();
  }

  retry(): void {
    if (this.isLive) this.graphRefresher.now();
    else if (this.moment !== undefined) void this.readGraph(this.moment);
    if (this.timelineWanted) this.timelineRefresher.now();
  }

  setVisible(next: boolean): void {
    this.visible = next;
    this.applyVisibility();
  }

  setView(next: OrchestrationView): void {
    this.view = next;
    if (next === "timeline" && !this.timelineWanted) {
      this.timelineWanted = true;
      this.timelineRefresher.now();
    }
    this.applyVisibility();
  }

  dispose(): void {
    this.disposed = true;
    this.stopPlaying();
    clearTimeout(this.scrubTimer);
    this.graphRefresher.dispose();
    this.timelineRefresher.dispose();
  }

  private async readGraph(wanted: number | undefined): Promise<void> {
    this.asked += 1;
    const mine = this.asked;
    this.reading = true;
    this.setInFlight((count) => count + 1);
    try {
      this.showGraph(mine, await this.deps.graph(wanted));
    } catch (error) {
      if (this.answers(mine)) this.setGraphError(errorMessage(error));
    } finally {
      if (mine === this.asked) this.reading = false;
      this.setInFlight((count) => count - 1);
    }
  }

  /** True when question `mine` is still the one whose answer is shown. */
  private answers(mine: number): boolean {
    return !this.disposed && mine === this.asked;
  }

  private showGraph(mine: number, loaded: Graph): void {
    if (!this.answers(mine)) return;
    this.range = { from: loaded.from_ts, to: loaded.to_ts };
    this.setGraph(loaded);
    this.setGraphError(undefined);
  }

  private async readTimeline(): Promise<void> {
    this.setInFlight((count) => count + 1);
    try {
      this.showTimeline(await this.deps.timeline());
    } catch (error) {
      if (!this.disposed) this.setTimelineError(errorMessage(error));
    } finally {
      this.setInFlight((count) => count - 1);
    }
  }

  private showTimeline(loaded: Timeline): void {
    if (this.disposed) return;
    this.setTimeline(loaded);
    this.setTimelineError(undefined);
  }

  private applyVisibility(): void {
    this.graphRefresher.setVisible(this.visible);
    this.timelineRefresher.setVisible(this.visible && this.view === "timeline");
  }

  private setMoment(next: number | undefined): void {
    this.moment = next;
    this.setAtSignal(next);
  }

  private stopPlaying(): void {
    if (this.playTimer !== undefined) clearInterval(this.playTimer);
    this.playTimer = undefined;
    this.isPlaying = false;
    this.setPlayingSignal(false);
  }

  private goLive(): void {
    this.stopPlaying();
    clearTimeout(this.scrubTimer);
    this.isLive = true;
    this.setLiveSignal(true);
    this.setMoment(undefined);
    this.graphRefresher.now();
  }

  private freeze(next: number): void {
    this.isLive = false;
    this.setLiveSignal(false);
    this.setMoment(next);
  }

  private scrubRested(next: number): void {
    this.scrubTimer = undefined;
    if (!this.isLive && this.visible) void this.readGraph(next);
  }

  /** From the present there is nothing left to replay: start over. */
  private replayStart(range: { from: number; to: number }): number {
    const moment = this.moment;
    return this.isLive || moment === undefined || moment >= range.to ? range.from : moment;
  }

  private tick(): void {
    // A slow answer holds the replay rather than piling requests up.
    if (!this.visible || this.reading || !this.range || this.moment === undefined) return;
    const next = this.moment + playStep(this.range.to - this.range.from);
    if (next >= this.range.to) {
      this.goLive();
      return;
    }
    this.setMoment(next);
    void this.readGraph(next);
  }
}
