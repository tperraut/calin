import { Show } from "solid-js";

/** Why the last try failed, in the server's words; read out as it appears. */
export function ErrorNote(props: { message: string | undefined; id?: string }) {
  return (
    <Show when={props.message}>
      {(message) => (
        <p id={props.id} role="alert" class="text-sm text-tone-danger">
          {message()}
        </p>
      )}
    </Show>
  );
}
