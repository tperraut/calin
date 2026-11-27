import { Show } from "solid-js";
import type { ItemStatus } from "../gen/ItemStatus";
import type { SessionStatus } from "../gen/SessionStatus";
import { interruptedLook, itemLook, sessionLook, type Look, type Tone } from "../lib/status";

const toneClass: Record<Tone, string> = {
  neutral: "text-tone-neutral",
  active: "text-tone-active",
  attention: "text-tone-attention",
  success: "text-tone-success",
  danger: "text-tone-danger",
};

/** A status as a dot and a word. The word is always there: color is never the only cue. */
export function Badge(props: { look: Look; pulse?: boolean }) {
  return (
    <span
      class={[
        "inline-flex shrink-0 items-center gap-1.5 rounded-full border border-line bg-surface px-2 py-0.5 text-xs font-medium whitespace-nowrap",
        toneClass[props.look.tone],
      ]}
    >
      <span
        aria-hidden="true"
        class={["size-1.5 rounded-full bg-current", { pulse: props.pulse === true }]}
      />
      {props.look.label}
    </span>
  );
}

export function SessionStatusBadge(props: { status: SessionStatus }) {
  return (
    <Badge
      look={sessionLook(props.status)}
      // `waiting` pulses too: it is the one status that needs the reader.
      pulse={
        props.status === "running" || props.status === "preparing" || props.status === "waiting"
      }
    />
  );
}

/** `interrupted`: the item was still running when its turn ended (see the reducer). */
export function ItemStatusBadge(props: { status: ItemStatus; interrupted?: boolean }) {
  return (
    <Show
      when={props.interrupted === true}
      fallback={<Badge look={itemLook(props.status)} pulse={props.status === "running"} />}
    >
      <Badge look={interruptedLook} />
    </Show>
  );
}
