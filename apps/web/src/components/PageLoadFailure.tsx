/** The server's words when a page's load failed; `undefined` while it loads or once it is ready. */
export const loadFailure = (
  load: { status: "loading" | "ready" } | { status: "error"; message: string },
): string | undefined => (load.status === "error" ? load.message : undefined);

/** A page whose content could not be loaded: the server's words, and a way to ask again. */
export function PageLoadFailure(props: { what: string; message: string; onRetry: () => void }) {
  return (
    <div role="alert" class="flex flex-col items-start gap-2 text-sm">
      <p class="wrap-break-word">
        {props.what} could not be loaded: {props.message}
      </p>
      <button type="button" class="btn" onClick={() => props.onRetry()}>
        Try again
      </button>
    </div>
  );
}
