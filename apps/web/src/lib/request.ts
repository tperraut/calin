import { createSignal, type Accessor } from "solid-js";
import { errorMessage } from "./errors";

export type Request = {
  /** True from the click until the server answers; the button stays disabled meanwhile. */
  busy: Accessor<boolean>;
  /** The server's reason when the last call failed. */
  error: Accessor<string | undefined>;
  run(): Promise<void>;
};

/**
 * State of a command that changes nothing on screen by itself: what it did
 * shows up when the session's events arrive, so only "in flight" and "failed"
 * are tracked here.
 */
export function createRequest(call: () => Promise<void>): Request {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  return {
    busy,
    error,
    async run() {
      if (busy()) return;
      setBusy(true);
      setError(undefined);
      try {
        await call();
      } catch (reason) {
        setError(errorMessage(reason));
      } finally {
        setBusy(false);
      }
    },
  };
}
