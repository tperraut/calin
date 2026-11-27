import { createSignal, Show, useContext } from "solid-js";
import { errorMessage } from "../lib/errors";
import { AppContext } from "../state/app";

/**
 * What stands in for the app when nobody is signed in. It never redirects by
 * itself, so a sign-in that keeps failing cannot loop (remote-access spec, section 3).
 */
export function SignInScreen(props: { onShowUnsaved: () => void }) {
  const app = useContext(AppContext);
  const ended = (): boolean => {
    const state = app.auth();
    return state.status === "signed_out" && state.ended;
  };
  return (
    <div class="flex flex-1 flex-col items-center justify-center gap-4 p-6 text-center">
      <h1 class="text-2xl font-semibold">Calin</h1>
      <p class="max-w-sm text-sm text-muted">
        {ended()
          ? "Your session ended. Sign in again."
          : "Sign in to reach the agents on this server."}
      </p>
      <Show when={app.unsavedWork()}>
        {(unsaved) => (
          <div
            role="alert"
            class="flex max-w-sm flex-col items-center gap-2 rounded-md border border-tone-attention bg-attention-soft px-3 py-2 text-sm"
          >
            <p>
              {unsaved()} They are still in this tab, but signing in reloads the page: your browser
              asks before they are lost.
            </p>
            <button type="button" class="btn" onClick={() => props.onShowUnsaved()}>
              Show unsaved changes
            </button>
          </div>
        )}
      </Show>
      <button type="button" class="btn btn-primary" onClick={() => app.signIn()}>
        Sign in
      </button>
    </div>
  );
}

/** Over a page kept for its unsaved text after the session ended: nothing in it can be saved now. */
export function SessionEndedBar() {
  const app = useContext(AppContext);
  return (
    <div
      role="alert"
      class="flex flex-wrap items-center justify-between gap-2 border-b border-line bg-attention-soft px-3 py-1 text-sm"
    >
      <p class="min-w-0">Your session ended. Copy what you need, then sign in again.</p>
      <button type="button" class="btn btn-primary" onClick={() => app.signIn()}>
        Sign in
      </button>
    </div>
  );
}

/** The signed-in identity and the way out. Renders nothing in local mode. */
export function AccountControls(props: { class?: string }) {
  const app = useContext(AppContext);
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  async function signOut(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setFailure(undefined);
    try {
      await app.signOut();
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show when={app.auth().status === "signed_in"}>
      <div class={["flex flex-col gap-1", props.class]}>
        <div class="flex min-w-0 items-center justify-between gap-2">
          <p class="min-w-0 truncate text-sm" title={app.identity()}>
            <span class="sr-only">Signed in as </span>
            {app.identity() || "an unnamed identity"}
          </p>
          <button
            type="button"
            class="btn shrink-0"
            disabled={busy()}
            onClick={() => void signOut()}
          >
            {busy() ? "Signing out…" : "Sign out"}
          </button>
        </div>
        <Show when={failure()}>
          {(message) => (
            <p role="alert" class="text-sm wrap-break-word text-tone-danger">
              Could not sign out: {message()}
            </p>
          )}
        </Show>
      </div>
    </Show>
  );
}
