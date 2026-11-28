import { createSignal, type Accessor } from "solid-js";
import { errorMessage } from "./errors";

export type Request<Args extends unknown[] = []> = {
  /** True from the click until the server answers; the button stays disabled meanwhile. */
  busy: Accessor<boolean>;
  /** The server's reason when the last call failed. */
  error: Accessor<string | undefined>;
  /** Does nothing while a call is in flight. */
  run(...args: Args): Promise<void>;
  /** Forgets the last failure, as when the reader takes the question back. */
  clearError(): void;
};

/**
 * State of a command that changes nothing on screen by itself: what it did
 * shows up when the session's events arrive, so only "in flight" and "failed"
 * are tracked here.
 */
export function createRequest<Args extends unknown[] = []>(
  call: (...args: Args) => Promise<void>,
): Request<Args> {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  return {
    busy,
    error,
    clearError: () => setError(undefined),
    async run(...args: Args) {
      if (busy()) return;
      setBusy(true);
      setError(undefined);
      try {
        await call(...args);
      } catch (reason) {
        setError(errorMessage(reason));
      } finally {
        setBusy(false);
      }
    },
  };
}
