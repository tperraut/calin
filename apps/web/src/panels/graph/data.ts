import { createSignal, type Accessor } from "solid-js";
import type { Graph } from "../../gen/Graph";
import type { Timeline } from "../../gen/Timeline";
import { errorMessage } from "../../lib/errors";
import { createRefresher } from "../../lib/refresher";

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
  const [graph, setGraph] = createSignal<Graph>();
  const [graphError, setGraphError] = createSignal<string>();
  const [timeline, setTimeline] = createSignal<Timeline>();
  const [timelineError, setTimelineError] = createSignal<string>();
  const [inFlight, setInFlight] = createSignal(0);
  const [live, setLiveSignal] = createSignal(true);
  const [at, setAtSignal] = createSignal<number>();
  const [playing, setPlayingSignal] = createSignal(false);

  // Plain mirrors: timers and promise callbacks decide on these, not on signals a tick behind.
  let isLive = true;
  let moment: number | undefined;
  let isPlaying = false;
  let range: { from: number; to: number } | undefined;
  let visible = false;
  let view: OrchestrationView = "graph";
  let timelineWanted = false;
  let disposed = false;
  // Only the answer to the newest question is shown.
  let asked = 0;
  let reading = false;
  let scrubTimer: ReturnType<typeof setTimeout> | undefined;
  let playTimer: ReturnType<typeof setInterval> | undefined;

  async function readGraph(wanted: number | undefined): Promise<void> {
    asked += 1;
    const mine = asked;
    reading = true;
    setInFlight((count) => count + 1);
    try {
      const loaded = await deps.graph(wanted);
      if (disposed || mine !== asked) return;
      range = { from: loaded.from_ts, to: loaded.to_ts };
      setGraph(loaded);
      setGraphError(undefined);
    } catch (error) {
      if (!disposed && mine === asked) setGraphError(errorMessage(error));
    } finally {
      if (mine === asked) reading = false;
      setInFlight((count) => count - 1);
    }
  }

  async function readTimeline(): Promise<void> {
    setInFlight((count) => count + 1);
    try {
      const loaded = await deps.timeline();
      if (disposed) return;
      setTimeline(loaded);
      setTimelineError(undefined);
    } catch (error) {
      if (!disposed) setTimelineError(errorMessage(error));
    } finally {
      setInFlight((count) => count - 1);
    }
  }

  const delay = deps.delayMs ?? REFETCH_DELAY_MS;
  // A past moment does not change with new events: only the live graph is read again.
  const graphRefresher = createRefresher(
    () => (isLive ? readGraph(undefined) : Promise.resolve()),
    delay,
  );
  const timelineRefresher = createRefresher(readTimeline, delay);
  // Marked stale while hidden: the first read happens when the panel is first seen.
  graphRefresher.now();

  function applyVisibility(): void {
    graphRefresher.setVisible(visible);
    timelineRefresher.setVisible(visible && view === "timeline");
  }

  function setMoment(next: number | undefined): void {
    moment = next;
    setAtSignal(next);
  }

  function stopPlaying(): void {
    if (playTimer !== undefined) clearInterval(playTimer);
    playTimer = undefined;
    isPlaying = false;
    setPlayingSignal(false);
  }

  function goLive(): void {
    stopPlaying();
    clearTimeout(scrubTimer);
    isLive = true;
    setLiveSignal(true);
    setMoment(undefined);
    graphRefresher.now();
  }

  function freeze(next: number): void {
    isLive = false;
    setLiveSignal(false);
    setMoment(next);
  }

  function tick(): void {
    // A slow answer holds the replay rather than piling requests up.
    if (!visible || reading || !range || moment === undefined) return;
    const next = moment + playStep(range.to - range.from);
    if (next >= range.to) {
      goLive();
      return;
    }
    setMoment(next);
    void readGraph(next);
  }

  return {
    graph,
    graphError,
    timeline,
    timelineError,
    refreshing: () => inFlight() > 0,
    live,
    at,
    playing,
    setLive(next) {
      if (next) {
        goLive();
      } else if (isLive) {
        stopPlaying();
        freeze(range?.to ?? Date.now());
      }
    },
    scrub(next) {
      stopPlaying();
      freeze(next);
      clearTimeout(scrubTimer);
      scrubTimer = setTimeout(() => {
        scrubTimer = undefined;
        if (!isLive && visible) void readGraph(next);
      }, SCRUB_DELAY_MS);
    },
    play() {
      if (isPlaying || !range) return;
      clearTimeout(scrubTimer);
      // From the present there is nothing left to replay: start over.
      const start = isLive || moment === undefined || moment >= range.to ? range.from : moment;
      freeze(start);
      isPlaying = true;
      setPlayingSignal(true);
      void readGraph(start);
      playTimer = setInterval(tick, PLAY_TICK_MS);
    },
    pause: stopPlaying,
    changed() {
      graphRefresher.request();
      if (timelineWanted) timelineRefresher.request();
    },
    retry() {
      if (isLive) graphRefresher.now();
      else if (moment !== undefined) void readGraph(moment);
      if (timelineWanted) timelineRefresher.now();
    },
    setVisible(next) {
      visible = next;
      applyVisibility();
    },
    setView(next) {
      view = next;
      if (next === "timeline" && !timelineWanted) {
        timelineWanted = true;
        timelineRefresher.now();
      }
      applyVisibility();
    },
    dispose() {
      disposed = true;
      stopPlaying();
      clearTimeout(scrubTimer);
      graphRefresher.dispose();
      timelineRefresher.dispose();
    },
  };
}
