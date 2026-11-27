import { createSignal, For, Match, onSettled, Show, Switch, useContext } from "solid-js";
import { SessionStatusBadge } from "../components/Badge";
import type { Automation } from "../gen/Automation";
import { outcomeText, requestOf } from "../lib/automations";
import { errorMessage } from "../lib/errors";
import { absoluteTime, relativeFuture, relativeTime } from "../lib/format";
import { sessionPath } from "../panels/registry";
import { AppContext } from "../state/app";

type Load =
  | { status: "loading" }
  | { status: "ready"; automations: readonly Automation[] }
  | { status: "error"; message: string };

export const automationPath = (id: string): string => `/automations/${encodeURIComponent(id)}`;

/**
 * On or off, saved at once through `PUT` with the whole automation. On a
 * refusal the switch goes back and the server's reason shows under it.
 */
export function EnabledSwitch(props: {
  automation: Automation;
  onSaved: (automation: Automation) => void;
}) {
  const app = useContext(AppContext);
  // What the switch shows while a save is in flight; the stored value otherwise.
  const [pending, setPending] = createSignal<boolean>();
  const [error, setError] = createSignal<string>();
  const on = (): boolean => pending() ?? props.automation.enabled;

  async function toggle(): Promise<void> {
    if (pending() !== undefined) return;
    const next = !props.automation.enabled;
    setPending(next);
    setError(undefined);
    try {
      props.onSaved(
        await app.saveAutomation(
          props.automation.id,
          requestOf(props.automation, { enabled: next }),
        ),
      );
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setPending(undefined);
    }
  }

  return (
    <div class="flex flex-col items-end gap-1">
      <button
        type="button"
        role="switch"
        aria-checked={on() ? "true" : "false"}
        aria-label={`${props.automation.name} enabled`}
        disabled={pending() !== undefined}
        class="flex min-h-11 min-w-11 items-center gap-2 text-sm"
        onClick={() => void toggle()}
      >
        <span
          aria-hidden="true"
          class={[
            "relative inline-block h-6 w-10 rounded-full border transition-colors motion-reduce:transition-none",
            on() ? "border-accent bg-accent" : "border-line bg-sunken",
          ]}
        >
          <span
            class={[
              "absolute top-0.5 size-4.5 rounded-full bg-surface shadow transition-[left] motion-reduce:transition-none",
              on() ? "left-[1.125rem]" : "left-0.5",
            ]}
          />
        </span>
        <span>{on() ? "On" : "Off"}</span>
      </button>
      <Show when={error()}>
        {(message) => (
          <p role="alert" class="text-right text-xs wrap-break-word text-tone-danger">
            {message()}
          </p>
        )}
      </Show>
    </div>
  );
}

/** When it fires next, or when GitHub was last asked and what came of it. */
export function NextOrPoll(props: { automation: Automation; now: number }) {
  return (
    <Switch>
      <Match when={props.automation.trigger.kind === "schedule" && props.automation.next_run_at}>
        {(next) => (
          <span>
            Next run{" "}
            <time datetime={new Date(next()).toISOString()} title={absoluteTime(next())}>
              {relativeFuture(next(), props.now)}
            </time>
          </span>
        )}
      </Match>
      <Match when={props.automation.trigger.kind === "schedule"}>
        <span>No run scheduled while it is off</span>
      </Match>
      <Match when={props.automation.last_poll_problem}>
        {(problem) => <span class="text-tone-danger">GitHub could not be asked: {problem()}</span>}
      </Match>
      <Match when={props.automation.last_poll_at}>
        {(at) => (
          <span>
            Checked{" "}
            <time datetime={new Date(at()).toISOString()} title={absoluteTime(at())}>
              {relativeTime(at(), props.now)}
            </time>
          </span>
        )}
      </Match>
      <Match when={true}>
        <span>Not checked yet</span>
      </Match>
    </Switch>
  );
}

function AutomationRow(props: {
  automation: Automation;
  now: number;
  onSaved: (automation: Automation) => void;
}) {
  return (
    <li class="flex flex-col gap-1.5 py-3" data-part="automation">
      <div class="flex items-start justify-between gap-3">
        <div class="min-w-0">
          <a
            href={automationPath(props.automation.id)}
            class="flex min-h-11 items-center font-medium text-accent underline"
          >
            <span class="truncate">{props.automation.name}</span>
          </a>
          <p class="text-sm wrap-break-word">{props.automation.trigger_text}</p>
        </div>
        <EnabledSwitch automation={props.automation} onSaved={props.onSaved} />
      </div>
      <Show when={props.automation.disabled_reason}>
        {(reason) => (
          <p class="text-sm wrap-break-word text-tone-danger">Switched off by Calin: {reason()}</p>
        )}
      </Show>
      <p class="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted">
        <NextOrPoll automation={props.automation} now={props.now} />
        <Show when={props.automation.last_run}>
          {(run) => (
            <>
              <span aria-hidden="true">·</span>
              <span>
                Last run{" "}
                <time datetime={new Date(run().at).toISOString()} title={absoluteTime(run().at)}>
                  {relativeTime(run().at, props.now)}
                </time>
                : {outcomeText(run())}
              </span>
              <Show when={run().session_id}>
                {(id) => (
                  <a
                    href={sessionPath(id())}
                    class="inline-flex min-h-11 items-center gap-1.5 text-accent underline"
                  >
                    Open its session
                    <Show when={run().session_status}>
                      {(status) => <SessionStatusBadge status={status()} />}
                    </Show>
                  </a>
                )}
              </Show>
            </>
          )}
        </Show>
      </p>
    </li>
  );
}

/** `/automations`: every automation, with its switch and what it did last. */
export default function AutomationsPage() {
  const app = useContext(AppContext);
  const [load, setLoad] = createSignal<Load>({ status: "loading" });

  function read(): void {
    setLoad((current) => (current.status === "ready" ? current : { status: "loading" }));
    app.listAutomations().then(
      (automations) => setLoad({ status: "ready", automations }),
      (error: unknown) => setLoad({ status: "error", message: errorMessage(error) }),
    );
  }
  onSettled(() => read());

  const list = (): readonly Automation[] | undefined => {
    const current = load();
    return current.status === "ready" ? current.automations : undefined;
  };
  const failure = (): string | undefined => {
    const current = load();
    return current.status === "error" ? current.message : undefined;
  };
  function saved(next: Automation): void {
    setLoad((current) =>
      current.status === "ready"
        ? {
            status: "ready",
            automations: current.automations.map((one) => (one.id === next.id ? next : one)),
          }
        : current,
    );
  }

  return (
    <section class="min-h-0 flex-1 overflow-y-auto overscroll-contain">
      <div class="mx-auto flex w-full max-w-2xl flex-col gap-4 px-3 py-3 lg:px-4 lg:py-8">
        <header class="flex items-center gap-2">
          <a href="/" aria-label="Back to sessions" class="btn border-transparent px-0 lg:hidden">
            <span aria-hidden="true">←</span>
          </a>
          <h1 class="flex-1 text-lg font-semibold">Automations</h1>
          <Show when={(list()?.length ?? 0) > 0}>
            <a href="/automations/new" class="btn btn-primary">
              New automation
            </a>
          </Show>
        </header>
        <Switch>
          <Match when={failure()}>
            {(message) => (
              <div role="alert" class="flex flex-col items-start gap-2 text-sm">
                <p class="wrap-break-word">The automations could not be loaded: {message()}</p>
                <button type="button" class="btn" onClick={() => read()}>
                  Try again
                </button>
              </div>
            )}
          </Match>
          <Match when={load().status === "loading"}>
            <p class="text-sm text-muted">Loading automations…</p>
          </Match>
          <Match when={list()?.length === 0}>
            <div class="flex flex-col items-start gap-2 text-sm" data-part="empty">
              <p class="font-medium">No automations yet</p>
              <p class="text-muted">
                An automation starts a session by itself, on a schedule or when a new issue or pull
                request appears on GitHub. It runs unattended on this server, with the repos,
                workflow and permissions you choose.
              </p>
              <a href="/automations/new" class="btn btn-primary">
                New automation
              </a>
            </div>
          </Match>
          <Match when={list()}>
            {(automations) => (
              <ul class="divide-y divide-line">
                <For each={automations()} keyed={(one) => one.id}>
                  {(automation) => (
                    <AutomationRow automation={automation()} now={app.now()} onSaved={saved} />
                  )}
                </For>
              </ul>
            )}
          </Match>
        </Switch>
      </div>
    </section>
  );
}
