import { useLocation } from "@solidjs/router";
import {
  createSignal,
  Errored,
  Loading,
  Match,
  onSettled,
  Show,
  Switch,
  useContext,
  type ParentProps,
} from "solid-js";
import { ConnectionIndicator } from "./components/ConnectionIndicator";
import { AccountControls, SessionEndedBar, SignInScreen } from "./components/SignIn";
import { SessionList } from "./components/SessionList";
import { errorMessage } from "./lib/errors";
import { sessionIdFromPath } from "./panels/registry";
import { Router } from "./router";
import { AppContext, type AppState } from "./state/app";

function Unreachable() {
  const app = useContext(AppContext);
  return (
    <div
      role="alert"
      class="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center"
    >
      <h1 class="text-lg font-semibold">Cannot reach the server</h1>
      <p class="max-w-sm text-sm text-muted">
        Calin did not answer at <span class="font-mono">{location.host}</span>. Check that{" "}
        <span class="font-mono">calin serve</span> is running. This page keeps retrying on its own.
      </p>
      <button type="button" class="btn" onClick={() => app.retry()}>
        Retry now
      </button>
    </div>
  );
}

function SessionsPanel() {
  const app = useContext(AppContext);
  const location = useLocation();
  return (
    <Switch>
      <Match when={app.sessions().length > 0}>
        <SessionList
          sessions={app.sessions()}
          now={app.now()}
          openId={sessionIdFromPath(location.pathname)}
        />
      </Match>
      <Match when={app.sessionsError()}>
        {(message) => (
          <div role="alert" class="flex flex-col items-start gap-2 p-3 text-sm">
            <p>The server refused the session list: {message()}</p>
            <button type="button" class="btn" onClick={() => app.retry()}>
              Try again
            </button>
          </div>
        )}
      </Match>
      <Match when={!app.sessionsLoaded()}>
        <p class="p-3 text-sm text-muted">Loading sessions…</p>
      </Match>
      <Match when={true}>
        <div class="p-3 text-sm">
          <p class="font-medium">No sessions yet</p>
          <p class="mt-1 text-muted">
            Describe a task in the New session box and an agent will start on it.
          </p>
        </div>
      </Match>
    </Switch>
  );
}

/**
 * The frame around every route. On a phone the sessions list is the home
 * screen and steps aside for an open session; from 1024px it is a sidebar
 * that stays beside whatever is open.
 */
function Shell(props: ParentProps) {
  const app = useContext(AppContext);
  const location = useLocation();
  const atHome = (): boolean => location.pathname === "/";
  const dryRun = (): boolean => {
    const health = app.health();
    return health.status === "ready" && health.health.dry_run;
  };
  const authorized = (): boolean => {
    const status = app.auth().status;
    return status === "local" || status === "signed_in";
  };
  const signedOut = (): boolean => app.auth().status === "signed_out";
  // Nothing loaded and no socket: there is nothing useful to show but the reason.
  const unreachable = (): boolean =>
    app.auth().status === "unreachable" ||
    (authorized() && app.health().status === "unreachable" && app.connection() !== "open");
  // A page with unsaved text outlives the session, hidden, so the text is not lost with it.
  const [showUnsaved, setShowUnsaved] = createSignal(false);
  const keepPage = (): boolean => authorized() || (signedOut() && app.unsavedWork() !== undefined);
  const pageHidden = (): boolean => !authorized() && !showUnsaved();

  return (
    // Positioned and clipped: screen-reader-only text is absolutely positioned, and
    // inside a scrolling region it would otherwise stretch the page itself.
    <div class="pt-safe px-safe pb-safe relative flex h-dvh flex-col overflow-clip bg-bg text-fg">
      <Show when={authorized()}>
        <ConnectionIndicator />
      </Show>
      <Show when={authorized() && dryRun()}>
        <p class="border-b border-line bg-attention-soft px-3 py-1 text-center text-xs">
          Dry run: agents are simulated
        </p>
      </Show>
      <Show when={!unreachable()} fallback={<Unreachable />}>
        <Show when={signedOut()}>
          <Show
            when={showUnsaved() && app.unsavedWork() !== undefined}
            fallback={<SignInScreen onShowUnsaved={() => setShowUnsaved(true)} />}
          >
            <SessionEndedBar />
          </Show>
        </Show>
        <Show when={keepPage()}>
          <div
            class="flex min-h-0 flex-1 flex-col lg:flex-row"
            hidden={pageHidden()}
            inert={pageHidden()}
          >
            <aside
              class={[
                "min-h-0 flex-1 flex-col border-line lg:flex lg:w-80 lg:flex-none lg:border-r",
                atHome() ? "flex" : "hidden",
              ]}
            >
              <header class="flex min-h-11 items-center justify-between border-b border-line px-3">
                <a href="/" class="flex min-h-11 min-w-11 items-center text-base font-semibold">
                  Calin
                </a>
                <span class="flex items-center gap-4">
                  <a href="/" class="hidden min-h-11 items-center text-sm text-accent lg:flex">
                    New session
                  </a>
                  <a
                    href="/settings"
                    class="flex min-h-11 min-w-11 items-center justify-end text-sm text-accent"
                    aria-current={location.pathname === "/settings" ? "page" : undefined}
                  >
                    Settings
                  </a>
                </span>
              </header>
              <nav aria-label="Sessions" class="min-h-0 flex-1 overflow-y-auto overscroll-contain">
                <SessionsPanel />
              </nav>
              {/* Public mode, wide screens; on a phone it is in Settings, out of the header's way. */}
              <AccountControls class="hidden border-t border-line px-3 py-2 lg:flex" />
            </aside>
            {/* At home on a phone the composer shares the screen with the list above it. */}
            <main
              class={[
                // `min-w-0`: a long unbreakable line in a session (code, a branch name) must
                // scroll or be cut where it is, not push the side pane out of the window.
                "flex min-h-0 min-w-0 flex-col lg:flex-1",
                atHome() ? "max-h-[65dvh] lg:max-h-none" : "flex-1",
              ]}
            >
              {/* A page whose code is fetched on its first visit (see router.ts) shows this meanwhile. */}
              <Loading fallback={<p class="p-3 text-sm text-muted">Loading…</p>}>
                {props.children}
              </Loading>
            </main>
          </div>
        </Show>
      </Show>
    </div>
  );
}

function Crash(props: { error: unknown }) {
  return (
    <div role="alert" class="flex h-dvh flex-col items-center justify-center gap-3 p-6 text-center">
      <h1 class="text-lg font-semibold">Something went wrong</h1>
      <p class="max-w-sm font-mono text-sm text-muted">{errorMessage(props.error)}</p>
      <button type="button" class="btn" onClick={() => location.reload()}>
        Reload
      </button>
    </div>
  );
}

export function App(props: { state: AppState }) {
  onSettled(() => props.state.start());
  return (
    <Errored fallback={(error) => <Crash error={error()} />}>
      <AppContext value={props.state}>
        <Router>{(route) => <Shell>{route.children}</Shell>}</Router>
      </AppContext>
    </Errored>
  );
}
