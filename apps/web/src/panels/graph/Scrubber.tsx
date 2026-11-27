import { Show } from "solid-js";
import { absoluteTime, clockTime } from "../../lib/format";

/**
 * The moment the graph shows: a slider over the tree's history, a replay from
 * there to the present, and the way back to following it live.
 */
export function Scrubber(props: {
  /** First and newest event of the tree, in Unix milliseconds. */
  from: number;
  to: number;
  /** The moment on screen when not live. */
  at: number | undefined;
  live: boolean;
  playing: boolean;
  onScrub: (at: number) => void;
  onLive: (live: boolean) => void;
  onPlay: () => void;
  onPause: () => void;
}) {
  const moment = (): number => (props.live ? props.to : (props.at ?? props.to));
  const empty = (): boolean => props.to <= props.from;
  return (
    <div
      role="group"
      aria-label="Moment shown"
      class="flex shrink-0 items-center gap-1.5 border-t border-line bg-surface px-2 py-1"
    >
      <Show
        when={props.playing}
        fallback={
          <button
            type="button"
            class="btn shrink-0 px-0"
            aria-label="Play"
            title="Replay from here to the present"
            disabled={empty()}
            onClick={() => props.onPlay()}
          >
            <span aria-hidden="true">▶</span>
          </button>
        }
      >
        <button
          type="button"
          class="btn shrink-0 px-0"
          aria-label="Pause"
          title="Pause the replay"
          onClick={() => props.onPause()}
        >
          <span aria-hidden="true">❚❚</span>
        </button>
      </Show>
      <input
        type="range"
        aria-label="Moment"
        aria-valuetext={props.live ? "Live" : absoluteTime(moment())}
        class="h-11 min-w-0 flex-1 accent-(--accent)"
        min={props.from}
        max={Math.max(props.to, props.from + 1)}
        step="1"
        value={moment()}
        disabled={empty()}
        onInput={(event) => props.onScrub(Number(event.currentTarget.value))}
      />
      <output
        class="w-16 shrink-0 text-center font-mono text-xs text-muted"
        title={absoluteTime(moment())}
      >
        {clockTime(moment())}
      </output>
      <button
        type="button"
        class="btn shrink-0 px-2.5"
        aria-pressed={props.live ? "true" : "false"}
        title={props.live ? "Following the present" : "Go back to the present and follow it"}
        onClick={() => props.onLive(!props.live)}
      >
        Live
      </button>
    </div>
  );
}
