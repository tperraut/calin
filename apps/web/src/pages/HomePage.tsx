import { useNavigate } from "@solidjs/router";
import { onSettled, Show, useContext } from "solid-js";
import { Composer } from "../components/Composer";
import { sessionPath } from "../panels/registry";
import { AppContext } from "../state/app";

/** `/`: the composer. The sessions list beside (or above) it belongs to the shell. */
export default function HomePage() {
  const app = useContext(AppContext);
  const navigate = useNavigate();

  const runners = (): readonly string[] => {
    const health = app.health();
    return health.status === "ready" ? health.health.runners : [];
  };

  onSettled(() => {
    app.loadRepos();
    app.loadWorkflows();
  });
  return (
    <section class="relative min-h-0 overflow-y-auto overscroll-contain border-t border-line bg-surface px-3 pt-3 lg:mx-auto lg:mt-10 lg:mb-4 lg:w-full lg:max-w-2xl lg:rounded-md lg:border lg:px-4 lg:pt-4">
      <Composer
        runners={runners()}
        registry={app.repos()}
        sets={app.repoSets()}
        workflows={app.workflows()}
        disabled={app.health().status !== "ready"}
        onRegisterRepo={(request) => app.registerRepo(request)}
        onRemoveRepo={(id) => app.removeRepo(id)}
        onCloneRepo={(id) => app.cloneRepo(id)}
        onRetryRepos={() => app.loadRepos()}
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
