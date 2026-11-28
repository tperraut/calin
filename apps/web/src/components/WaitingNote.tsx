import { Show } from "solid-js";

/**
 * Where an answer is, once given: on its way to the server, then taken and
 * waiting for whoever asked. Nothing is optimistic, so the question stays
 * until its event arrives; this says why its buttons are held meanwhile.
 */
export function WaitingNote(props: {
  waiting: boolean;
  sending: boolean;
  /** Who acts on the answer: "agent" or "workflow". */
  asker: string;
  class: string;
}) {
  return (
    <Show when={props.waiting}>
      <span class={props.class}>
        {props.sending ? "Sending…" : `Sent, waiting for the ${props.asker}…`}
      </span>
    </Show>
  );
}
