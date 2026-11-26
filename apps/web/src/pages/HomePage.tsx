import { useNavigate } from "@solidjs/router";
import { Show, useContext } from "solid-js";
import { Composer } from "../components/Composer";
import { sessionPath } from "../components/SessionList";
import { AppContext } from "../state/app";

/** `/`: the composer. The sessions list beside (or above) it belongs to the shell. */
export default function HomePage() {
  const app = useContext(AppContext);
  const navigate = useNavigate();

  const runners = (): readonly string[] => {
    const health = app.health();
    return health.status === "ready" ? health.health.runners : [];
  };

  return (
    <section class="border-t border-line bg-surface p-3 lg:mx-auto lg:mt-10 lg:w-full lg:max-w-2xl lg:rounded-md lg:border lg:p-4">
      <Composer
        runners={runners()}
        disabled={app.health().status !== "ready"}
        onCreate={async (request) => {
          const session = await app.createSession(request);
          navigate(sessionPath(session.id));
        }}
      />
      {/* The socket is up but /health failed, so the shell's "cannot reach" screen does not apply. */}
      <Show when={app.health().status === "unreachable"}>
        <p role="alert" class="mt-2 flex items-center justify-between gap-2 text-sm">
          The server's settings could not be loaded, so sessions cannot be started yet.
          <button type="button" class="btn" onClick={() => app.retry()}>
            Retry
          </button>
        </p>
      </Show>
    </section>
  );
}
